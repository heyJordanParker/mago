use mago_allocator::Arena;
use mago_codex::ttype::TType;
use mago_codex::ttype::get_int_or_string;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::DieConstruct;
use mago_syntax::cst::ExitConstruct;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Literal;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::common::construct::ConstructInput;
use crate::common::construct::analyze_construct_inputs;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::context::scope::control_action::ControlAction;
use crate::error::AnalysisError;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for ExitConstruct<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let is_sharp = context.dialect.is_sharp();

        analyze_construct_inputs(
            context,
            block_context,
            artifacts,
            "exit",
            self.exit.span,
            ConstructInput::ArgumentList(self.arguments.as_ref()),
            &if is_sharp { get_mixed() } else { get_int_or_string() },
            true,
            true,
            true,
        )?;

        if is_sharp && let Some(argument) = self.arguments.as_ref().and_then(|arguments| arguments.arguments.first()) {
            let mut value = argument.value();
            while let Expression::Parenthesized(parenthesized) = value {
                value = parenthesized.expression;
            }

            if !matches!(value, Expression::Literal(Literal::String(_)) | Expression::CompositeString(_))
                && let Some(argument_type) = artifacts.get_expression_type(argument.value())
                && !argument_type.is_int()
                && !argument_type.is_never()
            {
                let issue = if argument_type.has_string() || argument_type.has_mixed() {
                    Issue::error("PHP# has no `die`: write the message to STDERR, then `exit(1)`.")
                        .with_annotation(
                            Annotation::primary(self.span())
                                .with_message(format!("This is `{}`, not an `int`.", argument_type.get_id())),
                        )
                        .with_note(
                            "`die(\"…\")` and `exit(\"…\")` print the message and exit with status 0, which reports success.",
                        )
                } else {
                    Issue::error(format!("`exit` takes an `int` status: this is `{}`.", argument_type.get_id()))
                        .with_annotation(Annotation::primary(self.span()).with_message("This status is not an `int`."))
                };

                context.collector.report_with_code(IssueCode::InvalidArgument, issue);
            }
        }

        block_context.flags.set_has_returned(true);
        block_context.control_actions.insert(ControlAction::End);

        artifacts.set_expression_type(self, get_never());

        Ok(())
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for DieConstruct<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_construct_inputs(
            context,
            block_context,
            artifacts,
            "die",
            self.die.span,
            ConstructInput::ArgumentList(self.arguments.as_ref()),
            &get_int_or_string(),
            true,
            true,
            true,
        )?;

        block_context.flags.set_has_returned(true);
        block_context.control_actions.insert(ControlAction::End);

        artifacts.set_expression_type(self, get_never());

        Ok(())
    }
}
