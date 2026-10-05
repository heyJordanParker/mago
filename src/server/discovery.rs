use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::path::PathBuf;

use mago_build_id::BUILD_ID;

use crate::error::Error;

/// The runtime directory of the daemon this binary's build runs: one per `BUILD_ID`, so a rebuilt
/// binary reaches a daemon of its own build and never one it did not run.
#[derive(Debug, Clone)]
pub(crate) struct Runtime {
    directory: PathBuf,
}

impl Runtime {
    /// The runtime directory under `$XDG_RUNTIME_DIR/mago`, else the temporary directory's
    /// `mago-<uid>`, created with mode 0700.
    pub(crate) fn current() -> Result<Self, Error> {
        let base = match std::env::var_os("XDG_RUNTIME_DIR").filter(|directory| !directory.is_empty()) {
            Some(directory) => PathBuf::from(directory).join("mago"),
            // SAFETY: getuid has no preconditions and cannot fail.
            None => std::env::temp_dir().join(format!("mago-{}", unsafe { libc::getuid() })),
        };
        let directory = base.join(format!("{BUILD_ID:032x}"));

        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&directory).map_err(|error| {
            Error::Server(format!("cannot create the analysis server directory {}: {error}", directory.display()))
        })?;

        Ok(Self { directory })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn socket(&self) -> PathBuf {
        self.directory.join("server.sock")
    }

    pub(crate) fn lock(&self) -> PathBuf {
        self.directory.join("server.lock")
    }

    pub(crate) fn state(&self) -> PathBuf {
        self.directory.join("server.json")
    }

    pub(crate) fn log(&self) -> PathBuf {
        self.directory.join("server.log")
    }

    pub(crate) fn progress(&self) -> PathBuf {
        self.directory.join("progress.json")
    }

    /// The last `lines` lines of `server.log`.
    pub(crate) fn log_tail(&self, lines: usize) -> String {
        let log = std::fs::read_to_string(self.log()).unwrap_or_default();
        let tail = log.lines().rev().take(lines).collect::<Vec<_>>();

        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }
}

/// The folder overlays persist in: `$XDG_CACHE_HOME/mago`, else the platform's user cache folder.
pub(crate) fn cache_directory() -> Result<PathBuf, Error> {
    if let Some(directory) = std::env::var_os("XDG_CACHE_HOME").filter(|directory| !directory.is_empty()) {
        return Ok(PathBuf::from(directory).join("mago"));
    }

    let home = std::env::home_dir()
        .ok_or_else(|| Error::Server("cannot find the home folder for the analysis cache".to_string()))?;

    Ok(if cfg!(target_os = "macos") { home.join("Library/Caches/mago") } else { home.join(".cache/mago") })
}
