//! Helpers for rendering symbol names in user-facing diagnostics.

use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
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
use mago_names::kind::NameKind;
use mago_names::short_name;
use mago_syntax_core::utils::is_part_of_identifier;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::context::Context;

/// Returns the case-preserved name of a class-like as the analyzed file writes it: as [`sharp_class_like_name`] names
/// it in a `.sharp` file, and its full name in PHP. Falls back to the input if no metadata is available.
#[inline]
pub(crate) fn display_class_like_name<A>(context: &Context<'_, '_, A>, name: Word) -> Word
where
    A: Arena,
{
    if context.dialect.is_sharp() {
        word(sharp_class_like_name(context, name))
    } else {
        context.codebase.get_class_like(name.as_bytes()).map_or(name, |m| m.original_name)
    }
}

/// Returns the case-preserved name of the class-like `name` as a `.sharp` file names it: the name the file's import
/// gives it, as in `Rx` for `import Sharp.Text.Regex as Rx;` and `Order` for `import App.Orders.Order;`. A class the
/// file doesn't import is named by its short name, or, when another class-like of the codebase has the same short name,
/// by its full name with `.` between its parts, so `Billing.Order` and `App.Orders.Order` stay apart.
fn sharp_class_like_name<A>(context: &Context<'_, '_, A>, name: Word) -> String
where
    A: Arena,
{
    let name = context.codebase.get_class_like(name.as_bytes()).map_or(name, |m| m.original_name);
    let full_name = mago_bytes::trim_start_byte(name.as_bytes(), b'\\');

    if let Some(imported_name) = context.imported_names.get(&ascii_lowercase_word(full_name)) {
        imported_name.to_string()
    } else if context.shares_short_name(name) {
        String::from_utf8_lossy(full_name).replace('\\', ".")
    } else {
        short_name(name)
    }
}

/// Returns the sentence that names the `import` lines a `.sharp` file needs before code that names `class_names` by
/// their short names compiles, as `Add `import Sharp.Text.Regex;` to the file.`: one line for each class-like the file
/// doesn't [bind](binds_class_like), once each, in the order given. Returns `None` when the file binds them all, and in
/// a PHP file.
pub(crate) fn display_missing_imports<A>(
    context: &Context<'_, '_, A>,
    class_names: impl IntoIterator<Item = Word>,
) -> Option<String>
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return None;
    }

    let mut imports: Vec<String> = Vec::new();
    for class_name in class_names {
        let name = context.codebase.get_class_like(class_name.as_bytes()).map_or(class_name, |m| m.original_name);
        let import = format!(
            "`import {};`",
            String::from_utf8_lossy(mago_bytes::trim_start_byte(name.as_bytes(), b'\\')).replace('\\', ".")
        );
        if !binds_class_like(context, name) && !imports.contains(&import) {
            imports.push(import);
        }
    }

    let (last, others) = imports.split_last()?;
    let imports = if others.is_empty() { last.clone() } else { format!("{} and {last}", others.join(", ")) };

    Some(format!("Add {imports} to the file."))
}

/// Whether the analyzed file binds a name to the class-like `name`, as spec section 23 binds a name: by an import, under
/// its short name or the name after `as`, by declaring it, or by sharing its namespace. A class-like directly in `Sharp`
/// is imported by default, unless the file imports or declares another class-like of its short name.
fn binds_class_like<A>(context: &Context<'_, '_, A>, name: Word) -> bool
where
    A: Arena,
{
    let name = mago_bytes::trim_start_byte(name.as_bytes(), b'\\');
    if context.imported_names.contains_key(&ascii_lowercase_word(name)) {
        return true;
    }

    let (bound, imported) = context.scope.resolve(NameKind::Default, short_name(name));
    if bound.eq_ignore_ascii_case(name) {
        return true;
    }

    !imported
        && name.iter().rposition(|byte| *byte == b'\\').is_some_and(|end| name[..end].eq_ignore_ascii_case(b"Sharp"))
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

/// Returns the variable `id` as the analyzed file writes it. The analyzer keys a variable, a parameter and a read
/// through one by its PHP text, as `$order`, `$this->total` and `App\Shop\Order::$count`. A `.sharp` file writes them
/// as `order`, `this.total` and `Order.count`: a property is named without `$`, and the class before `::` as
/// [`sharp_class_like_name`] names it. PHP gets `id` unchanged. Text inside quotes, as the key of `$prices['$']`, is
/// kept as written.
#[must_use]
pub(crate) fn display_variable_name<A>(context: &Context<'_, '_, A>, id: impl AsRef<[u8]>) -> String
where
    A: Arena,
{
    let id = id.as_ref();
    if !context.dialect.is_sharp() {
        return String::from_utf8_lossy(id).into_owned();
    }

    let mut written = Vec::with_capacity(id.len());
    let mut quote = None;
    let mut index = 0;
    while let Some(&byte) = id.get(index) {
        let rest = &id[index..];
        let (text, length): (&[u8], usize) = match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }

                (&rest[..1], 1)
            }
            None if byte == b'\'' || byte == b'"' => {
                quote = Some(byte);

                (&rest[..1], 1)
            }
            None if byte == b'$' => (b"", 1),
            None if rest.starts_with(b"?->") => (b"?.", 3),
            None if rest.starts_with(b"::") => {
                let class_start = id[..index]
                    .iter()
                    .rposition(|byte| !is_part_of_identifier(byte) && *byte != b'\\')
                    .map_or(0, |end| end + 1);
                if class_start < index
                    && !matches!(class_start.checked_sub(1).map(|before| id[before]), Some(b'$' | b'>'))
                {
                    written.truncate(written.len() - (index - class_start));
                    written.extend_from_slice(sharp_class_like_name(context, word(&id[class_start..index])).as_bytes());
                }

                (b".", 2)
            }
            None if rest.starts_with(b"->") => (b".", 2),
            None => (&rest[..1], 1),
        };

        written.extend_from_slice(text);
        index += length;
    }

    String::from_utf8_lossy(&written).into_owned()
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
pub(crate) fn display_sharp_collection<A>(context: &Context<'_, '_, A>, object: &TObject) -> Option<String>
where
    A: Arena,
{
    let collection = sharp_collection_name(object)?;
    let parameters = object.get_type_parameters().unwrap_or_default();
    let parameters = parameters.iter().map(|parameter| display_sharp_type(context, parameter)).collect::<Vec<_>>();

    Some(format!("{collection}<{}>", parameters.join(", ")))
}

/// Returns `union` as the analyzed file writes types: as PHP# writes it in a `.sharp` file, and by its Mago type id in
/// PHP.
#[must_use]
pub(crate) fn display_type<A>(context: &Context<'_, '_, A>, union: &TUnion) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() { display_sharp_type(context, union) } else { union.get_id().to_string() }
}

/// Returns the one type `atomic` as the analyzed file writes types: as PHP# writes it in a `.sharp` file, and by its
/// Mago type id in PHP. A union of the one type would wrap a template type's id in parentheses, as it does inside a
/// union of several types.
#[must_use]
pub(crate) fn display_atomic<A>(context: &Context<'_, '_, A>, atomic: &TAtomic) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() {
        display_sharp_type(context, &TUnion::from_atomic(atomic.clone()))
    } else {
        atomic.get_id().to_string()
    }
}

/// Returns `union` with null added as the analyzed file writes types: as PHP# writes the nullable type in a `.sharp`
/// file, and `php` in PHP.
#[must_use]
pub(crate) fn display_nullable_type<A>(context: &Context<'_, '_, A>, union: &TUnion, php: String) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() { display_sharp_type(context, &union.clone().as_nullable()) } else { php }
}

/// Returns what a condition is when it holds, or when it does not, as the analyzed file says it. A PHP# condition is a
/// `bool`, so it is `true` or `false`. PHP tests truthiness, so it is truthy or falsy.
#[must_use]
pub(crate) const fn display_truth<A>(context: &Context<'_, '_, A>, holds: bool) -> &'static str
where
    A: Arena,
{
    match (context.dialect.is_sharp(), holds) {
        (true, _) => display_bool(context, holds),
        (false, true) => "truthy",
        (false, false) => "falsy",
    }
}

/// Returns the `bool` value `value` as the analyzed file writes it in prose: `` `true` `` or `` `false` `` in a
/// `.sharp` file, as PHP# writes its `bool` literals, and true or false in PHP.
#[must_use]
pub(crate) const fn display_bool<A>(context: &Context<'_, '_, A>, value: bool) -> &'static str
where
    A: Arena,
{
    match (context.dialect.is_sharp(), value) {
        (true, true) => "`true`",
        (true, false) => "`false`",
        (false, true) => "true",
        (false, false) => "false",
    }
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

    display_sharp_type(context, &TUnion::from_vec(general))
}

/// Returns `union` as PHP# writes the type: `List<int>`, `Map<string, int>`, `int?`, `(int|string)?`, `Any?`, a class
/// as [`sharp_class_like_name`] names it, a type parameter by its name, an intersection as `A & B`, a function type as
/// `Function<void(int)>`, `Class<Order>`, `Object`, `Iterable<int>`, and a literal as `1` or `"text"`. A refinement that
/// PHP# cannot write is the type that holds it, such as `int` for a `positive-int` and `int|string` for an
/// `array-key`, and is named once. `numeric`, `scalar` and `never` have no PHP# name and keep Mago's.
#[must_use]
pub(crate) fn display_sharp_type<A>(context: &Context<'_, '_, A>, union: &TUnion) -> String
where
    A: Arena,
{
    if let Some(TAtomic::Mixed(mixed)) = union.types.iter().find(|atomic| atomic.is_mixed()) {
        return if mixed.is_non_null() { "Any" } else { "Any?" }.to_owned();
    }

    let mut parts: Vec<String> = Vec::new();
    for atomic in union.types.iter().filter(|atomic| !atomic.is_null()) {
        let written = match atomic {
            TAtomic::Scalar(TScalar::ArrayKey) => vec!["int".to_owned(), "string".to_owned()],
            atomic => vec![display_sharp_atomic(context, atomic)],
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

fn display_sharp_atomic<A>(context: &Context<'_, '_, A>, atomic: &TAtomic) -> String
where
    A: Arena,
{
    let written = match atomic {
        TAtomic::Array(array) => {
            let (key, value) = get_array_parameters(array, context.codebase);
            match array {
                TArray::List(_) => format!("List<{}>", display_sharp_type(context, &value)),
                TArray::Keyed(_) => {
                    format!("Map<{}, {}>", display_sharp_type(context, &key), display_sharp_type(context, &value))
                }
            }
        }
        TAtomic::Iterable(iterable) => format!("Iterable<{}>", display_sharp_type(context, iterable.get_value_type())),
        TAtomic::Object(TObject::Any) => "Object".to_owned(),
        TAtomic::Object(object) => {
            let Some(name) = object.get_name() else {
                return atomic.get_id().to_string();
            };
            let name = sharp_class_like_name(context, name);
            match object.get_type_parameters() {
                Some(parameters) if !parameters.is_empty() => {
                    let parameters: Vec<String> =
                        parameters.iter().map(|parameter| display_sharp_type(context, parameter)).collect();
                    format!("{name}<{}>", parameters.join(", "))
                }
                _ => name,
            }
        }
        TAtomic::GenericParameter(parameter) => parameter.parameter_name.to_string(),
        TAtomic::Callable(TCallable::Signature(signature)) => {
            let written = |union: Option<&TUnion>| {
                union.map_or_else(|| "Any?".to_owned(), |union| display_sharp_type(context, union))
            };
            let parameters: Vec<String> =
                signature.get_parameters().iter().map(|parameter| written(parameter.get_type_signature())).collect();

            format!("Function<{}({})>", written(signature.get_return_type()), parameters.join(", "))
        }
        TAtomic::Scalar(TScalar::ClassLikeString(class_string)) => match class_string {
            TClassLikeString::Literal { value } => format!("Class<{}>", sharp_class_like_name(context, *value)),
            TClassLikeString::OfType { constraint, .. } => {
                format!("Class<{}>", display_sharp_atomic(context, constraint))
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
            .chain(intersection_types.iter().map(|intersection_type| display_sharp_atomic(context, intersection_type)))
            .collect::<Vec<_>>()
            .join(" & "),
        _ => written,
    }
}

/// The accessor `hook_name` of the property `property_name` of the class `class_name` as PHP# names it, `Box.total.get`,
/// as C# names an accessor in its messages.
#[must_use]
pub(crate) fn display_sharp_accessor<A>(
    context: &Context<'_, '_, A>,
    class_name: Word,
    property_name: Word,
    hook_name: Word,
) -> String
where
    A: Arena,
{
    display_sharp_member(sharp_class_like_name(context, class_name), format_args!("{property_name}.{hook_name}"))
}

/// The member `member_name` of the class `class_name` as the analyzed file names it: `Box.put` in a `.sharp` file, with
/// the class named as [`sharp_class_like_name`] names it, and `Box::put` in PHP, where a property keeps its `$`:
/// `Box::$total`.
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
        display_sharp_member(sharp_class_like_name(context, class_name), member_name)
    } else {
        format!("{class_name}::{member_name}")
    }
}

/// The member `member_name` of the class `class_name` as code the analyzed file writes: `Status.cases()` in a `.sharp`
/// file, by the name an import binds, and `Status::cases()` in PHP. The class is named as [`sharp_class_like_name`]
/// names it, cut to its last part: PHP# refuses a full name in code (spec section 23), so the dotted name that tells two
/// classes of one short name apart in prose would not compile here.
#[must_use]
pub(crate) fn display_code_member<A>(
    context: &Context<'_, '_, A>,
    class_name: Word,
    member_name: impl std::fmt::Display,
) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() {
        let class_name = sharp_class_like_name(context, class_name);

        display_sharp_member(class_name.rsplit('.').next().unwrap_or_default(), member_name)
    } else {
        display_member(context, class_name, member_name)
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
