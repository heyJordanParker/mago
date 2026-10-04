use mago_allocator::prelude::*;
use mago_database::file::HasFileId;
use mago_span::Span;

use crate::T;
use crate::cst::cst::ForOf;
use crate::cst::cst::ForOfKeyValueTarget;
use crate::cst::cst::ForOfTarget;
use crate::cst::cst::Keyword;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether the `for` header after `(` starts a PHP# `for … of` loop: `let` or `const`, then `[` or a name followed
    /// by `of` or `in`.
    pub(crate) fn is_at_for_of(&mut self) -> Result<bool, ParseError> {
        if !self.dialect.is_sharp() {
            return Ok(false);
        }

        let declares = self
            .stream
            .lookahead(0)?
            .is_some_and(|token| token.kind == T!["const"] || (token.kind == T![Identifier] && token.value == b"let"));
        if !declares {
            return Ok(false);
        }

        Ok(match self.stream.lookahead(1)? {
            Some(token) if token.kind == T!["["] => true,
            Some(token) if token.kind.is_identifier_maybe_reserved() => self
                .stream
                .lookahead(2)?
                .is_some_and(|token| token.kind == T![Identifier] && matches!(token.value, b"of" | b"in")),
            _ => false,
        })
    }

    pub(crate) fn parse_for_of(
        &mut self,
        r#for: Keyword<'arena>,
        left_parenthesis: Span,
    ) -> Result<ForOf<'arena>, ParseError> {
        Ok(ForOf {
            r#for,
            left_parenthesis,
            keyword: self.expect_any_keyword()?,
            target: self.parse_for_of_target()?,
            of: self.parse_of_keyword()?,
            expression: self.parse_expression()?,
            right_parenthesis: self.stream.eat_span(T![")"])?,
            body: self.arena.alloc(self.parse_statement()?),
        })
    }

    fn parse_for_of_target(&mut self) -> Result<ForOfTarget<'arena>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T!["["]) => ForOfTarget::KeyValue(ForOfKeyValueTarget {
                left_bracket: self.stream.eat_span(T!["["])?,
                key: self.parse_local_identifier()?,
                comma: self.stream.eat_span(T![","])?,
                value: self.parse_local_identifier()?,
                right_bracket: self.stream.eat_span(T!["]"])?,
            }),
            _ => ForOfTarget::Value(self.parse_local_identifier()?),
        })
    }

    /// Consumes `of`. TypeScript's `in` is reported with the `of` to write, and the loop still parses.
    fn parse_of_keyword(&mut self) -> Result<Keyword<'arena>, ParseError> {
        let token = self.stream.consume()?;
        let keyword = Keyword { span: token.span_for(self.stream.file_id()), value: token.value };
        if token.kind != T![Identifier] || !matches!(token.value, b"of" | b"in") {
            return Err(self.stream.unexpected(Some(token), &[]));
        }

        if token.value == b"in" {
            self.errors.push(ParseError::ForInInSharp(keyword.span));
        }

        Ok(keyword)
    }
}
