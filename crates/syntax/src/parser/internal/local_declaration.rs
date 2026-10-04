use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::LocalDeclaration;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# local declaration starts here: `const`, or `let` followed by a name.
    pub(crate) fn is_at_local_declaration(&mut self) -> Result<bool, ParseError> {
        if !self.dialect.is_sharp() {
            return Ok(false);
        }

        Ok(match self.stream.lookahead(0)? {
            Some(token) if token.kind == T!["const"] => true,
            Some(token) if token.kind == T![Identifier] && token.value == b"let" => {
                self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved())
            }
            _ => false,
        })
    }

    pub(crate) fn parse_local_declaration(&mut self) -> Result<LocalDeclaration<'arena>, ParseError> {
        Ok(LocalDeclaration {
            keyword: self.expect_any_keyword()?,
            name: self.parse_local_identifier()?,
            equals: self.stream.eat_span(T!["="])?,
            value: self.parse_expression()?,
            terminator: self.parse_terminator()?,
        })
    }
}
