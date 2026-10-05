use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;
use std::time::Instant;

use clap::ColorChoice;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_database::DatabaseConfiguration;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_database::membership::WorkspaceMatcher;
use mago_extension::WorkerPool;
use mago_prelude::Prelude;
use mago_reporting::IssueCollection;
use mago_server::Server;
use mago_server::Settings as ServerSettings;

use crate::commands::analyze::analyze_the_whole_workspace_without_paths;
use crate::commands::analyze::decode_prelude;
use crate::commands::analyze::server_settings;
use crate::config::Configuration;
use crate::consts::PRELUDE_BYTES;
use crate::error::Error;
use crate::extensions::initialize_external_analyzer;
use crate::server::discovery::Runtime;
use crate::server::fingerprint::Fingerprint;
use crate::server::message::Analyzed;
use crate::server::message::Response;
use crate::server::message::Verification;
use crate::server::overlays;
use crate::server::sync::Index;
use crate::server::sync::Sweep;
use crate::server::vendor;
use crate::server::vendor::VendorKey;
use crate::utils::create_orchestrator;

/// How long a worktree waits after its last request before it persists its overlay.
const PERSIST_AFTER: Duration = Duration::from_secs(30);

/// The files whose change restarts the extension workers, besides the PHP files they loaded.
const COMPOSER_FILES: [&str; 2] = ["composer.json", "composer.lock"];

pub(crate) enum Job {
    Analyze { paths: Vec<PathBuf>, queued: Instant, reply: mpsc::Sender<Response> },
    Verify { reply: mpsc::Sender<Response> },
    Encode { reply: mpsc::Sender<Option<Vec<u8>>> },
    Stop { reply: mpsc::Sender<()> },
}

/// Finds the encoded overlay of another warm worktree at the same fingerprint over the same vendored
/// files, for a new worktree to start from.
pub(crate) type Siblings = Arc<dyn Fn(&Path, Fingerprint, VendorKey) -> Option<Vec<u8>> + Send + Sync>;

/// Admits at most this many cold starts at once; the others wait their turn.
pub(crate) type ColdStarts = Arc<(Mutex<usize>, std::sync::Condvar)>;

pub(crate) const MAXIMUM_COLD_STARTS: usize = 3;

/// The handle the daemon routes one worktree's requests through: its queue, and the vendor key of
/// its warm state, which siblings start from.
#[derive(Clone)]
pub(crate) struct Worktree {
    sender: mpsc::Sender<Job>,
    vendor: Arc<Mutex<Option<VendorKey>>>,
}

impl Worktree {
    /// Starts the writer thread that owns the worktree at `configuration`, registered on its first
    /// request.
    pub(crate) fn spawn(
        runtime: Runtime,
        configuration: Configuration,
        fingerprint: Fingerprint,
        colors: bool,
        stubs: bool,
        siblings: Siblings,
        cold_starts: ColdStarts,
    ) -> Result<Self, Error> {
        let (sender, receiver) = mpsc::channel();
        let vendor = Arc::new(Mutex::new(None));
        let writer = Writer {
            runtime,
            configuration,
            fingerprint,
            colors,
            stubs,
            siblings,
            cold_starts,
            vendor: Arc::clone(&vendor),
            warm: None,
        };

        std::thread::Builder::new()
            .name("mago-worktree".to_string())
            .spawn(move || writer.run(&receiver))
            .map_err(|error| Error::Server(format!("cannot start a worktree thread: {error}")))?;

        Ok(Self { sender, vendor })
    }

    /// Queues `job`, or fails when the writer thread is gone.
    pub(crate) fn submit(&self, job: Job) -> Result<(), Error> {
        self.sender.send(job).map_err(|_| Error::Server("the worktree's writer thread stopped".to_string()))
    }

    pub(crate) fn vendor(&self) -> Option<VendorKey> {
        *self.vendor.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The writer thread's state: one worktree's configuration, and its warm analysis once registered.
struct Writer {
    runtime: Runtime,
    configuration: Configuration,
    fingerprint: Fingerprint,
    colors: bool,
    stubs: bool,
    siblings: Siblings,
    cold_starts: ColdStarts,
    vendor: Arc<Mutex<Option<VendorKey>>>,
    warm: Option<Warm>,
}

/// A registered worktree: its analysis server, the stamps the sweep compares, and its workers.
struct Warm {
    server: Server,
    settings: ServerSettings,
    index: Index,
    pools: Vec<Arc<WorkerPool>>,
    vendor: VendorKey,
    pending: Vec<FileId>,
    dirty: bool,
}

impl Writer {
    fn run(mut self, receiver: &mpsc::Receiver<Job>) {
        loop {
            let dirty = self.warm.as_ref().is_some_and(|warm| warm.dirty);
            let first = if dirty {
                match receiver.recv_timeout(PERSIST_AFTER) {
                    Ok(job) => job,
                    Err(RecvTimeoutError::Timeout) => {
                        self.persist();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match receiver.recv() {
                    Ok(job) => job,
                    Err(_) => break,
                }
            };

            let mut analyses = Vec::new();
            let mut verifications = Vec::new();
            let mut encodings = Vec::new();
            let mut stops = Vec::new();
            for job in std::iter::once(first).chain(receiver.try_iter()) {
                match job {
                    Job::Analyze { paths, queued, reply } => analyses.push((paths, queued, reply)),
                    Job::Verify { reply } => verifications.push(reply),
                    Job::Encode { reply } => encodings.push(reply),
                    Job::Stop { reply } => stops.push(reply),
                }
            }

            if !analyses.is_empty() || !verifications.is_empty() {
                self.answer(analyses, verifications);
            }

            for reply in encodings {
                let _ = reply.send(self.warm.as_ref().and_then(|warm| warm.server.encode().ok()));
            }

            if !stops.is_empty() {
                self.persist();
                for reply in stops {
                    let _ = reply.send(());
                }

                return;
            }
        }

        self.persist();
    }

    /// Answers every waiting request from one sweep and one pass.
    fn answer(
        &mut self,
        analyses: Vec<(Vec<PathBuf>, Instant, mpsc::Sender<Response>)>,
        verifications: Vec<mpsc::Sender<Response>>,
    ) {
        let start = Instant::now();
        let waits = analyses.iter().map(|(_, queued, _)| start.duration_since(*queued)).max().unwrap_or_default();

        let issues = match self.pass() {
            Ok(issues) => issues,
            Err(error) => {
                tracing::error!(
                    "{}: the analysis failed, so its state is dropped: {error}",
                    self.workspace().display()
                );
                self.drop_warm();
                let _ = std::fs::remove_file(self.runtime.progress());
                let message = error.to_string();
                for reply in analyses.into_iter().map(|(_, _, reply)| reply).chain(verifications) {
                    let _ = reply.send(Response::Failed { message: message.clone() });
                }

                return;
            }
        };

        let requests = analyses.len() + verifications.len();
        for (paths, _, reply) in analyses {
            let _ = reply.send(
                self.analyzed(&issues, &paths).unwrap_or_else(|error| Response::Failed { message: error.to_string() }),
            );
        }

        for reply in verifications {
            let _ = reply
                .send(self.verify(&issues).unwrap_or_else(|error| Response::Failed { message: error.to_string() }));
        }

        tracing::info!(
            "{}: answered {requests} request(s) in {:?}, after waiting up to {waits:?} in the queue.",
            self.workspace().display(),
            start.elapsed(),
        );
    }

    /// Sweeps the worktree and analyzes what changed, registering it first when it is not warm.
    /// Returns every issue of the worktree.
    fn pass(&mut self) -> Result<IssueCollection, Error> {
        let mut cold_start = None;
        if self.warm.is_none() {
            cold_start = Some(ColdStart::wait(&self.cold_starts));
            self.register()?;
        }

        let sweep = match self.warm.as_mut() {
            Some(warm) => warm.index.sweep(warm.server.database_mut())?,
            None => return Err(Error::Server("the worktree did not register".to_string())),
        };
        match sweep {
            Sweep::Changed(changed) => {
                self.warm.iter_mut().for_each(|warm| warm.pending.extend(changed.iter().copied()))
            }
            Sweep::Restart(reason) => {
                tracing::info!("{}: restarting the extension workers: {reason}.", self.workspace().display());
                self.drop_warm();
                cold_start.get_or_insert_with(|| ColdStart::wait(&self.cold_starts));
                self.register()?;
            }
        }

        let workspace = self.configuration.source.workspace.as_path();
        let warm = self.warm.as_mut().ok_or_else(|| Error::Server("the worktree did not register".to_string()))?;
        let changed = std::mem::take(&mut warm.pending);
        let start = Instant::now();
        let issues = warm.server.analyze_incremental(&changed)?.issues;
        tracing::info!("{}: analyzed {} changed file(s) in {:?}.", workspace.display(), changed.len(), start.elapsed());

        let restart = restart_set(workspace, &warm.pools)?;
        warm.index.set_restart(restart);
        warm.dirty |= cold_start.is_some() || !changed.is_empty();
        if cold_start.is_some() {
            let _ = std::fs::remove_file(self.runtime.progress());
        }

        Ok(issues)
    }

    /// Loads the worktree, starts its extension workers after the protocol handshake, and restores
    /// its overlay from a sibling or from disk, else analyzes it from scratch on the next pass.
    fn register(&mut self) -> Result<(), Error> {
        refuse_inherited_environment(&self.configuration)?;
        let start = Instant::now();
        let workspace = self.configuration.source.workspace.clone();

        self.progress("starting the extension workers");
        let color_choice = if self.colors { ColorChoice::Always } else { ColorChoice::Never };
        let mut orchestrator = create_orchestrator(&self.configuration, color_choice, false, false, false);
        analyze_the_whole_workspace_without_paths(&mut orchestrator);
        orchestrator.add_exclude_patterns(self.configuration.analyzer.excludes.iter());
        let pools = match initialize_external_analyzer(
            &self.configuration.extension_hosts,
            self.configuration.php_version,
            self.configuration.threads,
            &self.configuration.analyzer.plugins,
            self.configuration.analyzer.disable_default_plugins,
        )
        .map_err(|error| Error::Server(error.to_string()))?
        {
            Some((analyzer, pools)) => {
                orchestrator.set_external_analyzer(analyzer);
                pools
            }
            None => Vec::new(),
        };

        self.progress("loading the worktree");
        let prelude_database = if self.stubs {
            Prelude::decode_database(PRELUDE_BYTES).map_err(|error| Error::Server(error.to_string()))?
        } else {
            Prelude::default().database
        };
        let database = orchestrator.load_database(&workspace, true, Some(prelude_database), None)?.into_static();
        let matcher = WorkspaceMatcher::from_configuration(&orchestrator.database_configuration(&workspace, true))?;
        let settings = server_settings(&orchestrator);
        let vendor = vendor::key(&database, self.configuration.php_version, settings.parser);
        let index = Index::new(&workspace, matcher, &database);

        self.progress("restoring the overlay");
        let prelude = self.prelude();
        let restored = (self.siblings)(&workspace, self.fingerprint, vendor)
            .or_else(|| overlays::load(&self.overlay(vendor)?, &workspace, self.fingerprint, vendor))
            .and_then(|state| {
                Server::restore(database.clone(), prelude, settings.clone(), &state)
                    .inspect_err(|error| tracing::warn!("{}: cannot restore the overlay: {error}", workspace.display()))
                    .ok()
            });
        let (server, pending, how) = match restored {
            Some(server) => {
                let all =
                    server.database().files().filter(|file| file.file_type != FileType::Builtin).map(|file| file.id);
                let pending = all.collect();
                (server, pending, "restored its overlay")
            }
            None => {
                self.progress("analyzing the whole worktree");
                (Server::new(database, prelude, settings.clone()), Vec::new(), "will analyze from scratch")
            }
        };

        tracing::info!("{}: registered in {:?} and {how}.", workspace.display(), start.elapsed());
        *self.vendor.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(vendor);
        self.warm = Some(Warm { server, settings, index, pools, vendor, pending, dirty: false });

        Ok(())
    }

    /// The issues `paths` report, or every issue for no path, with the files they annotate or edit.
    fn analyzed(&self, issues: &IssueCollection, paths: &[PathBuf]) -> Result<Response, Error> {
        let warm = self.warm.as_ref().ok_or_else(|| Error::Server("the worktree is not warm".to_string()))?;
        let issues = if paths.is_empty() {
            issues.clone()
        } else {
            let workspace = self.configuration.source.workspace.as_path();
            let paths = paths.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>();
            let extensions =
                self.configuration.source.extensions.iter().map(|extension| Cow::Borrowed(extension.as_bytes()));
            let scope = WorkspaceMatcher::from_configuration(&DatabaseConfiguration {
                workspace: Cow::Borrowed(workspace),
                paths: paths.iter().map(|path| Cow::Borrowed(path.as_bytes())).collect(),
                includes: Vec::new(),
                patches: Vec::new(),
                excludes: Vec::new(),
                extensions: extensions.collect(),
                glob: self.configuration.source.glob.to_database_settings(),
            })?;

            warm.server.issues_in(&scope)
        };

        let annotated = issues
            .iter()
            .flat_map(|issue| {
                issue.annotations.iter().map(|annotation| annotation.span.file_id).chain(issue.edits.keys().copied())
            })
            .collect::<HashSet<_>>();
        let database = warm.server.database();
        let files = annotated
            .into_iter()
            .filter_map(|id| database.get(&id).ok())
            .map(|file| File::new(file.name.clone(), file.file_type, file.path.clone(), file.contents.clone()))
            .collect();

        Ok(Response::Analyzed(Analyzed { issues, files }))
    }

    /// Compares `warm` with a fresh analysis of the worktree's current files.
    fn verify(&self, warm_issues: &IssueCollection) -> Result<Response, Error> {
        let warm = self.warm.as_ref().ok_or_else(|| Error::Server("the worktree is not warm".to_string()))?;
        let mut fresh = Server::new(warm.server.database().clone(), self.prelude(), warm.settings.clone());
        let fresh_issues = fresh.analyze()?.issues;

        let render = |issues: &IssueCollection| -> Result<Vec<String>, Error> {
            let mut rendered = issues.iter().map(serde_json::to_string).collect::<Result<Vec<_>, _>>()?;
            rendered.sort_unstable();
            Ok(rendered)
        };
        let (warm_rendered, fresh_rendered) = (render(warm_issues)?, render(&fresh_issues)?);

        Ok(Response::Verified(Verification {
            issues: fresh_rendered.len(),
            only_warm: difference(&warm_rendered, &fresh_rendered),
            only_fresh: difference(&fresh_rendered, &warm_rendered),
        }))
    }

    /// Writes the overlay when the warm state changed since it was last written.
    fn persist(&mut self) {
        let Some(warm) = self.warm.as_mut().filter(|warm| warm.dirty) else {
            return;
        };

        let workspace = self.configuration.source.workspace.as_path();
        let start = Instant::now();
        let written = loaded_files(&warm.pools).and_then(|loaded| {
            let worker_files = overlays::worker_files(workspace, &loaded);
            let key = overlays::key(self.fingerprint, warm.vendor, &worker_files);
            let path = overlays::path(&crate::server::discovery::cache_directory()?, self.fingerprint, warm.vendor);
            overlays::persist(&path, key, &worker_files, &warm.server.encode()?)?;
            Ok(path)
        });

        match written {
            Ok(path) => {
                warm.dirty = false;
                tracing::info!(
                    "{}: persisted its overlay to {} in {:?}.",
                    workspace.display(),
                    path.display(),
                    start.elapsed()
                );
            }
            Err(error) => tracing::error!("{}: cannot persist its overlay: {error}", workspace.display()),
        }
    }

    fn drop_warm(&mut self) {
        self.warm = None;
        *self.vendor.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn prelude(&self) -> fn() -> (CodebaseMetadata, SymbolReferences) {
        if self.stubs { decode_prelude } else { Default::default }
    }

    fn overlay(&self, vendor: VendorKey) -> Option<PathBuf> {
        let cache = crate::server::discovery::cache_directory().ok()?;
        Some(overlays::path(&cache, self.fingerprint, vendor))
    }

    fn workspace(&self) -> &Path {
        &self.configuration.source.workspace
    }

    /// Records what this worktree's registration is doing, for a client that waits on it.
    fn progress(&self, phase: &str) {
        let progress = serde_json::json!({ "workspace": self.workspace(), "phase": phase });
        let _ = std::fs::write(self.runtime.progress(), progress.to_string());
    }
}

/// D3: a daemon's workers serve many clients, so none of them may inherit one client's environment.
fn refuse_inherited_environment(configuration: &Configuration) -> Result<(), Error> {
    match configuration.extension_hosts.iter().find(|(_, host)| host.enabled && host.inherit_environment) {
        Some((name, _)) => Err(Error::Server(format!(
            "extension host \"{name}\" inherits the environment, which the analysis server cannot pass to its workers. \
             Set `inherit-environment = false` and list what the workers need under `environment`, or run `mago analyze --no-server`."
        ))),
        None => Ok(()),
    }
}

/// The Composer files and every PHP file the workers loaded.
fn restart_set(workspace: &Path, pools: &[Arc<WorkerPool>]) -> Result<Vec<PathBuf>, Error> {
    let composer = COMPOSER_FILES.iter().map(|file| workspace.join(file));

    Ok(composer.chain(loaded_files(pools)?).collect())
}

fn loaded_files(pools: &[Arc<WorkerPool>]) -> Result<Vec<PathBuf>, Error> {
    let mut files = Vec::new();
    for pool in pools {
        files.extend(pool.loaded_files().map_err(|error| Error::Server(error.to_string()))?);
    }
    files.sort_unstable();
    files.dedup();

    Ok(files)
}

/// The elements of the sorted `left` missing from the sorted `right`, counting duplicates.
fn difference(left: &[String], right: &[String]) -> Vec<String> {
    let mut missing = Vec::new();
    let mut right = right.iter().peekable();
    for item in left {
        while right.next_if(|candidate| *candidate < item).is_some() {}
        if right.next_if(|candidate| *candidate == item).is_none() {
            missing.push(item.clone());
        }
    }

    missing
}

/// One of the [`MAXIMUM_COLD_STARTS`] turns, held through a cold start's first pass and released
/// when dropped.
struct ColdStart(ColdStarts);

impl ColdStart {
    fn wait(cold_starts: &ColdStarts) -> Self {
        let (running, released) = &**cold_starts;
        let mut running = running.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while *running >= MAXIMUM_COLD_STARTS {
            running = released.wait(running).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *running += 1;

        Self(Arc::clone(cold_starts))
    }
}

impl Drop for ColdStart {
    fn drop(&mut self) {
        let (running, released) = &*self.0;
        *running.lock().unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
        released.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_difference_keeps_duplicates_one_side_has_more_of() {
        let strings = |items: &[&str]| items.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(difference(&strings(&["a", "b", "b", "c"]), &strings(&["b", "c", "d"])), strings(&["a", "b"]));
        assert_eq!(difference(&strings(&["b", "c", "d"]), &strings(&["a", "b", "b", "c"])), strings(&["d"]));
    }
}
