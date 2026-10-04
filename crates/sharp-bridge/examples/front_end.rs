//! Runs the front end on each file named on the command line, so `/usr/bin/time -l` counts only its instructions.
//!
//! A `.sharp` file goes through `sharp_lower`, as the engine compiles it. Any other file goes through the parser, the
//! binder and the semantic checks in the dialect its name selects, as the checker reads it. `bench.sh` beside this
//! crate generates the classes and runs this example on them.

#![allow(clippy::print_stdout, clippy::expect_used)]

use std::borrow::Cow;
use std::env;
use std::ffi::c_char;
use std::fs;
use std::path::Path;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_semantics::SemanticsChecker;
use mago_sharp_bridge::sharp_lower;
use mago_sharp_bridge::sharp_unit_free;
use mago_syntax::parser::parse_file;

fn main() {
    for path in env::args().skip(1) {
        let source = fs::read(&path).expect("the file is readable");

        if Path::new(&path).extension().is_some_and(|extension| extension == "sharp") {
            // SAFETY: both pointers point to as many bytes as their lengths say.
            let unit = unsafe {
                sharp_lower(path.as_ptr().cast::<c_char>(), path.len(), source.as_ptr().cast::<c_char>(), source.len())
            };
            // SAFETY: `sharp_lower` returns a valid unit, freed below.
            let lowered = unsafe { &*unit };
            let (nodes, diagnostics) = (lowered.node_count, lowered.diagnostic_count);
            // SAFETY: `sharp_lower` returned the unit, and nothing reads it after this.
            unsafe { sharp_unit_free(unit) };

            println!("{path}: {nodes} nodes, {diagnostics} diagnostics");
        } else {
            let file = File::ephemeral(Cow::Owned(path.clone().into_bytes()), Cow::Owned(source));
            let arena = LocalArena::new();
            let program = parse_file(&arena, &file);
            let names = NameResolver::new(&arena).resolve(program);
            let issues = SemanticsChecker::new(PHPVersion::PHP85).check(&file, program, &names);

            println!("{path}: {} names, {} parse errors, {} issues", names.len(), program.errors.len(), issues.len());
        }
    }
}
