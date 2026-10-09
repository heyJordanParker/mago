//! The Lean runtime library and the `sharp-lean` runner, built once per Mago build in Mago's cache folder.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use mago_build_id::BUILD_ID;

/// The library's Lean version, which `.sharp/.lean/lean-toolchain` pins too.
pub(crate) const TOOLCHAIN: &str = include_str!("../lean/lean-toolchain");

/// The library's Lake package, as `crates/sharp-lean/lean/` holds it.
const PACKAGE: [(&str, &str); 6] = [
    ("lakefile.toml", include_str!("../lean/lakefile.toml")),
    ("lean-toolchain", TOOLCHAIN),
    ("Sharp/Lean/Int.lean", include_str!("../lean/Sharp/Lean/Int.lean")),
    ("Sharp/Lean/Law.lean", include_str!("../lean/Sharp/Lean/Law.lean")),
    ("Sharp/Lean/String.lean", include_str!("../lean/Sharp/Lean/String.lean")),
    ("Sharp/Lean/Runner.lean", include_str!("../lean/Sharp/Lean/Runner.lean")),
];

/// The runner executable in the library's build.
pub(crate) fn runner(library: &Path) -> PathBuf {
    library.join(".lake/build/bin").join(format!("sharp-lean{}", std::env::consts::EXE_SUFFIX))
}

/// Writes the library's package to `lean/<build id>` in Mago's cache folder and builds it with Lake, once per build ID.
/// Builds in one process take turns, builds in different processes run in their own staging folders, and the first
/// one renamed into place wins. A new build ID deletes every older build.
pub(crate) fn build() -> io::Result<PathBuf> {
    static BUILD: Mutex<()> = Mutex::new(());

    let builds = cache_root()?.join("lean");
    let library = builds.join(format!("{BUILD_ID:032x}"));
    let _build = BUILD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if runner(&library).is_file() {
        return Ok(library);
    }

    let staging = library.with_extension(std::process::id().to_string());
    for (path, source) in PACKAGE {
        let target = staging.join(path);
        fs::create_dir_all(target.parent().unwrap_or(&staging))?;
        fs::write(target, source)?;
    }

    let output = Command::new("lake").arg("build").current_dir(&staging).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "Lake could not build the Lean runtime library in {}:\n{}{}",
            staging.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    if fs::rename(&staging, &library).is_err() {
        fs::remove_dir_all(&staging)?;
    }

    for entry in fs::read_dir(&builds)? {
        let old = entry?.path();
        if old.extension().is_none() && old.is_dir() && old != library {
            fs::remove_dir_all(old)?;
        }
    }

    Ok(library)
}

/// Mago's cache folder: `$XDG_CACHE_HOME/mago`, else `~/Library/Caches/mago` on macOS, `%LOCALAPPDATA%\mago` on
/// Windows and `~/.cache/mago` elsewhere.
fn cache_root() -> io::Result<PathBuf> {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME").filter(|cache| !cache.is_empty()) {
        return Ok(PathBuf::from(cache).join("mago"));
    }
    if cfg!(windows)
        && let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|local| !local.is_empty())
    {
        return Ok(PathBuf::from(local).join("mago"));
    }

    let home = std::env::home_dir().ok_or_else(|| io::Error::other("Mago's cache folder needs a home folder"))?;

    Ok(if cfg!(target_os = "macos") { home.join("Library/Caches/mago") } else { home.join(".cache/mago") })
}
