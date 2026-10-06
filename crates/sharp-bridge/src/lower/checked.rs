use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_semantics::SemanticsChecker;
use mago_syntax::cst::Program;

/// A program the semantic checks passed, `check_slice` among them, with its resolved names. Only [`check`] creates
/// one, and the lowering takes one, so the lowering never sees a construct the checks refuse.
///
/// Its fields are private to this module, so code that skips the checks cannot build one:
///
/// ```compile_fail
/// # mod checked {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/checked.rs"));
/// # }
/// # use mago_allocator::LocalArena;
/// # use mago_database::file::File;
/// # use mago_names::ResolvedNames;
/// # use mago_syntax::cst::Program;
/// fn skip_the_checks<'arena>(
///     _arena: &'arena LocalArena,
///     _file: &File,
///     program: &'arena Program<'arena>,
///     names: ResolvedNames<'arena>,
/// ) -> Option<checked::CheckedProgram<'arena>> {
///     Some(checked::CheckedProgram { program, names })
/// }
/// # fn main() {}
/// ```
///
/// The same code compiles when the program goes through [`check`]:
///
/// ```
/// # mod checked {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/checked.rs"));
/// # }
/// # use mago_allocator::LocalArena;
/// # use mago_database::file::File;
/// # use mago_names::ResolvedNames;
/// # use mago_syntax::cst::Program;
/// fn skip_the_checks<'arena>(
///     arena: &'arena LocalArena,
///     file: &File,
///     program: &'arena Program<'arena>,
///     _names: ResolvedNames<'arena>,
/// ) -> Option<checked::CheckedProgram<'arena>> {
///     checked::check(arena, file, program).ok()
/// }
/// # fn main() {}
/// ```
pub(super) struct CheckedProgram<'arena> {
    program: &'arena Program<'arena>,
    names: ResolvedNames<'arena>,
}

impl<'arena> CheckedProgram<'arena> {
    /// `program` as if the checks passed, for the tests of what the lowering does with a construct they refuse.
    #[cfg(test)]
    pub(super) fn unchecked(program: &'arena Program<'arena>, names: ResolvedNames<'arena>) -> Self {
        Self { program, names }
    }

    pub(super) fn program(&self) -> &'arena Program<'arena> {
        self.program
    }

    pub(super) fn names(&self) -> &ResolvedNames<'arena> {
        &self.names
    }
}

/// Resolves the names of `program` and runs the semantic checks on it, `check_slice` among them. Returns the errors
/// they report, or the checked program when there are none.
pub(super) fn check<'arena>(
    arena: &'arena LocalArena,
    file: &File,
    program: &'arena Program<'arena>,
) -> Result<CheckedProgram<'arena>, Vec<Issue>> {
    let names = NameResolver::new(arena).resolve(program);
    let errors: Vec<Issue> = SemanticsChecker::new(PHPVersion::PHP85)
        .check(file, program, &names)
        .into_iter()
        .filter(|issue| issue.level == Level::Error)
        .collect();
    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(CheckedProgram { program, names })
}
