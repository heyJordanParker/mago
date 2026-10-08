//! The rules effects decide: an `extern` declaration's own checks, and the rules over solved effects.

use mago_allocator::Arena;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::HasSpan;
use mago_syntax::cst::Extern;

use crate::code::IssueCode;
use crate::context::Context;
use crate::effects::Body;
use crate::effects::Effect;
use crate::effects::Effects;

/// Spec section 29: a getter reads, so it has no effect and changes nothing.
pub(crate) fn getters_must_be_pure(effects: &Effects) -> IssueCollection {
    effects
        .impure_bodies()
        .filter(|(body, _, _)| matches!(body, Body::Accessor(_, _, accessor) if accessor.as_bytes() == b"get"))
        .map(|(_, name, impurity)| {
            let getter = name.as_bytes().rsplit(|byte| *byte == b'.').next().unwrap_or(name.as_bytes());
            let issue = Issue::error(format!(
                "Getter `{}` {impurity}. Getters must be pure (section 29).",
                String::from_utf8_lossy(getter)
            ))
            .with_code(IssueCode::ImpureGetter.as_str())
            .with_annotation(Annotation::primary(impurity.span));

            match impurity.effect {
                Some(Effect::Unknown(_)) => issue.with_help(format!(
                    "Declare its effect in a .sharp file, such as `extern {} uses Environment;`.",
                    impurity.cause
                )),
                _ => issue,
            }
        })
        .collect()
}

/// Spec section 29: an `extern` declaration declares plain PHP that exists, once in the whole project.
pub(crate) fn check_extern<A>(context: &mut Context<'_, '_, A>, r#extern: &Extern<'_>)
where
    A: Arena,
{
    let span = r#extern.span();
    let Some((declarations, own)) = context.codebase.externs.values().find_map(|declarations| {
        declarations.iter().find(|declaration| declaration.span == span).map(|own| (declarations, own))
    }) else {
        return;
    };

    let written = String::from_utf8_lossy(r#extern.target.value());
    if let Some(first) = declarations.first()
        && first.span != span
    {
        context.collector.report_with_code(
            IssueCode::DuplicateExtern,
            Issue::error(format!("`{written}` already has an `extern` declaration."))
                .with_annotation(Annotation::primary(span))
                .with_annotation(Annotation::secondary(first.span).with_message("First declared here.")),
        );

        return;
    }

    let target = r#extern.target.span();
    let (class, member) = own.target;
    if class.is_empty() {
        if context.codebase.get_function(member.as_bytes()).is_none() {
            context.collector.report_with_code(
                IssueCode::NonExistentFunction,
                Issue::error(format!("Function `{written}` does not exist."))
                    .with_annotation(Annotation::primary(target)),
            );
        }

        return;
    }

    let Some(class_metadata) = context.codebase.get_class_like(class.as_bytes()) else {
        context.collector.report_with_code(
            IssueCode::NonExistentClassLike,
            Issue::error(format!("Cannot find class, interface, enum, or type alias `{written}`."))
                .with_annotation(Annotation::primary(target)),
        );

        return;
    };

    if class_metadata.flags.is_sharp() {
        context.collector.report_with_code(
            IssueCode::ExternOnSharp,
            Issue::error(format!("`{written}` is PHP# code, so it needs no `extern`: PHP# infers its effects."))
                .with_annotation(Annotation::primary(target)),
        );
    } else if !member.is_empty() && context.codebase.get_declaring_method(class.as_bytes(), member.as_bytes()).is_none()
    {
        context.collector.report_with_code(
            IssueCode::NonExistentMethod,
            Issue::error(format!("Method `{written}` does not exist.")).with_annotation(Annotation::primary(target)),
        );
    }
}
