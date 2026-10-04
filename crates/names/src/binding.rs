use mago_span::Span;

/// What a bare PHP# name refers to.
///
/// The parser reads `x`, `x.y` and `x.y()` the same way whatever `x` is. The binder then decides,
/// from the scopes of the file alone, what each bare name means.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum Binding {
    /// A parameter, or a local declared with `let` or `const`, whose block is still open.
    Local(Local),
    /// `this`, the object the method runs on.
    This,
    /// A class, written before `.`. Its fully qualified name is resolved like any class name.
    Class,
    /// A constant. Its fully qualified name is resolved like any constant name.
    Constant,
    /// A member of the enclosing class, written without `this.`.
    Member,
    /// A local whose block has already closed.
    OutOfScope(Local),
    /// The declaration of a local whose name an enclosing block of the same method already declares.
    Redeclared(Local),
}

/// A local of a PHP# method: where it is declared, and how.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Local {
    /// The name in the local's declaration.
    pub declaration: Span,
    pub kind: LocalKind,
}

/// How a PHP# local is declared.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum LocalKind {
    Parameter,
    /// Declared with `let`, so it can be reassigned.
    Let,
    /// Declared with `const`, so it cannot be reassigned.
    Const,
}
