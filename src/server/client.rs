use std::fs::File;
use std::io::IsTerminal;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use mago_build_id::BUILD_ID;

use crate::error::Error;
use crate::server::discovery::Runtime;
use crate::server::message;
use crate::server::message::Request;
use crate::server::message::Response;

/// How long a client waits for a daemon it started to accept connections.
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// How many lines of `server.log` a client prints when the daemon fails it.
const LOG_TAIL: usize = 20;

/// The answer to one exchange, or the daemon closing the connection before it answered.
enum Exchange {
    Answered(Response),
    Lost(String),
}

/// Sends `request` to the daemon of this build, starting it under the lock when none listens.
/// When the daemon exits before it answers, starts it once more and asks once more, then fails
/// with the tail of `server.log`.
pub(crate) fn ask(runtime: &Runtime, request: &Request) -> Result<Response, Error> {
    let waiting = show_progress(runtime);
    let first = exchange(runtime, request)?;
    let answer = match first {
        Exchange::Answered(response) => Ok(response),
        Exchange::Lost(reason) => {
            tracing::warn!("The analysis server closed the connection ({reason}); starting it again.");
            match exchange(runtime, request)? {
                Exchange::Answered(response) => Ok(response),
                Exchange::Lost(reason) => Err(Error::Server(format!(
                    "the analysis server closed the connection twice ({reason}). The end of {}:\n{}",
                    runtime.log().display(),
                    runtime.log_tail(LOG_TAIL)
                ))),
            }
        }
    };
    drop(waiting);

    answer
}

/// Sends `request` to a daemon that already listens, or returns `None` when none does.
pub(crate) fn ask_running(runtime: &Runtime, request: &Request) -> Result<Option<Response>, Error> {
    let Ok(mut stream) = UnixStream::connect(runtime.socket()) else {
        return Ok(None);
    };

    match send(&mut stream, request)? {
        Exchange::Answered(response) => Ok(Some(response)),
        Exchange::Lost(_) => Ok(None),
    }
}

/// Connects to the daemon of this build, starting it when none listens. Thirty racing clients
/// start one daemon: each takes `server.lock` and connects once more before it starts one.
pub(crate) fn connect(runtime: &Runtime) -> Result<UnixStream, Error> {
    if let Ok(stream) = UnixStream::connect(runtime.socket()) {
        return Ok(stream);
    }

    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(runtime.lock())
        .map_err(|error| Error::Server(format!("cannot open {}: {error}", runtime.lock().display())))?;
    // SAFETY: flock takes the descriptor of the open lock file, which outlives the call.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(Error::Server(format!("cannot lock {}: {error}", runtime.lock().display())));
    }

    if let Ok(stream) = UnixStream::connect(runtime.socket()) {
        return Ok(stream);
    }

    start(runtime)
}

fn exchange(runtime: &Runtime, request: &Request) -> Result<Exchange, Error> {
    let mut stream = connect(runtime)?;

    send(&mut stream, request)
}

fn send(stream: &mut UnixStream, request: &Request) -> Result<Exchange, Error> {
    let hello = Request::Hello { protocol: message::PROTOCOL, build: BUILD_ID };
    if let Err(error) = message::write(stream, &hello) {
        return Ok(Exchange::Lost(error.to_string()));
    }
    match message::read::<Response>(stream) {
        Ok(Some(Response::Ready)) => {}
        Ok(Some(Response::Stale { reason })) => {
            return Err(Error::Server(format!("the analysis server is stale: {reason}")));
        }
        Ok(Some(response)) => {
            return Err(Error::Server(format!("the analysis server answered a hello with {response:?}")));
        }
        Ok(None) => return Ok(Exchange::Lost("it closed the connection".to_string())),
        Err(error) => return Ok(Exchange::Lost(error.to_string())),
    }

    if let Err(error) = message::write(stream, request) {
        return Ok(Exchange::Lost(error.to_string()));
    }

    Ok(match message::read::<Response>(stream) {
        Ok(Some(response)) => Exchange::Answered(response),
        Ok(None) => Exchange::Lost("it closed the connection".to_string()),
        Err(error) => Exchange::Lost(error.to_string()),
    })
}

/// Starts `mago server start --foreground` in its own session, with no stdin and its output in
/// `server.log`, and waits until it accepts a connection.
fn start(runtime: &Runtime) -> Result<UnixStream, Error> {
    let failed = |error: &dyn std::fmt::Display| Error::Server(format!("cannot start the analysis server: {error}"));
    let log = File::options().create(true).append(true).open(runtime.log()).map_err(|error| failed(&error))?;
    let executable = std::env::current_exe().map_err(|error| failed(&error))?;

    let mut command = Command::new(executable);
    command
        .args(["--no-version-check", "--colors", "never", "server", "start", "--foreground"])
        .current_dir(runtime.directory())
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| failed(&error))?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe, so it may run between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut daemon = command.spawn().map_err(|error| failed(&error))?;

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(stream) = UnixStream::connect(runtime.socket()) {
            return Ok(stream);
        }

        if let Some(status) = daemon.try_wait().map_err(|error| failed(&error))? {
            return Err(failed(&format!(
                "it exited with {status}. The end of {}:\n{}",
                runtime.log().display(),
                runtime.log_tail(LOG_TAIL)
            )));
        }

        if Instant::now() >= deadline {
            return Err(failed(&format!("it did not accept connections within {START_TIMEOUT:?}")));
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

/// While the client waits on a terminal, prints what a cold start is doing, as the daemon records
/// it in `progress.json`. Stops when dropped.
fn show_progress(runtime: &Runtime) -> Option<mpsc::Sender<()>> {
    if !std::io::stderr().is_terminal() {
        return None;
    }

    let (stop, stopped) = mpsc::channel::<()>();
    let progress = runtime.progress();
    std::thread::spawn(move || {
        let mut shown = String::new();
        while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(Duration::from_secs(1)) {
            let Ok(current) = std::fs::read_to_string(&progress) else {
                continue;
            };
            if current != shown {
                let phase = serde_json::from_str::<serde_json::Value>(&current).ok();
                if let Some(phase) = phase.as_ref().and_then(|progress| progress["phase"].as_str()) {
                    eprintln!("Waiting for the analysis server: {phase}.");
                }
                shown = current;
            }
        }
    });

    Some(stop)
}
