//! The rules effects decide: an `extern` declaration's own checks, and the rules over solved effects.

use mago_allocator::Arena;
use mago_codex::metadata::CodebaseMetadata;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::HasSpan;
use mago_syntax::cst::Extern;
use mago_word::Word;

use crate::code::IssueCode;
use crate::context::Context;
use crate::effects::Body;
use crate::effects::Effect;
use crate::effects::Effects;
use crate::effects::Impurity;

/// Spec section 29: a getter reads, so it has no effect and changes nothing.
pub(crate) fn getters_must_be_pure(effects: &Effects) -> IssueCollection {
    effects
        .impure_bodies()
        .filter(|(body, _, _)| matches!(body, Body::Accessor(_, _, accessor) if accessor.as_bytes() == b"get"))
        .map(|(_, name, impurity)| {
            impure(
                IssueCode::ImpureGetter,
                format!("Getter `{}` {impurity}. Getters must be pure.", member(name)),
                &impurity,
            )
        })
        .collect()
}

/// Spec section 28: a law holds only over pure code, which is the code Lean reads.
pub(crate) fn laws_must_be_pure(effects: &Effects, codebase: &CodebaseMetadata) -> IssueCollection {
    effects
        .impure_bodies()
        .filter(|(body, _, _)| {
            matches!(body, Body::Method(class, method)
                if codebase.get_class_like(class.as_bytes()).is_some_and(|class| class.laws.contains_key(method)))
        })
        .map(|(_, name, impurity)| {
            impure(
                IssueCode::ImpureLaw,
                format!("Law `{}` {impurity}. Laws hold only over pure code.", member(name)),
                &impurity,
            )
        })
        .collect()
}

/// The member a body's message name ends with: `total` of `Cart.total`.
fn member(name: Word) -> String {
    let bytes = name.as_bytes();

    String::from_utf8_lossy(bytes.rsplit(|byte| *byte == b'.').next().unwrap_or(bytes)).into_owned()
}

/// The error on the call or write `impurity` names, with the `extern` to write when the callee has none.
fn impure(code: IssueCode, message: String, impurity: &Impurity) -> Issue {
    let issue = Issue::error(message).with_code(code.as_str()).with_annotation(Annotation::primary(impurity.span));

    match impurity.effect {
        Some(Effect::Unknown(target)) => issue.with_help(format!(
            "Declare it in a .sharp file: `extern {target};` when it has no effect, or name its effects after `uses`."
        )),
        _ => issue,
    }
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
