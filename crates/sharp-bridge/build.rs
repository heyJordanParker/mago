#![allow(clippy::expect_used)]

use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use cbindgen::Builder;
use cbindgen::Config;
use cbindgen::DocumentationStyle;
use cbindgen::Language;
use cbindgen::Style;

/// Writes the bridge's C header to `OUT_DIR`, exports that folder to `ext/sharp` as `DEP_SHARP_BRIDGE_INCLUDE`, and
/// records the Mago commit the bridge is built from.
fn main() {
    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let abi = crate_dir.join("src/lib.rs");
    let kinds = crate_dir.join("src/kind.rs");

    let config = Config {
        language: Language::C,
        style: Style::Type,
        usize_is_size_t: true,
        include_guard: Some("SHARP_BRIDGE_H".to_owned()),
        after_includes: Some(kinds_macro(&kinds)),
        documentation_style: DocumentationStyle::C99,
        ..Config::default()
    };

    Builder::new()
        .with_config(config)
        .with_src(&abi)
        .generate()
        .expect("the bridge ABI generates a C header")
        .write_to_file(out_dir.join("sharp_bridge.h"));

    println!("cargo:rerun-if-changed={}", abi.display());
    println!("cargo:rerun-if-changed={}", kinds.display());
    println!("cargo:include={}", out_dir.display());

    println!("cargo:rustc-env=SHARP_MAGO_COMMIT={}", git(&crate_dir, &["rev-parse", "HEAD"]));
    println!("cargo:rerun-if-changed={}", git_path(&crate_dir, "HEAD"));
    let branch = git(&crate_dir, &["rev-parse", "--symbolic-full-name", "HEAD"]);
    if branch != "HEAD" {
        println!("cargo:rerun-if-changed={}", git_path(&crate_dir, &branch));
    }
}

/// `SHARP_KINDS(X)` calls `X(KIND)` once per `sharp_kind`, so `ext/sharp` asserts that each equals its `zend_ast_kind`.
fn kinds_macro(kinds: &Path) -> String {
    let source = fs::read_to_string(kinds).expect("the generated sharp_kind is readable");
    let calls: Vec<String> = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("SHARP_AST_")?.split_once(" = "))
        .map(|(kind, _)| format!("  X({kind})"))
        .collect();

    format!("\n#define SHARP_KINDS(X) \\\n{}\n", calls.join(" \\\n"))
}

/// The absolute path of a file in the repository's git folder, such as `HEAD` or a branch ref.
fn git_path(crate_dir: &Path, name: &str) -> String {
    git(crate_dir, &["rev-parse", "--path-format=absolute", "--git-path", name])
}

fn git(crate_dir: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(crate_dir)
        .output()
        .expect("git runs: the bridge records the Mago commit it is built from");
    assert!(
        output.status.success(),
        "`git {}` failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout).expect("git prints UTF-8").trim().to_owned()
}
