use crate::T;
use crate::cst::cst::Extends;
use crate::cst::cst::Implements;
use crate::cst::cst::Inheritance;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_optional_implements(&mut self) -> Result<Option<Implements<'arena>>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T!["implements"]) => Some(Implements {
                implements: self.expect_any_keyword()?,
                types: {
                    let mut types = self.new_vec();
                    let mut commas = self.new_vec();
                    loop {
                        types.push(self.parse_identifier()?);

                        match self.stream.peek_kind(0)? {
                            Some(T![","]) => {
                                commas.push(self.stream.consume()?);
                            }
                            _ => break,
                        }
                    }

                    TokenSeparatedSequence::new(types, commas)
                },
            }),
            _ => None,
        })
    }

    /// PHP#'s `: Base, Interface` header, as spec section 22 writes it.
    pub(crate) fn parse_optional_inheritance(&mut self) -> Result<Option<Inheritance<'arena>>, ParseError> {
        if !self.dialect.is_sharp() || !matches!(self.stream.peek_kind(0)?, Some(T![":"])) {
            return Ok(None);
        }

        let colon = self.stream.consume_span()?;
        let mut types = self.new_vec();
        let mut commas = self.new_vec();
        loop {
            types.push(self.parse_identifier()?);

            match self.stream.peek_kind(0)? {
                Some(T![","]) => {
                    commas.push(self.stream.consume()?);
                }
                _ => break,
            }
        }

        Ok(Some(Inheritance { colon, types: TokenSeparatedSequence::new(types, commas) }))
    }

    pub(crate) fn parse_optional_extends(&mut self) -> Result<Option<Extends<'arena>>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T!["extends"]) => Some(Extends {
                extends: self.expect_any_keyword()?,
                types: {
                    let mut types = self.new_vec();
                    let mut commas = self.new_vec();
                    loop {
                        types.push(self.parse_identifier()?);

                        match self.stream.peek_kind(0)? {
                            Some(T![","]) => {
                                commas.push(self.stream.consume()?);
                            }
                            _ => break,
                        }
                    }
                    TokenSeparatedSequence::new(types, commas)
                },
            }),
            _ => None,
        })
    }
}
