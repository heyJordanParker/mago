use std::path::Path;
use std::path::PathBuf;

/// Hashes every Rust source under `crates/` and `src/`, every `Cargo.toml`, and `Cargo.lock` of the
/// workspace at `root`, in path order, followed by `toolchain`.
///
/// # Panics
///
/// Panics when a directory or file under `root` cannot be read.
#[must_use]
pub fn digest(root: &Path, toolchain: &str) -> u128 {
    let mut files = vec![root.join("Cargo.toml"), root.join("Cargo.lock")];
    collect(&root.join("crates"), &mut files);
    collect(&root.join("src"), &mut files);
    files.sort();

    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    for file in files {
        let Ok(contents) = std::fs::read(&file) else {
            continue;
        };

        let name = file.strip_prefix(root).unwrap_or(&file);
        hasher.update(format!("{}\0{}\0", name.display(), contents.len()).as_bytes());
        hasher.update(&contents);
    }

    hasher.update(toolchain.as_bytes());
    hasher.digest128()
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };

    for entry in entries {
        let path = entry.unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display())).path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "target") {
                collect(&path, files);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs")
            || path.file_name().is_some_and(|name| name == "Cargo.toml")
        {
            files.push(path);
        }
    }
}
