//! Helpers for rendering symbol names in user-facing diagnostics.

use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
use mago_names::display_sharp_member;
use mago_names::short_name;
use mago_word::Word;
use mago_word::word;

use crate::context::Context;

/// Returns the case-preserved name of a class-like as the analyzed file writes it: its short name in a `.sharp` file,
/// and its full name in PHP. Falls back to the input if no metadata is available.
#[inline]
pub(crate) fn display_class_like_name<A>(context: &Context<'_, '_, A>, name: Word) -> Word
where
    A: Arena,
{
    let name = context.codebase.get_class_like(name.as_bytes()).map_or(name, |m| m.original_name);

    if context.dialect.is_sharp() { word(short_name(name)) } else { name }
}

/// Returns the property `name`, which the codebase keys with its `$`, as the analyzed file names it in prose: `total`
/// in a `.sharp` file, and `$total` in PHP.
#[inline]
pub(crate) fn display_property_name<A>(context: &Context<'_, '_, A>, name: Word) -> Word
where
    A: Arena,
{
    if context.dialect.is_sharp() { word(mago_bytes::trim_start_byte(name.as_bytes(), b'$')) } else { name }
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
pub(crate) fn display_sharp_collection(object: &TObject, codebase: &CodebaseMetadata) -> Option<String> {
    let collection = sharp_collection_name(object)?;
    let parameters = object.get_type_parameters().unwrap_or_default();
    let parameters = parameters.iter().map(|parameter| display_sharp_type(parameter, codebase)).collect::<Vec<_>>();

    Some(format!("{collection}<{}>", parameters.join(", ")))
}

/// Returns `union` as the analyzed file writes types: as PHP# writes it in a `.sharp` file, and by its Mago type id in
/// PHP.
#[must_use]
pub(crate) fn display_type<A>(context: &Context<'_, '_, A>, union: &TUnion) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() { display_sharp_type(union, context.codebase) } else { union.get_id().to_string() }
}

/// Returns `union` with null added as the analyzed file writes types: as PHP# writes the nullable type in a `.sharp`
/// file, and `php` in PHP.
#[must_use]
pub(crate) fn display_nullable_type<A>(context: &Context<'_, '_, A>, union: &TUnion, php: String) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() { display_sharp_type(&union.clone().as_nullable(), context.codebase) } else { php }
}

/// Returns `union`, the type of a value a message checks against `expected`, the type it must have, as the analyzed
/// file writes types. In a `.sharp` file a literal is its general type, `string` for `"text"`, as PHP# writes no
/// literal type, unless `expected` holds literals of its kind: `"up"` stays `"up"` against `"asc"|"desc"`, as the
/// literal is what fails. In PHP it is its Mago type id.
#[must_use]
pub(crate) fn display_value_type<A>(context: &Context<'_, '_, A>, union: &TUnion, expected: &TUnion) -> String
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return union.get_id().to_string();
    }

    let literal_kind = |atomic: &TAtomic| match atomic {
        TAtomic::Scalar(scalar) if scalar.is_literal_value() => Some(std::mem::discriminant(scalar)),
        _ => None,
    };
    let general = union
        .types
        .iter()
        .flat_map(|atomic| {
            let kept = literal_kind(atomic)
                .is_some_and(|kind| expected.types.iter().any(|wanted| literal_kind(wanted) == Some(kind)));
            let mut atomic = TUnion::from_atomic(atomic.clone());
            if !kept {
                atomic.widen_literals();
            }

            atomic.types.into_owned()
        })
        .collect();

    display_sharp_type(&TUnion::from_vec(general), context.codebase)
}

/// Returns `union` as PHP# writes the type: `List<int>`, `Map<string, int>`, `int?`, `(int|string)?`, `Any?`, a class
/// by its short name, a type parameter by its name, an intersection as `A & B`, a function type as
/// `Function<void(int)>`, `Class<Order>`, `Object`, `Iterable<int>`, and a literal as `1` or `"text"`. A refinement that
/// PHP# cannot write is the type that holds it, such as `int` for a `positive-int` and `int|string` for an
/// `array-key`, and is named once. `numeric`, `scalar` and `never` have no PHP# name and keep Mago's.
#[must_use]
pub(crate) fn display_sharp_type(union: &TUnion, codebase: &CodebaseMetadata) -> String {
    if let Some(TAtomic::Mixed(mixed)) = union.types.iter().find(|atomic| atomic.is_mixed()) {
        return if mixed.is_non_null() { "Any" } else { "Any?" }.to_owned();
    }

    let mut parts: Vec<String> = Vec::new();
    for atomic in union.types.iter().filter(|atomic| !atomic.is_null()) {
        let written = match atomic {
            TAtomic::Scalar(TScalar::ArrayKey) => vec!["int".to_owned(), "string".to_owned()],
            atomic => vec![display_sharp_atomic(atomic, codebase)],
        };
        for part in written {
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
    }

    match (union.has_null(), parts.as_slice()) {
        (false, _) => parts.join("|"),
        (true, []) => "null".to_owned(),
        (true, [part]) => format!("{part}?"),
        (true, _) => format!("({})?", parts.join("|")),
    }
}

fn display_sharp_atomic(atomic: &TAtomic, codebase: &CodebaseMetadata) -> String {
    let written = match atomic {
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
        TAtomic::Object(TObject::Any) => "Object".to_owned(),
        TAtomic::Object(object) => {
            let Some(name) = object.get_name() else {
                return atomic.get_id().to_string();
            };
            let name = short_name(codebase.get_class_like(name.as_bytes()).map_or(name, |class| class.original_name));
            match object.get_type_parameters() {
                Some(parameters) if !parameters.is_empty() => {
                    let parameters: Vec<String> =
                        parameters.iter().map(|parameter| display_sharp_type(parameter, codebase)).collect();
                    format!("{name}<{}>", parameters.join(", "))
                }
                _ => name,
            }
        }
        TAtomic::GenericParameter(parameter) => parameter.parameter_name.to_string(),
        TAtomic::Callable(TCallable::Signature(signature)) => {
            let written = |union: Option<&TUnion>| {
                union.map_or_else(|| "Any?".to_owned(), |union| display_sharp_type(union, codebase))
            };
            let parameters: Vec<String> =
                signature.get_parameters().iter().map(|parameter| written(parameter.get_type_signature())).collect();

            format!("Function<{}({})>", written(signature.get_return_type()), parameters.join(", "))
        }
        TAtomic::Scalar(TScalar::ClassLikeString(class_string)) => match class_string {
            TClassLikeString::Literal { value } => {
                let class =
                    short_name(codebase.get_class_like(value.as_bytes()).map_or(*value, |class| class.original_name));
                format!("Class<{class}>")
            }
            TClassLikeString::OfType { constraint, .. } => {
                format!("Class<{}>", display_sharp_atomic(constraint, codebase))
            }
            TClassLikeString::Generic { parameter_name, .. } => format!("Class<{parameter_name}>"),
            TClassLikeString::Any { .. } => "Class<Object>".to_owned(),
        },
        TAtomic::Scalar(scalar) => {
            if let Some(value) = scalar.get_literal_int_value() {
                value.to_string()
            } else if let Some(value) = scalar.get_literal_float_value() {
                value.to_string()
            } else if let Some(value) = scalar.get_known_literal_string_value() {
                format!("\"{}\"", String::from_utf8_lossy(value))
            } else {
                match scalar {
                    TScalar::Integer(_) => "int".to_owned(),
                    TScalar::String(_) => "string".to_owned(),
                    _ => atomic.get_id().to_string(),
                }
            }
        }
        _ => atomic.get_id().to_string(),
    };

    match atomic.get_intersection_types() {
        Some(intersection_types) if !intersection_types.is_empty() => std::iter::once(written)
            .chain(intersection_types.iter().map(|intersection_type| display_sharp_atomic(intersection_type, codebase)))
            .collect::<Vec<_>>()
            .join(" & "),
        _ => written,
    }
}

/// The accessor `hook_name` of the property `property_name` of the class `class_name` as PHP# names it, `Box.total.get`,
/// as C# names an accessor in its messages.
#[must_use]
pub(crate) fn display_sharp_accessor(class_name: Word, property_name: Word, hook_name: Word) -> String {
    display_sharp_member(class_name, format_args!("{property_name}.{hook_name}"))
}

/// The member `member_name` of the class `class_name` as the analyzed file names it: `Box.put` in a `.sharp` file, and
/// `Box::put` in PHP, where a property keeps its `$`: `Box::$total`.
#[must_use]
pub(crate) fn display_member<A>(
    context: &Context<'_, '_, A>,
    class_name: Word,
    member_name: impl std::fmt::Display,
) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() {
        display_sharp_member(class_name, member_name)
    } else {
        format!("{class_name}::{member_name}")
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

/// Produces a user-facing display string for a `FunctionLikeIdentifier`: a method reads `Order::total` in PHP and
/// `Order.total` in a `.sharp` file.
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
        FunctionLikeIdentifier::Method(class_name, method_name) => display_member(
            context,
            display_class_like_name(context, *class_name),
            display_method_name(context, *class_name, *method_name),
        ),
        FunctionLikeIdentifier::Closure(name) => name.to_string(),
    }
}
