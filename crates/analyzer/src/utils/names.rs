//! Helpers for rendering symbol names in user-facing diagnostics.

use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
use mago_word::Word;

use crate::context::Context;

/// Returns the case-preserved name of a class-like for user-facing diagnostics,
/// falling back to the input if no metadata is available.
#[inline]
pub(crate) fn display_class_like_name<A>(context: &Context<'_, '_, A>, name: Word) -> Word
where
    A: Arena,
{
    context.codebase.get_class_like(name.as_bytes()).map(|m| m.original_name).unwrap_or(name)
}

/// Returns the case-preserved method name on the given class-like for
/// user-facing diagnostics. Falls back to the input if metadata is missing.
#[inline]
pub(crate) fn display_method_name<A>(context: &Context<'_, '_, A>, class_name: Word, method_name: Word) -> Word
where
    A: Arena,
{
    context.codebase.get_method(class_name.as_bytes(), method_name.as_bytes()).map_or(method_name, |m| m.original_name)
}

/// Returns the case-preserved name of a global function for user-facing
/// diagnostics. Falls back to the input if metadata is missing.
#[inline]
pub(crate) fn display_function_name<A>(context: &Context<'_, '_, A>, name: Word) -> Word
where
    A: Arena,
{
    context.codebase.get_function(name.as_bytes()).map_or(name, |m| m.original_name)
}

/// Returns the PHP# collection type, as in `Map<string, int>`, that `object` stands for when it is `Sharp\ListMethods`,
/// `Sharp\MapMethods` or `Sharp\IterableMethods`. The analyzer checks a method call on a `List`, `Map` or `Iterable`
/// against those classes, and a message names the type the code wrote instead.
#[must_use]
pub(crate) fn display_sharp_collection(object: &TObject, codebase: &CodebaseMetadata) -> Option<String> {
    let collection = sharp_collection_name(object)?;
    let parameters = object.get_type_parameters().unwrap_or_default();
    let parameters = parameters.iter().map(|parameter| display_sharp_type(parameter, codebase)).collect::<Vec<_>>();

    Some(format!("{collection}<{}>", parameters.join(", ")))
}

/// Returns `union` as PHP# writes the type: `List<int>`, `Map<string, int>`, `Iterable<int>`, `int?`, `(int|string)?`,
/// `Any?`, and a class by its short name.
#[must_use]
pub(crate) fn display_sharp_type(union: &TUnion, codebase: &CodebaseMetadata) -> String {
    if let Some(TAtomic::Mixed(mixed)) = union.types.iter().find(|atomic| atomic.is_mixed()) {
        return if mixed.is_non_null() { "Any" } else { "Any?" }.to_owned();
    }

    let parts: Vec<String> = union
        .types
        .iter()
        .filter(|atomic| !atomic.is_null())
        .map(|atomic| display_sharp_atomic(atomic, codebase))
        .collect();

    match (union.has_null(), parts.as_slice()) {
        (false, _) => parts.join("|"),
        (true, []) => "null".to_owned(),
        (true, [part]) => format!("{part}?"),
        (true, _) => format!("({})?", parts.join("|")),
    }
}

fn display_sharp_atomic(atomic: &TAtomic, codebase: &CodebaseMetadata) -> String {
    match atomic {
        TAtomic::Array(array) => {
            let (key, value) = get_array_parameters(array, codebase);
            match array {
                TArray::List(_) => format!("List<{}>", display_sharp_type(&value, codebase)),
                TArray::Keyed(_) => {
                    format!("Map<{}, {}>", display_sharp_type(&key, codebase), display_sharp_type(&value, codebase))
                }
            }
        }
        TAtomic::Iterable(iterable) => format!("Iterable<{}>", display_sharp_type(iterable.get_value_type(), codebase)),
        TAtomic::Object(object) => {
            let Some(name) = object.get_name() else {
                return atomic.get_id().to_string();
            };
            let name = String::from_utf8_lossy(name.as_bytes());
            let short_name = name.rsplit('\\').next().unwrap_or_default();
            match object.get_type_parameters() {
                Some(parameters) if !parameters.is_empty() => {
                    let parameters: Vec<String> =
                        parameters.iter().map(|parameter| display_sharp_type(parameter, codebase)).collect();
                    format!("{short_name}<{}>", parameters.join(", "))
                }
                _ => short_name.to_owned(),
            }
        }
        _ => atomic.get_id().to_string(),
    }
}

/// Returns `List`, `Map` or `Iterable` when `object` is `Sharp\ListMethods`, `Sharp\MapMethods` or
/// `Sharp\IterableMethods`.
#[must_use]
pub(crate) fn sharp_collection_name(object: &TObject) -> Option<&'static str> {
    let name = object.get_name()?;

    if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\ListMethods") {
        Some("List")
    } else if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\MapMethods") {
        Some("Map")
    } else if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\IterableMethods") {
        Some("Iterable")
    } else {
        None
    }
}

/// Produces a user-facing display string for a `FunctionLikeIdentifier`.
#[must_use]
pub(crate) fn display_function_like_identifier<A>(
    context: &Context<'_, '_, A>,
    identifier: &FunctionLikeIdentifier,
) -> String
where
    A: Arena,
{
    match identifier {
        FunctionLikeIdentifier::Function(name) => display_function_name(context, *name).to_string(),
        FunctionLikeIdentifier::Method(class_name, method_name) => {
            let class_display = display_class_like_name(context, *class_name);
            let method_display = display_method_name(context, *class_name, *method_name);
            format!("{class_display}::{method_display}")
        }
        FunctionLikeIdentifier::Closure(name) => name.to_string(),
    }
}
