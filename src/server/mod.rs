//! The analysis server: one daemon per build keeps every worktree's analysis warm, and `mago
//! analyze` asks it over a Unix socket. The daemon owns the analysis and the client owns I/O:
//! reporting, baselines, fixes and the exit code stay in the command.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::error::Error;

mod client;
mod daemon;
mod discovery;
mod fingerprint;
mod message;
mod overlays;
mod sync;
mod vendor;
mod worktree;

pub(crate) use message::Analyzed;
pub(crate) use message::Check;
pub(crate) use message::Status;
pub(crate) use message::Verification;

use discovery::Runtime;
use message::Request;
use message::Response;

/// Analyzes `check`'s worktree through the daemon. When a named path changes while the daemon
/// answers, asks once more, so no answer comes from a file that changed during the check.
pub(crate) fn analyze(check: Check) -> Result<Analyzed, Error> {
    let runtime = Runtime::current()?;
    let named = check.paths.iter().map(|path| check.configuration.source.workspace.join(path)).collect::<Vec<_>>();
    let stamps = || named.iter().map(|path| stamp(path)).collect::<Vec<_>>();
    let before = stamps();

    let request = Request::Analyze(check);
    let answer = analyzed(client::ask(&runtime, &request)?)?;
    if stamps() == before {
        return Ok(answer);
    }

    analyzed(client::ask(&runtime, &request)?)
}

/// Compares the daemon's warm state of `check`'s worktree with a fresh analysis.
pub(crate) fn verify(check: Check) -> Result<Verification, Error> {
    match client::ask(&Runtime::current()?, &Request::Verify(check))? {
        Response::Verified(verification) => Ok(verification),
        response => Err(unexpected(response)),
    }
}

/// Starts the daemon of this build unless one already listens.
pub(crate) fn start() -> Result<(), Error> {
    client::connect(&Runtime::current()?).map(drop)
}

/// Runs the daemon of this build in this process.
pub(crate) fn run() -> Result<(), Error> {
    // SAFETY: ignoring SIGPIPE turns a write to a client that went away into an error.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };

    daemon::run(Runtime::current()?)
}

/// The status of the daemon of this build, or `None` when none runs.
pub(crate) fn status() -> Result<Option<Status>, Error> {
    match client::ask_running(&Runtime::current()?, &Request::Status)? {
        None => Ok(None),
        Some(Response::Status(status)) => Ok(Some(status)),
        Some(response) => Err(unexpected(response)),
    }
}

/// Stops the daemon of this build after it persists every overlay. Returns whether one ran.
pub(crate) fn stop() -> Result<bool, Error> {
    match client::ask_running(&Runtime::current()?, &Request::Stop)? {
        None => Ok(false),
        Some(Response::Stopped) => Ok(true),
        Some(response) => Err(unexpected(response)),
    }
}

fn analyzed(response: Response) -> Result<Analyzed, Error> {
    match response {
        Response::Analyzed(analyzed) => Ok(analyzed),
        response => Err(unexpected(response)),
    }
}

fn unexpected(response: Response) -> Error {
    match response {
        Response::Failed { message } => Error::Server(message),
        response => Error::Server(format!("the analysis server answered {response:?}")),
    }
}

fn stamp(path: &Path) -> Option<(u64, u64, i64, i64)> {
    let metadata = std::fs::metadata(path).ok()?;

    Some((metadata.ino(), metadata.size(), metadata.mtime(), metadata.mtime_nsec()))
}
