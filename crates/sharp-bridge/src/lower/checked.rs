use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::metadata::CodebaseMetadata;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_syntax::cst::Program;
use mago_syntax::error::ParseError;

use super::types::Types;

/// A program the checker accepted, with its resolved names and the types the analysis gave it.
///
/// It parsed without errors, and no check reported an error-level issue. Only [`check`] creates one, and the lowering
/// takes one, so the lowering never sees a construct the parser or the checks refuse.
///
/// Its fields are private to this module, so code that skips the checks cannot build one:
///
/// ```compile_fail
/// # mod types {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/types.rs"));
/// # }
/// # mod checked {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/checked.rs"));
/// # }
/// # use mago_analyzer::artifacts::AnalysisArtifacts;
/// # use mago_codex::metadata::CodebaseMetadata;
/// # use mago_database::file::File;
/// # use mago_names::ResolvedNames;
/// # use mago_syntax::cst::Program;
/// fn skip_the_checks<'program>(
///     file: &'program File,
///     program: &'program Program<'program>,
///     names: ResolvedNames<'program>,
///     artifacts: &'program AnalysisArtifacts,
///     codebase: &'program CodebaseMetadata,
/// ) -> Option<checked::CheckedProgram<'program>> {
///     Some(checked::CheckedProgram { file, program, names, types: types::Types::new(artifacts, codebase) })
/// }
/// # fn main() {}
/// ```
///
/// The same code compiles when the program goes through [`check`]:
///
/// ```
/// # mod types {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/types.rs"));
/// # }
/// # mod checked {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/checked.rs"));
/// # }
/// # use mago_analyzer::artifacts::AnalysisArtifacts;
/// # use mago_codex::metadata::CodebaseMetadata;
/// # use mago_database::file::File;
/// # use mago_names::ResolvedNames;
/// # use mago_syntax::cst::Program;
/// fn skip_the_checks<'program>(
///     file: &'program File,
///     program: &'program Program<'program>,
///     names: ResolvedNames<'program>,
///     artifacts: &'program AnalysisArtifacts,
///     codebase: &'program CodebaseMetadata,
/// ) -> Option<checked::CheckedProgram<'program>> {
///     checked::check(file, program, names, artifacts, codebase, &[]).ok()
/// }
/// # fn main() {}
/// ```
pub struct CheckedProgram<'program> {
    file: &'program File,
    program: &'program Program<'program>,
    names: ResolvedNames<'program>,
    #[expect(dead_code, reason = "the first typed site in piece 3 reads it")]
    types: Types<'program>,
}

impl<'program> CheckedProgram<'program> {
    /// `program` as if the checks passed, for the tests of what the lowering does with a construct they refuse.
    #[cfg(test)]
    pub(crate) fn unchecked(
        file: &'program File,
        program: &'program Program<'program>,
        names: ResolvedNames<'program>,
        artifacts: &'program AnalysisArtifacts,
        codebase: &'program CodebaseMetadata,
    ) -> Self {
        Self { file, program, names, types: Types::new(artifacts, codebase) }
    }

    pub(crate) fn file(&self) -> &'program File {
        self.file
    }

    pub(crate) fn program(&self) -> &'program Program<'program> {
        self.program
    }

    pub(crate) fn names(&self) -> &ResolvedNames<'program> {
        &self.names
    }

    #[expect(dead_code, reason = "the first typed site in piece 3 reads it")]
    pub(crate) fn types(&self) -> &Types<'program> {
        &self.types
    }
}

/// Why [`check`] refused a program.
#[derive(Debug)]
pub enum Refusal<'program> {
    /// The parser's errors.
    Parse(&'program [ParseError]),
    /// The error-level issues the checks reported.
    Compile(Vec<Issue>),
}

/// Accepts `program` when it parsed without errors and the checks reported no error-level issue.
///
/// `program` is the orchestrator's parse of `file`, and `issues` is every issue the checks reported for the file.
/// `artifacts` and `codebase` are the file's analysis, which the lowering reads its types from.
///
/// # Errors
///
/// Returns the parser's errors, or else the error-level issues.
pub fn check<'program>(
    file: &'program File,
    program: &'program Program<'program>,
    names: ResolvedNames<'program>,
    artifacts: &'program AnalysisArtifacts,
    codebase: &'program CodebaseMetadata,
    issues: &[Issue],
) -> Result<CheckedProgram<'program>, Refusal<'program>> {
    if !program.errors.is_empty() {
        return Err(Refusal::Parse(program.errors));
    }

    let errors: Vec<Issue> = issues.iter().filter(|issue| issue.level == Level::Error).cloned().collect();
    if !errors.is_empty() {
        return Err(Refusal::Compile(errors));
    }

    Ok(CheckedProgram { file, program, names, types: Types::new(artifacts, codebase) })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use mago_allocator::LocalArena;
    use mago_names::resolver::NameResolver;
    use mago_syntax::dialect::Dialect;
    use mago_syntax::parser::parse_file_with_dialect;
    use mago_syntax::settings::ParserSettings;

    use super::*;

    fn file(code: &'static str) -> File {
        File::ephemeral(Cow::Borrowed(b"src/Report.sharp"), Cow::Borrowed(code.as_bytes()))
    }

    #[test]
    fn a_program_with_a_parse_error_is_not_checked() {
        let file = file("class Report\n{\n    public int run()\n    {\n        return this->run();\n    }\n}\n");
        let arena = LocalArena::new();
        let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
        let names = NameResolver::new(&arena).resolve(program);
        let (artifacts, codebase) = (AnalysisArtifacts::new(), CodebaseMetadata::new());

        let refusal = check(&file, program, names, &artifacts, &codebase, &[]);

        assert!(matches!(refusal, Err(Refusal::Parse(errors)) if errors.len() == 1));
    }

    #[test]
    fn a_program_with_an_error_level_issue_is_refused_with_only_its_errors() {
        let file = file("class Report\n{\n}\n");
        let arena = LocalArena::new();
        let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
        let names = NameResolver::new(&arena).resolve(program);
        let (artifacts, codebase) = (AnalysisArtifacts::new(), CodebaseMetadata::new());
        let issues = [Issue::warning("a warning"), Issue::error("an error"), Issue::help("a help")];

        let refusal = check(&file, program, names, &artifacts, &codebase, &issues);

        assert!(
            matches!(&refusal, Err(Refusal::Compile(errors)) if errors.len() == 1 && errors[0].message == "an error")
        );
    }
}
