//! Per-file derived data computed in a single parse + name-resolve pass.

use std::sync::Arc;

use mago_allocator::LocalArena;
use mago_database::file::File as MagoFile;
use mago_linter::Linter;
use mago_names::resolver::NameResolver;
use mago_reporting::IssueCollection;
use mago_syntax::parser::parse_file_with_settings;

use crate::linter::LinterContext;

/// Owned, cacheable view of one file. Built by [`build`]; held on the
/// workspace state for every file that's ever been touched.
#[derive(Debug)]
pub struct FileAnalysis {
    pub lint_issues: IssueCollection,
}

/// Run one parse + resolve pass over `file` and extract every per-file
/// derivative the server keeps.
#[must_use]
pub fn build(file: &MagoFile, linter_ctx: &LinterContext) -> FileAnalysis {
    let arena = LocalArena::new();

    let program = parse_file_with_settings(&arena, file, linter_ctx.parser_settings);
    let resolved = NameResolver::new(&arena).resolve(program);

    let linter = Linter::from_registry(&arena, Arc::clone(&linter_ctx.registry), linter_ctx.settings.php_version);

    FileAnalysis { lint_issues: linter.lint(file, program, &resolved) }
}
