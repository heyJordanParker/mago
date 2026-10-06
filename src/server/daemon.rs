use std::collections::HashMap;
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use mago_build_id::BUILD_ID;

use crate::error::Error;
use crate::server::discovery::Runtime;
use crate::server::fingerprint;
use crate::server::fingerprint::Fingerprint;
use crate::server::message;
use crate::server::message::Check;
use crate::server::message::Request;
use crate::server::message::Response;
use crate::server::message::Status;
use crate::server::message::WorktreeStatus;
use crate::server::vendor::VendorKey;
use crate::server::worktree::ColdStarts;
use crate::server::worktree::Job;
use crate::server::worktree::Siblings;
use crate::server::worktree::Worktree;

/// The daemon exits after this long without a request.
const IDLE_EXIT: Duration = Duration::from_secs(3 * 60 * 60);

/// The write end of the pipe the signal handler wakes the shutdown thread through.
static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

/// One daemon per build: every worktree its clients name, each behind its own queue and writer
/// thread.
struct Daemon {
    runtime: Runtime,
    worktrees: Mutex<HashMap<(PathBuf, Fingerprint), Worktree>>,
    cold_starts: ColdStarts,
    last_request: Mutex<Instant>,
    stopping: Mutex<()>,
}

/// Runs the daemon until `mago server stop`, `SIGTERM`, `SIGINT`, or three idle hours.
pub(crate) fn run(runtime: Runtime) -> Result<(), Error> {
    let listener = bind(&runtime)?;
    let daemon = Arc::new(Daemon {
        runtime,
        worktrees: Mutex::new(HashMap::new()),
        cold_starts: Arc::default(),
        last_request: Mutex::new(Instant::now()),
        stopping: Mutex::new(()),
    });
    daemon.write_state();
    tracing::info!("The analysis server {:032x} started as process {}.", BUILD_ID, std::process::id());

    watch_signals(Arc::clone(&daemon))?;
    let idle = Arc::clone(&daemon);
    std::thread::Builder::new()
        .name("mago-server-idle".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                if idle.stopping.try_lock().is_err() {
                    continue;
                }
                if lock(&idle.last_request).elapsed() >= IDLE_EXIT {
                    tracing::info!("Stopping after {IDLE_EXIT:?} without a request.");
                    idle.shutdown(None);
                }
                if !idle.runtime.socket().exists() {
                    tracing::info!(
                        "Stopping because {} is gone, so no client can reach this server.",
                        idle.runtime.socket().display()
                    );
                    idle.shutdown(None);
                }
            }
        })
        .map_err(|error| Error::Server(format!("cannot start the idle timer: {error}")))?;

    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };

        let daemon = Arc::clone(&daemon);
        let _ = std::thread::Builder::new().name("mago-server-client".to_string()).spawn(move || daemon.serve(stream));
    }

    Ok(())
}

/// Binds the socket, replacing a socket file no daemon answers on.
fn bind(runtime: &Runtime) -> Result<UnixListener, Error> {
    let socket = runtime.socket();
    if UnixStream::connect(&socket).is_ok() {
        return Err(Error::Server(format!("an analysis server already listens on {}", socket.display())));
    }

    let _ = std::fs::remove_file(&socket);
    UnixListener::bind(&socket)
        .map_err(|error| Error::Server(format!("cannot listen on {}: {error}", socket.display())))
}

/// Shuts the daemon down on `SIGTERM` and `SIGINT`, from whichever thread receives them.
fn watch_signals(daemon: Arc<Daemon>) -> Result<(), Error> {
    extern "C" fn wake(_: libc::c_int) {
        let pipe = SIGNAL_PIPE.load(Ordering::Relaxed);
        // SAFETY: write is async-signal-safe, and the buffer outlives the call.
        unsafe { libc::write(pipe, [1u8].as_ptr().cast(), 1) };
    }

    let mut pipe = [0; 2];
    // SAFETY: pipe writes two descriptors into the array it is given.
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } != 0 {
        return Err(Error::Server(format!("cannot create the signal pipe: {}", std::io::Error::last_os_error())));
    }
    SIGNAL_PIPE.store(pipe[1], Ordering::Relaxed);
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: `wake` only calls async-signal-safe functions.
        unsafe { libc::signal(signal, wake as *const () as libc::sighandler_t) };
    }

    std::thread::Builder::new()
        .name("mago-server-signals".to_string())
        .spawn(move || {
            // SAFETY: the read end of the pipe belongs to this thread alone from here on.
            let mut reader = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(pipe[0]) };
            let _ = reader.read(&mut [0u8]);
            tracing::info!("Stopping on a signal.");
            daemon.shutdown(None);
        })
        .map(|_| ())
        .map_err(|error| Error::Server(format!("cannot start the signal thread: {error}")))
}

impl Daemon {
    fn serve(self: Arc<Self>, mut stream: UnixStream) {
        let answer = |stream: &mut UnixStream, response: Response| {
            if let Err(error) = message::write(stream, &response) {
                tracing::warn!("Cannot answer a client: {error}");
            }
        };

        match message::read::<Request>(&mut stream) {
            Ok(Some(Request::Hello { protocol, build })) if protocol == message::PROTOCOL && build == BUILD_ID => {
                answer(&mut stream, Response::Ready);
            }
            Ok(Some(Request::Hello { protocol, build })) => {
                let reason = format!(
                    "the client speaks protocol {protocol} of build {build:032x}, the server speaks {} of build {BUILD_ID:032x}",
                    message::PROTOCOL
                );
                return answer(&mut stream, Response::Stale { reason });
            }
            Ok(_) => return answer(&mut stream, Response::Failed { message: "expected a hello".to_string() }),
            Err(error) => return tracing::warn!("Cannot read a client's hello: {error}"),
        }

        let request = match message::read::<Request>(&mut stream) {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(error) => return tracing::warn!("Cannot read a client's request: {error}"),
        };

        *lock(&self.last_request) = Instant::now();
        let response = match request {
            Request::Analyze(check) => {
                self.check(check, |paths, reply| Job::Analyze { paths, queued: Instant::now(), reply })
            }
            Request::Verify(check) => self.check(check, |_, reply| Job::Verify { reply }),
            Request::Status => Response::Status(self.status()),
            Request::Stop => self.shutdown(Some(stream)),
            Request::Hello { .. } => Response::Failed { message: "a connection says hello once".to_string() },
        };

        answer(&mut stream, response);
    }

    /// Routes `check` to its worktree's queue, registering the worktree on its first check, and
    /// waits for the answer.
    fn check(
        self: &Arc<Self>,
        check: Check,
        job: impl FnOnce(Vec<PathBuf>, mpsc::Sender<Response>) -> Job,
    ) -> Response {
        let (reply, answer) = mpsc::channel();
        let routed = self.worktree(check).and_then(|(worktree, paths)| worktree.submit(job(paths, reply)));
        if let Err(error) = routed {
            return Response::Failed { message: error.to_string() };
        }

        let response = answer
            .recv()
            .unwrap_or_else(|_| Response::Failed { message: "the worktree stopped before it answered".to_string() });
        self.write_state();

        response
    }

    fn worktree(self: &Arc<Self>, check: Check) -> Result<(Worktree, Vec<PathBuf>), Error> {
        let fingerprint = fingerprint::of(&check)?;
        let Check { mut configuration, config_file, stubs, paths } = check;
        configuration.config_file = config_file;
        configuration.normalize()?;
        let workspace = configuration.source.workspace.clone();

        let mut worktrees = lock(&self.worktrees);
        if let Some(worktree) = worktrees.get(&(workspace.clone(), fingerprint)) {
            return Ok((worktree.clone(), paths));
        }

        let siblings = self.siblings();
        let worktree = Worktree::spawn(
            self.runtime.clone(),
            *configuration,
            fingerprint,
            stubs,
            siblings,
            Arc::clone(&self.cold_starts),
        )?;
        worktrees.insert((workspace.clone(), fingerprint), worktree.clone());
        drop(worktrees);

        tracing::info!("Registered the worktree {}.", workspace.display());

        Ok((worktree, paths))
    }

    /// The overlay of a warm worktree elsewhere at the same fingerprint over the same vendored files.
    fn siblings(self: &Arc<Self>) -> Siblings {
        let daemon = Arc::downgrade(self);

        Arc::new(move |workspace: &Path, fingerprint: Fingerprint, vendor: VendorKey| {
            let daemon = daemon.upgrade()?;
            let sibling = lock(&daemon.worktrees)
                .iter()
                .find(|((other, other_fingerprint), worktree)| {
                    other != workspace && *other_fingerprint == fingerprint && worktree.vendor() == Some(vendor)
                })
                .map(|(_, worktree)| worktree.clone())?;

            let (reply, answer) = mpsc::channel();
            sibling.submit(Job::Encode { reply }).ok()?;
            let state = answer.recv().ok()??;
            tracing::info!("{}: starting from a sibling worktree's overlay.", workspace.display());

            Some(state)
        })
    }

    fn status(&self) -> Status {
        let worktrees = lock(&self.worktrees);

        Status {
            pid: std::process::id(),
            build: format!("{BUILD_ID:032x}"),
            worktrees: worktrees
                .iter()
                .map(|((workspace, _), worktree)| WorktreeStatus {
                    workspace: workspace.clone(),
                    warm: worktree.vendor().is_some(),
                })
                .collect(),
        }
    }

    fn write_state(&self) {
        match serde_json::to_vec_pretty(&self.status()) {
            Ok(state) => {
                if let Err(error) = std::fs::write(self.runtime.state(), state) {
                    tracing::warn!("Cannot write {}: {error}", self.runtime.state().display());
                }
            }
            Err(error) => tracing::warn!("Cannot encode the server state: {error}"),
        }
    }

    /// Removes the socket, so new clients start another server, persists every worktree's overlay,
    /// stops its workers, answers the client that asked for the stop, and exits. The first caller
    /// holds `stopping` until the process exits, so a second stop waits instead of exiting early.
    fn shutdown(&self, mut client: Option<UnixStream>) -> ! {
        let _stopping = lock(&self.stopping);
        let _ = std::fs::remove_file(self.runtime.socket());
        let worktrees = lock(&self.worktrees).drain().map(|(_, worktree)| worktree).collect::<Vec<_>>();
        let stopped = worktrees
            .iter()
            .filter_map(|worktree| {
                let (reply, stopped) = mpsc::channel();
                worktree.submit(Job::Stop { reply }).ok().map(|()| stopped)
            })
            .collect::<Vec<_>>();
        for stopped in stopped {
            let _ = stopped.recv();
        }

        let _ = std::fs::remove_file(self.runtime.state());
        let _ = std::fs::remove_file(self.runtime.progress());
        if let Some(client) = client.as_mut()
            && let Err(error) = message::write(client, &Response::Stopped)
        {
            tracing::warn!("Cannot answer the client that stopped the server: {error}");
        }
        tracing::info!("The analysis server stopped.");
        std::process::exit(0);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
