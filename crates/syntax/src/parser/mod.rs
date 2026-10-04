use mago_allocator::prelude::*;

use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::HasFileId;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax_core::input::Input;

use crate::cst::Expression;
use crate::cst::Program;
use crate::cst::Statement;
use crate::cst::sequence::Sequence;
use crate::dialect::Dialect;
use crate::error::ParseError;
use crate::lexer::Lexer;
use crate::parser::stream::TokenStream;
use crate::settings::ParserSettings;
use crate::walker::MutWalker;

mod internal;

pub mod stream;

/// Maximum recursion depth of statement and expression parsing, which bounds the parser's work on deeply nested input.
///
/// A PHP# file nests no statement or expression deeper, however the parser built it, so the engine compiles every
/// PHP# file the parser accepts.
pub(crate) const MAX_RECURSION_DEPTH: u16 = 512;

#[derive(Debug, Default)]
pub struct State {
    pub within_string_interpolation: bool,
    pub recursion_depth: u16,
}

/// The main parser for PHP source code.
///
/// The parser holds an arena reference, the token stream, and parsing state.
#[derive(Debug)]
#[allow(clippy::field_scoped_visibility_modifiers)]
pub struct Parser<'input, 'arena, A>
where
    'input: 'arena,
    A: Arena,
{
    pub(crate) arena: &'arena A,
    pub(crate) dialect: Dialect,
    pub(crate) state: State,
    pub(crate) stream: TokenStream<'input, 'arena, A>,
    pub(crate) errors: Vec<'arena, ParseError, A>,
}

impl<'input, 'arena, A> Parser<'input, 'arena, A>
where
    A: Arena,
{
    /// Creates a new parser for the given PHP content.
    ///
    /// # Parameters
    ///
    /// - `arena`: The memory arena for allocations.
    /// - `file_id`: The ID of the file being parsed.
    /// - `content`: The content to parse.
    /// - `settings`: The parser settings.
    ///
    /// # Returns
    ///
    /// A new `Parser` instance.
    #[inline]
    pub fn new(arena: &'arena A, file_id: FileId, content: &'input [u8], settings: ParserSettings) -> Self {
        Self::for_dialect(arena, file_id, content, Dialect::Php, settings)
    }

    fn for_dialect(
        arena: &'arena A,
        file_id: FileId,
        content: &'input [u8],
        dialect: Dialect,
        settings: ParserSettings,
    ) -> Self {
        let input = Input::new(file_id, content);
        let lexer = match dialect {
            Dialect::Php => Lexer::new(input, settings.lexer),
            Dialect::Sharp => Lexer::scripting(input, settings.lexer),
        };
        let stream = TokenStream::new(arena, lexer);

        Self { arena, dialect, state: State::default(), stream, errors: Vec::new_in(arena) }
    }

    /// Creates a new parser for the given file, in the dialect its name selects.
    ///
    /// # Parameters
    ///
    /// - `arena`: The memory arena for allocations.
    /// - `file`: The file to parse.
    /// - `settings`: The parser settings.
    ///
    /// # Returns
    ///
    /// A new `Parser` instance.
    pub fn for_file(arena: &'arena A, file: &'input File, settings: ParserSettings) -> Self {
        Self::for_dialect(arena, file.file_id(), file.contents.as_ref(), Dialect::of(file), settings)
    }

    /// Parses and returns the program CST.
    fn parse(mut self, source_text: &'arena [u8], file_id: FileId) -> &'arena Program<'arena> {
        let mut statements = Vec::new_in(self.arena);

        loop {
            let reached_eof = match self.stream.has_reached_eof() {
                Ok(eof) => eof,
                Err(err) => {
                    self.errors.push(ParseError::from(err));
                    break;
                }
            };

            if reached_eof {
                break;
            }

            // Record position before parsing to detect infinite loops
            let position_before = self.stream.current_position();

            match self.parse_statement() {
                Ok(statement) => match self.nesting_too_deep(&statement) {
                    Some(error) => self.errors.push(error),
                    None => statements.push(statement),
                },
                Err(err) => self.errors.push(err),
            }

            // Safety check: if we didn't advance at all, skip a token to prevent infinite loop.
            // This can happen with orphan keywords like `finally`, `catch`, `else`, etc.
            // that are preserved by the expression parser but not handled by the statement parser.
            let position_after = self.stream.current_position();
            if position_after == position_before
                && let Ok(Some(token)) = self.stream.lookahead(0)
            {
                self.errors.push(self.stream.unexpected(Some(token), &[]));
                let _ = self.stream.consume();
            }
        }

        self.arena.alloc(Program {
            file_id,
            dialect: self.dialect,
            source_text,
            statements: Sequence::new(statements),
            trivia: self.stream.get_trivia(),
            errors: self.errors.leak(),
        })
    }

    /// The error for a statement or expression that recursed past [`MAX_RECURSION_DEPTH`] while parsing.
    pub(crate) fn recursion_limit_exceeded(&self, span: Span) -> ParseError {
        match self.dialect {
            Dialect::Php => ParseError::RecursionLimitExceeded(span),
            Dialect::Sharp => ParseError::NestingTooDeepInSharp(span),
        }
    }

    /// The error for a top-level PHP# statement that nests a statement or expression deeper than
    /// [`MAX_RECURSION_DEPTH`]. The parser builds a chain such as `a + b + c` or `a.f().g()` in a loop, so only the
    /// finished statement shows how deep the chain nests. The parser leaves out the statement it refuses, so no later
    /// pass walks a tree deeper than the engine compiles.
    fn nesting_too_deep(&self, statement: &Statement<'arena>) -> Option<ParseError> {
        if self.dialect == Dialect::Php {
            return None;
        }

        let mut nesting = Nesting::default();
        nesting.walk_statement(statement, &mut ());

        nesting.too_deep.map(ParseError::NestingTooDeepInSharp)
    }
}

/// Finds the first statement or expression nested deeper than [`MAX_RECURSION_DEPTH`], counting the statements and
/// expressions around it.
#[derive(Default)]
struct Nesting {
    depth: usize,
    too_deep: Option<Span>,
}

impl Nesting {
    fn enter(&mut self, node: &impl HasSpan) {
        self.depth += 1;
        if self.depth > usize::from(MAX_RECURSION_DEPTH) && self.too_deep.is_none() {
            self.too_deep = Some(node.span());
        }
    }
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for Nesting {
    fn walk_in_statement(&mut self, statement: &'ast Statement<'arena>, _: &mut ()) {
        self.enter(statement);
    }

    fn walk_out_statement(&mut self, _: &'ast Statement<'arena>, _: &mut ()) {
        self.depth -= 1;
    }

    fn walk_in_expression(&mut self, expression: &'ast Expression<'arena>, _: &mut ()) {
        self.enter(expression);
    }

    fn walk_out_expression(&mut self, _: &'ast Expression<'arena>, _: &mut ()) {
        self.depth -= 1;
    }
}

/// Parses the given file in the dialect its name selects and returns the program CST.
///
/// # Parameters
///
/// - `arena`: The memory arena for allocations.
/// - `file`: The file to parse.
///
/// # Returns
///
/// The parsed `Program` CST.
#[inline]
pub fn parse_file<'arena, A>(arena: &'arena A, file: &File) -> &'arena Program<'arena>
where
    A: Arena,
{
    parse_file_with_settings(arena, file, ParserSettings::default())
}

/// Parses the given file with custom settings, in the dialect its name selects, and returns the program CST.
///
/// # Parameters
///
/// - `arena`: The memory arena for allocations.
/// - `file`: The file to parse.
/// - `settings`: The parser settings.
///
/// # Returns
///
/// The parsed `Program` CST.
#[inline]
pub fn parse_file_with_settings<'arena, A>(
    arena: &'arena A,
    file: &File,
    settings: ParserSettings,
) -> &'arena Program<'arena>
where
    A: Arena,
{
    parse_file_with_dialect(arena, file, Dialect::of(file), settings)
}

/// Parses the given file in `dialect`, whatever its name, with custom settings, and returns the program CST.
///
/// A caller that already knows the dialect, such as the PHP engine compiling a PHP# file, passes it here. Every other
/// caller lets [`parse_file_with_settings`] choose it from the file name.
///
/// # Parameters
///
/// - `arena`: The memory arena for allocations.
/// - `file`: The file to parse.
/// - `dialect`: The dialect to parse the file in.
/// - `settings`: The parser settings.
///
/// # Returns
///
/// The parsed `Program` CST.
pub fn parse_file_with_dialect<'arena, A>(
    arena: &'arena A,
    file: &File,
    dialect: Dialect,
    settings: ParserSettings,
) -> &'arena Program<'arena>
where
    A: Arena,
{
    let file_id = file.file_id();
    let source_text = arena.alloc_slice_copy(file.contents.as_ref());
    Parser::for_dialect(arena, file_id, source_text, dialect, settings).parse(source_text, file_id)
}

/// Parses the given PHP file content and returns the program CST.
///
/// # Parameters
///
/// - `arena`: The memory arena for allocations.
/// - `file_id`: The ID of the file being parsed.
/// - `content`: The content to parse.
///
/// # Returns
///
/// The parsed `Program` CST.
pub fn parse_file_content<'arena, A>(arena: &'arena A, file_id: FileId, content: &[u8]) -> &'arena Program<'arena>
where
    A: Arena,
{
    let source_text = arena.alloc_slice_copy(content);
    Parser::new(arena, file_id, source_text, ParserSettings::default()).parse(source_text, file_id)
}

/// Parses the given PHP file content with custom settings and returns the program CST.
///
/// # Parameters
///
/// - `arena`: The memory arena for allocations.
/// - `file_id`: The ID of the file being parsed.
/// - `content`: The content to parse.
/// - `settings`: The parser settings.
///
/// # Returns
///
/// The parsed `Program` CST.
pub fn parse_file_content_with_settings<'arena, A>(
    arena: &'arena A,
    file_id: FileId,
    content: &[u8],
    settings: ParserSettings,
) -> &'arena Program<'arena>
where
    A: Arena,
{
    let source_text = arena.alloc_slice_copy(content);
    Parser::new(arena, file_id, source_text, settings).parse(source_text, file_id)
}
