use std::collections::HashSet;

use foldhash::HashMap;
use mago_span::Span;

use mago_span::HasPosition;
use mago_span::Position;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::PropertyAccess;

use crate::binding::Binding;
use crate::binding::BindingError;

pub mod binding;
pub mod kind;
pub mod resolver;
pub mod scope;

mod internal;

/// Stores the results of a name resolution pass over a PHP program.
///
/// Maps the start byte offset of every identifier in the source to a tuple of
/// `(end offset, resolved fully qualified name, was-imported flag)`. Storing the end
/// offset alongside the start lets callers answer "what name is at this cursor offset?"
/// without re-scanning the source for identifier boundaries.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ResolvedNames<'arena> {
    /// Internal map: start offset -> (end offset, (resolved FQN, imported flag)).
    ///
    /// The inner pair is kept as a nested tuple (not flattened) so that [`all`](Self::all)
    /// can return `&(&'arena [u8], bool)` references — preserving the original signature
    /// for backward compatibility.
    names: HashMap<u32, (u32, (&'arena [u8], bool))>,

    /// Start offset of every bare PHP# name -> what it refers to. Empty for PHP.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty"))]
    bindings: HashMap<u32, Binding>,

    /// The PHP# scope rules the bare names break, in source order. Empty for PHP.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty"))]
    binding_errors: Vec<BindingError>,
}

impl<'arena> ResolvedNames<'arena> {
    /// Returns the total number of resolved names stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Returns `true` if no resolved names are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Checks if a resolved name exists at the given source `Position`.
    #[must_use]
    pub fn contains(&self, position: &Position) -> bool {
        self.names.contains_key(&position.offset)
    }

    /// Gets the resolved name identifier for the given source position.
    ///
    /// # Panics
    ///
    /// Panics if no resolved name is found at the specified `position`.
    /// Use `contains` first if unsure.
    #[allow(clippy::expect_used)]
    pub fn get<T>(&self, position: &T) -> &'arena [u8]
    where
        T: HasPosition,
    {
        self.names.get(&position.offset()).map(|(_, (name, _))| *name).expect("resolved name not found at position")
    }

    /// Attempts to resolve the name at the given source position.
    ///
    /// Returns `Some(&str)` if a resolved name exists at the position, or `None` otherwise.
    pub fn resolve<T>(&self, position: &T) -> Option<&'arena [u8]>
    where
        T: HasPosition,
    {
        self.names.get(&position.offset()).map(|(_, (name, _))| *name)
    }

    /// Checks if the name resolved at the given position originated from an explicit
    /// `use` alias or construct.
    ///
    /// Returns `false` if the name was resolved relative to the namespace, is a
    /// definition, or if no name is found at the position.
    pub fn is_imported<T>(&self, position: &T) -> bool
    where
        T: HasPosition,
    {
        self.names.get(&position.offset()).is_some_and(|(_, (_, imported))| *imported)
    }

    /// Returns the resolved entry whose source range covers the given byte offset.
    ///
    /// Identifier ranges in `ResolvedNames` never overlap, so at most one entry can
    /// match. Returns `Some((start, end, fqn, imported))` for the covering entry, or
    /// `None` if the offset falls outside every tracked identifier.
    #[must_use]
    pub fn at_offset(&self, offset: u32) -> Option<(u32, u32, &'arena [u8], bool)> {
        self.names.iter().find_map(|(&start, &(end, (name, imported)))| {
            if start <= offset && offset < end { Some((start, end, name, imported)) } else { None }
        })
    }

    /// Iterates over every resolved entry as `(start, end, fqn, imported)`.
    pub fn iter(&self) -> impl Iterator<Item = (u32, u32, &'arena [u8], bool)> + '_ {
        self.names.iter().map(|(&start, &(end, (name, imported)))| (start, end, name, imported))
    }

    /// Byte ranges `(start, end)` of every entry whose resolved name matches
    /// `fqcn`, compared case-insensitively (PHP name resolution is). This
    /// matches aliased uses too: `use Bar as Qux; Qux\G` resolves to `Bar\G`, so
    /// searching for `Bar\G` finds it where a raw text scan would not.
    ///
    /// `exclude_offset` drops the entry starting at that offset, e.g. to omit a
    /// declaration from its own reference list; pass `None` to keep every match.
    #[must_use]
    pub fn references_to(&self, fqcn: &[u8], exclude_offset: Option<u32>) -> Vec<(u32, u32)> {
        self.iter()
            .filter(|(start, _, _, _)| exclude_offset.is_none_or(|offset| offset != *start))
            .filter_map(
                |(start, end, name, _)| if eq_ignore_ascii_case(name, fqcn) { Some((start, end)) } else { None },
            )
            .collect()
    }

    /// Returns what the bare PHP# name starting at the given position refers to.
    ///
    /// Returns `None` for every name in a PHP file.
    pub fn binding<T>(&self, position: &T) -> Option<Binding>
    where
        T: HasPosition,
    {
        self.bindings.get(&position.offset()).copied()
    }

    /// Returns the class of a PHP# static call, `Class.method()`: the object of `call` when the binder bound it to a
    /// class. Returns `None` for an instance call and for every call in a PHP file.
    ///
    /// The checker and the engine both read this, so they never disagree on a static call.
    #[must_use]
    pub fn static_call_class<'ast>(&self, call: &MethodCall<'ast>) -> Option<&'ast ConstantAccess<'ast>> {
        self.class_object(call.object)
    }

    /// Returns the class of a PHP# static property access, `Class.name`: the object of `access` when the binder bound
    /// it to a class. Returns `None` for an instance property and for every access in a PHP file.
    ///
    /// The checker, the analyzer and the engine all read this, so they never disagree on a static property.
    #[must_use]
    pub fn static_property_class<'ast>(&self, access: &PropertyAccess<'ast>) -> Option<&'ast ConstantAccess<'ast>> {
        self.class_object(access.object)
    }

    fn class_object<'ast>(&self, object: &'ast Expression<'ast>) -> Option<&'ast ConstantAccess<'ast>> {
        match object {
            Expression::ConstantAccess(access) if self.binding(&access.name) == Some(Binding::Class) => Some(access),
            _ => None,
        }
    }

    /// Returns the PHP# scope rules the bare names break, in source order.
    #[must_use]
    pub fn binding_errors(&self) -> &[BindingError] {
        &self.binding_errors
    }

    pub(crate) fn bind(&mut self, span: Span, binding: Binding) {
        self.bindings.insert(span.start.offset, binding);
    }

    pub(crate) fn report_binding_error(&mut self, error: BindingError) {
        self.binding_errors.push(error);
    }

    /// Inserts a resolution result into the map (intended for internal use).
    ///
    /// The full source span of the identifier is stored, so [`at_offset`](Self::at_offset)
    /// and other range-based lookups work without re-scanning the source.
    pub(crate) fn insert_at(&mut self, span: Span, name: &'arena [u8], imported: bool) {
        self.names.insert(span.start.offset, (span.end.offset, (name, imported)));
    }

    /// Returns a `HashSet` containing every resolution result as `(&start, (fqn, imported))`.
    #[deprecated(
        note = "Allocates a HashSet for no good reason. Prefer `iter()` for allocation-free iteration, or `at_offset()` for cursor lookups."
    )]
    #[must_use]
    pub fn all(&self) -> HashSet<(&u32, &(&'arena [u8], bool))> {
        self.names.iter().map(|(k, (_, inner))| (k, inner)).collect()
    }
}

/// Case-insensitive byte equality, routing equal-length inputs through
/// `mago_word`'s SIMD prefix comparison.
#[inline]
fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && mago_word::starts_with_ignore_case(a, b)
}
