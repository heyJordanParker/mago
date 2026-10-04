use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::LocalDeclaration;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
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
