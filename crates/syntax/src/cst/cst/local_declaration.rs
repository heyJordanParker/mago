use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::expression::Expression;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::terminator::Terminator;

/// Represents a PHP# local declaration, which `let` makes reassignable and `const` does not.
///
/// Example: `let total = 0;` or `const currency = this.currency;`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct LocalDeclaration<'arena> {
    pub keyword: Keyword<'arena>,
    pub name: LocalIdentifier<'arena>,
    pub equals: Span,
    pub value: &'arena Expression<'arena>,
    pub terminator: Terminator<'arena>,
}

impl LocalDeclaration<'_> {
    /// Returns `true` when the local is declared with `const`.
    #[inline]
    #[must_use]
    pub fn is_const(&self) -> bool {
        self.keyword.value.eq_ignore_ascii_case(b"const")
    }
}

impl HasSpan for LocalDeclaration<'_> {
    fn span(&self) -> Span {
        self.keyword.span().join(self.terminator.span())
    }
}
