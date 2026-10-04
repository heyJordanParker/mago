use mago_allocator::Arena;
use mago_span::HasSpan;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::Identifier;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::statement::r#loop;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for For<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        // A PHP# counter runs as the PHP assignment of its value to the variable it declares.
        let mut initializations = Vec::with_capacity(self.initializations.len() + 1);
        if let Some(declaration) = &self.declaration {
            let name = context
                .arena
                .alloc(Expression::ConstantAccess(ConstantAccess { name: Identifier::Local(declaration.name) }));

            initializations.push(&*context.arena.alloc(Expression::Assignment(Assignment {
                lhs: name,
                operator: AssignmentOperator::Assign(declaration.equals),
                rhs: declaration.value,
            })));
        }
        initializations.extend(self.initializations.iter().copied());

        let infinite_loop = initializations.is_empty() && self.conditions.is_empty() && self.increments.is_empty();

        r#loop::analyze_for_or_while_loop(
            context,
            block_context,
            artifacts,
            &initializations,
            self.conditions.as_slice(),
            self.increments.as_slice(),
            self.body.statements(),
            self.span(),
            infinite_loop,
        )
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use crate::test_analysis;

    test_analysis! {
        name = for_loop_is_entered_al_least_once,
        code = indoc! {"
            <?php

            /**
             * @template T
             *
             * @param int<1, max> $size
             * @param (Closure(int): T) $factory
             *
             * @return non-empty-list<T>
             */
            function reproduce(int $size, Closure $factory): array {
                $result = [];
                for ($i = 1; $i <= $size; $i++) {
                    $result[] = $factory($i);
                }

                return $result;
            }
        "},
    }
}
