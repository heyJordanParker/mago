//! Helpers for rendering symbol names in user-facing diagnostics.

use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
use mago_word::Word;
use mago_word::ascii_lowercase_word;

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

/// Returns the class-like `name` as the analyzed `.sharp` file names it once imported: the name the file imports it
/// under with `as`, as spec section 23 renames an import, as in `Rx`, and otherwise its short name, as in `Date`.
#[must_use]
pub(crate) fn display_sharp_class<A>(context: &Context<'_, '_, A>, name: Word) -> String
where
    A: Arena,
{
    if let Some(renamed) = context.renamed_imports.get(&ascii_lowercase_word(name.as_bytes())) {
        return renamed.to_string();
    }

    let name = name.as_bytes();
    String::from_utf8_lossy(name.rsplit(|byte| *byte == b'\\').next().unwrap_or(name)).into_owned()
}

/// Returns a method as PHP# calls it on its class, as in `Date.format`: the class as [`display_sharp_class`] names it,
/// and the method's name.
#[must_use]
pub(crate) fn display_sharp_method<A>(
    context: &Context<'_, '_, A>,
    class: &ClassLikeMetadata,
    method: &FunctionLikeMetadata,
) -> String
where
    A: Arena,
{
    format!("{}.{}", display_sharp_class(context, class.original_name), method.original_name)
}

/// Returns the PHP# collection type, as in `Map<string, int>`, that `object` stands for when it is `Sharp\ListMethods`
/// or `Sharp\MapMethods`. The analyzer checks a method call on a `List` or `Map` against those classes, and a message
/// names the type the code wrote instead.
#[must_use]
pub(crate) fn display_sharp_collection<A>(context: &Context<'_, '_, A>, object: &TObject) -> Option<String>
where
    A: Arena,
{
    let collection = sharp_collection_name(object)?;
    let parameters = object.get_type_parameters().unwrap_or_default();
    let parameters = parameters.iter().map(|parameter| display_sharp_type(context, parameter)).collect::<Vec<_>>();

    Some(format!("{collection}<{}>", parameters.join(", ")))
}

/// Returns `union` as PHP# writes the type: `List<int>`, `Map<string, int>`, `int?`, `(int|string)?`, `Any?`, and a
/// class as [`display_sharp_class`] names it.
#[must_use]
pub(crate) fn display_sharp_type<A>(context: &Context<'_, '_, A>, union: &TUnion) -> String
where
    A: Arena,
{
    if let Some(TAtomic::Mixed(mixed)) = union.types.iter().find(|atomic| atomic.is_mixed()) {
        return if mixed.is_non_null() { "Any" } else { "Any?" }.to_owned();
    }

    let parts: Vec<String> = union
        .types
        .iter()
        .filter(|atomic| !atomic.is_null())
        .map(|atomic| display_sharp_atomic(context, atomic))
        .collect();

    match (union.has_null(), parts.as_slice()) {
        (false, _) => parts.join("|"),
        (true, []) => "null".to_owned(),
        (true, [part]) => format!("{part}?"),
        (true, _) => format!("({})?", parts.join("|")),
    }
}

fn display_sharp_atomic<A>(context: &Context<'_, '_, A>, atomic: &TAtomic) -> String
where
    A: Arena,
{
    match atomic {
        TAtomic::Array(array) => {
            let (key, value) = get_array_parameters(array, context.codebase);
            match array {
                TArray::List(_) => format!("List<{}>", display_sharp_type(context, &value)),
                TArray::Keyed(_) => {
                    format!("Map<{}, {}>", display_sharp_type(context, &key), display_sharp_type(context, &value))
                }
            }
        }
        TAtomic::Object(object) => {
            let Some(name) = object.get_name() else {
                return atomic.get_id().to_string();
            };
            let name = display_sharp_class(context, name);
            match object.get_type_parameters() {
                Some(parameters) if !parameters.is_empty() => {
                    let parameters: Vec<String> =
                        parameters.iter().map(|parameter| display_sharp_type(context, parameter)).collect();
                    format!("{name}<{}>", parameters.join(", "))
                }
                _ => name,
            }
        }
        _ => atomic.get_id().to_string(),
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
