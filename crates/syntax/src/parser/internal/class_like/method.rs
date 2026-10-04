use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::FunctionLikeReturnTypeHint;
use crate::cst::cst::Method;
use crate::cst::cst::MethodAbstractBody;
use crate::cst::cst::MethodBody;
use crate::cst::cst::Modifier;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_method_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Method<'arena>, ParseError> {
        let function = self.expect_keyword(T!["function"])?;
        // The error stands, and the PHP method still parses so the rest of the class does.
        if self.dialect.is_sharp() {
            self.errors.push(ParseError::PhpSyntaxInSharp(T!["function"], function.span));
        }

        Ok(Method {
            attribute_lists: attributes,
            modifiers,
            function: Some(function),
            ampersand: if self.stream.is_at(T!["&"])? { Some(self.stream.eat_span(T!["&"])?) } else { None },
            name: self.parse_local_identifier()?,
            parameter_list: self.parse_function_like_parameter_list()?,
            return_type_hint: self.parse_optional_function_like_return_type_hint()?,
            body: self.parse_method_body()?,
        })
    }

    /// Parses a PHP# method, whose return type comes first, with no colon and no `function` keyword.
    pub(crate) fn parse_sharp_method_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Method<'arena>, ParseError> {
        Ok(Method {
            attribute_lists: attributes,
            modifiers,
            function: None,
            return_type_hint: Some(FunctionLikeReturnTypeHint { colon: None, hint: self.parse_type_hint()? }),
            ampersand: None,
            name: self.parse_local_identifier()?,
            parameter_list: self.parse_function_like_parameter_list()?,
            body: self.parse_method_body()?,
        })
    }

    /// Returns `true` when the next class member is a PHP# method: a type, then a name, then `(`.
    ///
    /// A property names its variable with a `$`, as it does in PHP.
    pub(crate) fn is_at_sharp_method(&mut self) -> Result<bool, ParseError> {
        let mut offset = 0;
        loop {
            match (self.stream.peek_kind(offset)?, self.stream.peek_kind(offset + 1)?) {
                (Some(kind), Some(T!["("])) if kind.is_identifier_maybe_reserved() => return Ok(true),
                (None | Some(T!["$variable" | ";" | "=" | "{" | "}"]), _) => return Ok(false),
                _ => offset += 1,
            }
        }
    }

    fn parse_method_body(&mut self) -> Result<MethodBody<'arena>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T![";" | "?>"]) => MethodBody::Abstract(MethodAbstractBody { terminator: self.parse_terminator()? }),
            _ => MethodBody::Concrete(self.parse_block()?),
        })
    }
}
