use mago_allocator::Arena;
use mago_codex::context::ScopeContext;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::concat_word;
use mago_word::word;

use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::union::TUnion;

use mago_names::binding::php_method_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::effects;
use crate::effects::Body;
use crate::effects::short_name;
use crate::error::AnalysisError;
use crate::statement::attributes::AttributeTarget;
use crate::statement::attributes::analyze_attributes;
use crate::statement::function_like::FunctionLikeBody;
use crate::statement::function_like::analyze_function_like;
use crate::statement::function_like::check_unused_function_template_parameters;
use crate::statement::function_like::rejected_nullable_parameter;
use crate::statement::function_like::unused_parameter;
use crate::utils::missing_type_hints;
use crate::utils::names::display_sharp_method;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Method<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let name = php_method_name(self);

        analyze_attributes(
            context,
            block_context,
            artifacts,
            self.attribute_lists.as_slice(),
            AttributeTarget::Method,
        )?;

        let Some(class_like_metadata) = block_context.scope.get_class_like() else {
            tracing::error!(
                "Attempted to analyze method `{}` without class-like context.",
                mago_bytes::BytesDisplay(name)
            );

            return Ok(());
        };

        let method_name = word(name);
        let lowercase_method_name = ascii_lowercase_word(name);
        if context.settings.diff
            && context.codebase.safe_symbol_members.contains(&(class_like_metadata.name, lowercase_method_name))
        {
            return Ok(());
        }

        let Some(method_metadata) =
            context.codebase.get_method_by_id(&MethodIdentifier::new(class_like_metadata.name, lowercase_method_name))
        else {
            tracing::error!(
                "Failed to find method metadata for `{}` in class `{}`.",
                mago_bytes::BytesDisplay(name),
                class_like_metadata.original_name
            );

            return Ok(());
        };

        // Skip duplicate methods; semantics reports the error
        if method_metadata.span != self.span() {
            return Ok(());
        }

        // Spec section 29 keeps native bodies to the standard library's classes under `Sharp\`. The engine compiles the
        // library from `vendor/` as any other code, so only the analyzer knows the file's package.
        if self.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_)))
            && !(context.source_file.is_standard_library && class_like_metadata.name.as_bytes().starts_with(b"sharp\\"))
        {
            context.collector.report_with_code(
                IssueCode::NativeBodyOutsideLibrary,
                Issue::error(format!(
                    "Only the standard library declares native bodies: give `{}` a body.",
                    mago_bytes::BytesDisplay(self.name.value)
                ))
                .with_annotation(Annotation::primary(self.name.span).with_message("Declared `extern` here."))
                .with_note("A native body is compiled into the PHP# engine, which ships only the standard library's."),
            );
        }

        check_replaced_functions(context, class_like_metadata, method_metadata);

        let body = match &self.body {
            MethodBody::Abstract(_) => None,
            MethodBody::Concrete(block) => {
                Some(FunctionLikeBody::Statements(block.statements.as_slice(), block.span()))
            }
            MethodBody::Expression(body) if self.returns_value() => Some(FunctionLikeBody::Expression(body.expression)),
            MethodBody::Expression(body) => Some(FunctionLikeBody::ExpressionStatement(body.expression)),
        };

        if let Some(body) = body {
            let mut scope =
                ScopeContext::new(ReferenceOrigin::Symbol((class_like_metadata.name, method_metadata.name)));
            scope.set_class_like(Some(class_like_metadata));
            scope.set_function_like(Some(method_metadata));
            scope.set_static(self.is_static());

            let mut method_block_context = BlockContext::new(scope, context.settings.register_super_globals);

            method_block_context.flags.set_collect_initializations(true);

            let method_artifacts = analyze_function_like(
                context,
                artifacts,
                &mut method_block_context,
                method_metadata,
                &self.parameter_list,
                body,
                None,
            )?;

            let method_key = (class_like_metadata.name, lowercase_method_name);

            if context.dialect.is_sharp() {
                effects::summary::record(
                    context,
                    artifacts,
                    Body::Method(class_like_metadata.name, lowercase_method_name),
                    concat_word!(short_name(class_like_metadata.original_name), ".", self.name.value),
                    self.parameter_list.parameters.iter().map(|parameter| parameter.variable.span).collect(),
                    body,
                );
            }

            if method_metadata.return_from_body.is_some() {
                let mut returned: Option<TUnion> = None;
                for inferred in method_artifacts.inferred_return_types {
                    returned = Some(add_optional_union_type((*inferred).clone(), returned.as_ref(), context.codebase));
                }

                if let Some(returned) = returned {
                    artifacts.body_returns.insert(method_key, returned);
                }
            }

            artifacts
                .method_initialized_properties
                .insert(method_key, method_block_context.definitely_initialized_properties.clone());

            artifacts
                .method_calls_this_methods
                .insert(method_key, method_block_context.definitely_called_methods.clone());

            if method_block_context.flags.calls_parent_constructor() {
                artifacts.method_calls_parent_constructor.insert(method_key, true);
            }

            if let Some(parent_initializer_name) = method_block_context.calls_parent_initializer {
                artifacts.method_calls_parent_initializer.insert(method_key, parent_initializer_name);
            }

            let is_overriding =
                context.codebase.method_is_overriding(class_like_metadata.name.as_bytes(), method_name.as_bytes());

            // An override keeps the parameter types of the method it overrides, so only a method that declares its
            // own parameters can drop a `?`.
            if context.dialect.is_sharp() && !is_overriding {
                rejected_nullable_parameter::check_rejected_nullable_parameters(
                    context,
                    method_metadata,
                    self.parameter_list.parameters.as_slice(),
                    body,
                    &method_block_context,
                    artifacts,
                );
            }

            if context.settings.find_unused_parameters && !is_overriding {
                unused_parameter::check_unused_params(
                    method_metadata,
                    self.parameter_list.parameters.as_slice(),
                    body,
                    context,
                );
            }
        }

        check_unused_function_template_parameters(
            context,
            method_metadata,
            self.name.span(),
            "method",
            concat_word!(&class_like_metadata.original_name, "::", &method_name),
        );

        // Check for missing type hints
        for (i, parameter) in self.parameter_list.parameters.iter().enumerate() {
            missing_type_hints::check_parameter_type_hint(
                context,
                Some(class_like_metadata),
                method_metadata,
                parameter,
            );

            missing_type_hints::check_imprecise_parameter_type_hint(context, method_metadata, parameter, i);
        }

        missing_type_hints::check_return_type_hint(
            context,
            Some(class_like_metadata),
            method_metadata,
            name,
            self.return_type_hint.as_ref(),
            self.span(),
        );

        missing_type_hints::check_imprecise_return_type_hint(
            context,
            method_metadata,
            name,
            self.return_type_hint.as_ref(),
        );

        Ok(())
    }
}

/// Decision 040: only the standard library's `[Replaces]` wraps a PHP function, and a library method names each
/// function once.
fn check_replaced_functions<A>(
    context: &mut Context<'_, '_, A>,
    class: &ClassLikeMetadata,
    method: &FunctionLikeMetadata,
) where
    A: Arena,
{
    if !context.source_file.is_standard_library {
        for attribute in method
            .attributes
            .iter()
            .filter(|attribute| attribute.name.as_bytes().eq_ignore_ascii_case(b"Sharp\\Replaces"))
        {
            context.collector.report_with_code(
                IssueCode::ReplacesOutsideLibrary,
                Issue::error(format!(
                    "Only the standard library declares which PHP functions it wraps: remove `[Replaces]` from `{}`.",
                    method.original_name
                ))
                .with_annotation(Annotation::primary(attribute.span).with_message("Written here.")),
            );
        }

        return;
    }

    let mut named: Vec<(Word, Span)> = Vec::new();
    for (function, span) in method.replaced_functions() {
        let first = named
            .iter()
            .find(|(earlier, _)| earlier.as_bytes().eq_ignore_ascii_case(function.as_bytes()))
            .map(|(_, first)| *first);
        let Some(first) = first else {
            named.push((function, span));
            continue;
        };

        let method_name = display_sharp_method(context, class, method);
        context.collector.report_with_code(
            IssueCode::DuplicateDefinition,
            Issue::error(format!("`{method_name}` names `{function}` twice: name each function once."))
                .with_annotation(Annotation::primary(span).with_message("Named again here."))
                .with_annotation(Annotation::secondary(first).with_message("First named here.")),
        );
    }
}
