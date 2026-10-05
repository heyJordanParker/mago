use std::path::Path;
use std::path::PathBuf;

/// Hashes everything the workspace at `root` builds the binary from, followed by `toolchain`.
///
/// The inputs, hashed in path order, are every Rust source under `crates/` and `src/`, every
/// `Cargo.toml`, `Cargo.lock`, the root `build.rs`, and the prelude stubs under
/// `crates/prelude/assets/` that it embeds.
///
/// # Panics
///
/// Panics when a directory or file under `root` cannot be read.
#[must_use]
pub fn digest(root: &Path, toolchain: &str) -> u128 {
    let mut files = vec![root.join("Cargo.toml"), root.join("Cargo.lock"), root.join("build.rs")];
    collect(&root.join("crates"), "rs", &mut files);
    collect(&root.join("src"), "rs", &mut files);
    collect(&root.join("crates/prelude/assets"), "php", &mut files);
    files.sort();

    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    for file in files {
        let contents = std::fs::read(&file).unwrap_or_else(|error| panic!("cannot read {}: {error}", file.display()));

        let name = file.strip_prefix(root).unwrap_or(&file);
        hasher.update(format!("{}\0{}\0", name.display(), contents.len()).as_bytes());
        hasher.update(&contents);
    }

    hasher.update(toolchain.as_bytes());
    hasher.digest128()
}

/// Adds every file under `directory` with the extension `extension`, and every `Cargo.toml`.
fn collect(directory: &Path, extension: &str, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };

    for entry in entries {
        let path = entry.unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display())).path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "target") {
                collect(&path, extension, files);
            }
        } else if path.extension().is_some_and(|found| found == extension)
            || path.file_name().is_some_and(|name| name == "Cargo.toml")
        {
            files.push(path);
        }
    }
}
