use mago_allocator::Arena;
use mago_codex::ttype::get_mixed;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::Expression;
use mago_word::concat_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
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
