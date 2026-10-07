use mago_allocator::Arena;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::TypeOf;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::access::class_constant_access::analyze_class_constant_access;
use crate::resolver::class_name::report_non_existent_class_like;
use crate::utils::expression::is_variable;

/// `typeof(X)` runs as PHP's `X::class`, so it is the literal class string of the class `X`, or, when `X` is a local
/// or `this`, PHP's `$x::class`, which is analyzed as that access.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for TypeOf<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let value = Expression::ConstantAccess(ConstantAccess { name: self.class });
        if is_variable(&value, context.resolved_names) {
            let class =
                ClassLikeConstantSelector::Identifier(LocalIdentifier { span: self.r#typeof.span(), value: b"class" });

            return analyze_class_constant_access(context, block_context, artifacts, &value, &class, self.span());
        }

        let mut name = word(context.resolved_names.get(&self.class));
        match context.codebase.get_class_like(name.as_bytes()) {
            Some(metadata) => name = metadata.original_name,
            None => report_non_existent_class_like(context, self.class.span(), name),
        }

        artifacts.symbol_references.add_reference_to_symbol(&block_context.scope, name, false);
        artifacts.set_expression_type(
            self,
            TUnion::from_atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::literal(name)))),
        );

        Ok(())
    }
}
