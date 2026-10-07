//! Helpers for rendering symbol names in user-facing diagnostics.

use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
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

/// Returns the PHP# collection type, as in `Map<string, int>`, that `object` stands for when it is `Sharp\ListMethods`
/// or `Sharp\MapMethods`. The analyzer checks a method call on a `List` or `Map` against those classes, and a message
/// names the type the code wrote instead.
#[must_use]
pub(crate) fn display_sharp_collection(object: &TObject) -> Option<String> {
    let collection = sharp_collection_name(object)?;
    let parameters = object.get_type_parameters().unwrap_or_default();
    let parameters = parameters.iter().map(|parameter| parameter.get_id().to_string()).collect::<Vec<_>>();

    Some(format!("{collection}<{}>", parameters.join(", ")))
}

/// Returns `atomic` as PHP# writes it, as in `PaginatedList<Order>`: a class by its short name, PHP's lists and maps as
/// `List<T>` and `Map<TKey, TValue>`, `T?` for a type that may be `null`, and every other type by its id.
#[must_use]
pub(crate) fn display_sharp_type(atomic: &TAtomic) -> String {
    match atomic {
        TAtomic::Object(TObject::Named(object)) => {
            let name = object.name.as_str_lossy();
            let name = name.rsplit('\\').next().unwrap_or_default();

            match object.get_type_parameters() {
                Some(arguments) if !arguments.is_empty() => {
                    format!("{name}<{}>", arguments.iter().map(display_sharp_union).collect::<Vec<_>>().join(", "))
                }
                _ => name.to_string(),
            }
        }
        TAtomic::Array(TArray::List(list)) => format!("List<{}>", display_sharp_union(&list.element_type)),
        TAtomic::Array(TArray::Keyed(keyed)) => match keyed.get_generic_parameters() {
            Some((key, value)) => format!("Map<{}, {}>", display_sharp_union(key), display_sharp_union(value)),
            None => atomic.get_id().to_string(),
        },
        _ => atomic.get_id().to_string(),
    }
}

fn display_sharp_union(union: &TUnion) -> String {
    match union.types.as_ref() {
        [TAtomic::Null, atomic] | [atomic, TAtomic::Null] => format!("{}?", display_sharp_type(atomic)),
        atomics => atomics.iter().map(display_sharp_type).collect::<Vec<_>>().join("|"),
    }
}

/// Returns `List` or `Map` when `object` is `Sharp\ListMethods` or `Sharp\MapMethods`.
#[must_use]
pub(crate) fn sharp_collection_name(object: &TObject) -> Option<&'static str> {
    let name = object.get_name()?;

    if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\ListMethods") {
        Some("List")
    } else if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\MapMethods") {
        Some("Map")
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
