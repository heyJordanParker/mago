//! Runs the front end on each file named on the command line, so `/usr/bin/time -l` counts only its instructions.
//!
//! A `.sharp` file goes through `sharp_lower`, as the engine compiles it. Any other file goes through the parser, the
//! binder and the semantic checks in the dialect its name selects, as the checker reads it. `--until parse`,
//! `--until names` or `--until checks` stops after that pass, so the difference between two counts is one pass's cost.
//! A diagnostic, a parse error or a semantic issue panics, so a count never measures a front end that failed.
//! `bench.sh` beside this crate generates the classes and runs this example on them.

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
    let mut arguments = env::args().skip(1).peekable();
    let until = if arguments.next_if_eq("--until").is_some() { arguments.next() } else { None };

    for path in arguments {
        let source = fs::read(&path).expect("the file is readable");

        if until.is_none() && Path::new(&path).extension().is_some_and(|extension| extension == "sharp") {
            // SAFETY: both pointers point to as many bytes as their lengths say.
            let unit = unsafe {
                sharp_lower(path.as_ptr().cast::<c_char>(), path.len(), source.as_ptr().cast::<c_char>(), source.len())
            };
            // SAFETY: `sharp_lower` returns a valid unit, freed below.
            let lowered = unsafe { &*unit };
            let (nodes, diagnostics) = (lowered.node_count, lowered.diagnostic_count);
            // SAFETY: `sharp_lower` returned the unit, and nothing reads it after this.
            unsafe { sharp_unit_free(unit) };

            assert_eq!(diagnostics, 0, "{path} lowers without diagnostics");
            println!("{path}: {nodes} nodes");
        } else {
            let file = File::ephemeral(Cow::Owned(path.clone().into_bytes()), Cow::Owned(source));
            let arena = LocalArena::new();
            let program = parse_file(&arena, &file);
            assert!(program.errors.is_empty(), "{path} parses without errors: {:?}", program.errors);
            if until.as_deref() == Some("parse") {
                println!("{path}: parsed");
                continue;
            }

            let names = NameResolver::new(&arena).resolve(program);
            if until.as_deref() == Some("names") {
                println!("{path}: {} names", names.len());
                continue;
            }

            let issues = SemanticsChecker::new(PHPVersion::PHP85).check(&file, program, &names);
            let messages = issues.iter().map(|issue| &issue.message).collect::<Vec<_>>();
            assert!(messages.is_empty(), "{path} checks without issues: {messages:?}");

            println!("{path}: {} names", names.len());
        }
    }
}
