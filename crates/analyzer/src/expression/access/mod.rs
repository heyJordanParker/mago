use mago_allocator::Arena;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::Expression;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
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
            // Like the engine, a read is the constant or enum case when the class has one by that name, and the
            // static property otherwise. A constant expression reads only the constant or enum case, as PHP's does.
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
