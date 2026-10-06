use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::ttype::get_mixed;
use mago_names::binding::Binding;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::PropertyAccess;
use mago_word::concat_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::resolver::property::resolve_method_value;
use crate::resolver::static_property::StaticProperty;
use crate::resolver::static_property::StaticPropertyName;

pub mod class_constant_access;
pub mod property_access;
pub mod static_property_access;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Access<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        match self {
            // As after an invalid call target, the read gets no type, so nothing reports again on what it reads.
            Access::Property(PropertyAccess {
                object, property: ClassLikeMemberSelector::Identifier(name), ..
            }) if report_full_name(context, object, Some(name)) => Ok(()),
            // PHP# writes both `Class::NAME` and `Class::$name` as `Class.name`, with a bare name bound to a class.
            // Like the engine, a read is the constant or enum case when the class has one by that name, then the
            // static property, then the static method as a closure, as `Class::name(...)`. A constant expression
            // reads only the constant or enum case, as PHP's does.
            Access::Property(access) => match StaticProperty::from_property_access(access, context.resolved_names) {
                Some(StaticProperty {
                    class: class @ Expression::ConstantAccess(class_name),
                    name: StaticPropertyName::Identifier(name),
                    span,
                }) if block_context.flags.inside_constant_expression()
                    || context
                        .codebase
                        .class_constant_exists(context.resolved_names.get(&class_name.name), name.value) =>
                {
                    class_constant_access::analyze_class_constant_access(
                        context,
                        block_context,
                        artifacts,
                        class,
                        &ClassLikeConstantSelector::Identifier(*name),
                        span,
                    )
                }
                Some(StaticProperty {
                    class: Expression::ConstantAccess(class_name),
                    name: StaticPropertyName::Identifier(name),
                    span,
                }) if is_static_method_value(context, context.resolved_names.get(&class_name.name), name.value) => {
                    let class_id = word(context.resolved_names.get(&class_name.name));
                    let method_type =
                        resolve_method_value(context, block_context, artifacts, class_id, word(name.value), span)
                            .unwrap_or_else(get_mixed);
                    artifacts.set_expression_type(self, method_type);

                    Ok(())
                }
                Some(static_property) => static_property_access::analyze_static_property_access(
                    context,
                    block_context,
                    artifacts,
                    static_property,
                ),
                None => access.analyze(context, block_context, artifacts),
            },
            Access::NullSafeProperty(access) => access.analyze(context, block_context, artifacts),
            Access::StaticProperty(access) => access.analyze(context, block_context, artifacts),
            Access::ClassConstant(access) => access.analyze(context, block_context, artifacts),
        }
    }
}

/// Reports a full name in a PHP# file's code, as in `App.Shared.Money.of(1)`, which spec section 23 keeps in `import`
/// lines. The binder reads the root of a chain of member names as a class, so the chain is a full name when its root
/// names no class and the root with the names after it, read as one name, names a class. One file cannot tell
/// `App.Status` from `Status.Active`, so the analyzer makes this refusal for the checker.
///
/// `object` is the chain before `last`, its last member name, which a method call does not pass. It runs at the
/// chain's outermost member, which the analysis reaches first, and returns whether it reported, so the caller analyzes
/// nothing inside the chain.
pub(crate) fn report_full_name<A>(
    context: &mut Context<'_, '_, A>,
    object: &Expression<'_>,
    last: Option<&LocalIdentifier<'_>>,
) -> bool
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return false;
    }

    let mut names: Vec<&LocalIdentifier<'_>> = last.into_iter().collect();
    let mut object = object;
    while let Expression::Access(Access::Property(property)) = object
        && let ClassLikeMemberSelector::Identifier(name) = &property.property
    {
        names.push(name);
        object = property.object;
    }

    let Expression::ConstantAccess(root) = object else {
        return false;
    };

    if context.resolved_names.binding(&root.name) != Some(Binding::Class)
        || context.resolved_names.is_imported(&root.name)
        || context.codebase.class_like_exists(context.resolved_names.get(&root.name))
    {
        return false;
    }

    let mut full_name = root.name.value().to_vec();
    for name in names.iter().rev() {
        full_name.push(b'\\');
        full_name.extend_from_slice(name.value);
        if !context.codebase.class_like_exists(&full_name) {
            continue;
        }

        let class = BytesDisplay(name.value);
        let full_name = String::from_utf8_lossy(&full_name).replace('\\', ".");
        context.collector.report_with_code(
            IssueCode::NonExistentClassLike,
            Issue::error(format!(
                "Full names appear only in `import` lines: add `import {full_name};` and write `{class}`."
            ))
            .with_annotation(Annotation::primary(root.span().join(name.span)).with_message("Full name used here."))
            .with_help(
                "In code, `.` is always member access, so a class is written by the short name its import brings in.",
            ),
        );

        return true;
    }

    false
}

/// Whether PHP# `Class.name` reads the static method `name`: the class has no static property of that name, which the
/// engine finds first, and its method of that name is static.
fn is_static_method_value<A>(context: &Context<'_, '_, A>, class: &[u8], name: &[u8]) -> bool
where
    A: Arena,
{
    context.codebase.get_declaring_property_class(class, concat_word!("$", name).as_bytes()).is_none()
        && context
            .codebase
            .get_declaring_method(class, name)
            .and_then(|method| method.method_metadata.as_ref())
            .is_some_and(|method| method.is_static)
}
