use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::ClassLikeMember;
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

    /// Parses a PHP# class member that starts with its type: a method, whose return type comes first with no colon
    /// and no `function` keyword, or a field, `int count = 0;`.
    ///
    /// The type is parsed once, and the name after it decides: a name followed by `(` makes a method, anything else
    /// a field. A type has no length limit, so no fixed lookahead can decide before it. A PHP property, which starts
    /// with `var` or a `$` variable, still parses so the rest of the class does, and its PHP syntax is an error.
    pub(crate) fn parse_sharp_member_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<ClassLikeMember<'arena>, ParseError> {
        if self.stream.is_at(T!["var"])? || self.stream.is_at(T!["$variable"])? {
            return Ok(ClassLikeMember::Property(
                self.parse_property_with_attributes_and_modifiers(attributes, modifiers)?,
            ));
        }

        let hint = self.parse_type_hint()?;
        if !matches!(self.stream.peek_kind(1)?, Some(T!["("])) {
            return Ok(ClassLikeMember::Property(self.parse_property_with_hint(
                attributes,
                modifiers,
                None,
                Some(hint),
            )?));
        }

        Ok(ClassLikeMember::Method(Method {
            attribute_lists: attributes,
            modifiers,
            function: None,
            return_type_hint: Some(FunctionLikeReturnTypeHint { colon: None, hint }),
            ampersand: None,
            name: self.parse_local_identifier()?,
            parameter_list: self.parse_function_like_parameter_list()?,
            body: self.parse_method_body()?,
        }))
    }

    fn parse_method_body(&mut self) -> Result<MethodBody<'arena>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T![";" | "?>"]) => MethodBody::Abstract(MethodAbstractBody { terminator: self.parse_terminator()? }),
            _ => MethodBody::Concrete(self.parse_block()?),
        })
    }
}
