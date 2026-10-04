use mago_span::HasSpan;
use mago_syntax::error::ParseError;
use mago_syntax::error::SyntaxError;

use crate::ir::error::Error;
use crate::ir::error::ErrorKind;

pub(crate) fn lower_parse_error(error: &ParseError) -> Error {
    let span = error.span();
    let kind = match error {
        ParseError::SyntaxError(SyntaxError::UnexpectedToken(..)) => ErrorKind::UnexpectedToken,
        ParseError::SyntaxError(SyntaxError::UnrecognizedToken(..)) => ErrorKind::UnrecognizedToken,
        ParseError::SyntaxError(SyntaxError::UnexpectedEndOfFile(..)) => ErrorKind::UnexpectedEndOfFile,
        ParseError::SyntaxError(SyntaxError::RecursionLimitExceeded(..)) => ErrorKind::RecursionLimitExceeded,
        ParseError::UnexpectedEndOfFile(..) => ErrorKind::UnexpectedEndOfFile,
        // PHP syntax in a PHP# file is a token the PHP# grammar does not expect there.
        ParseError::UnexpectedToken(..) | ParseError::PhpSyntaxInSharp(..) => ErrorKind::UnexpectedToken,
        ParseError::UnclosedLiteralString(..) => ErrorKind::UnclosedLiteralString,
        ParseError::RecursionLimitExceeded(..) => ErrorKind::RecursionLimitExceeded,
    };

    Error { span, kind }
}
