use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;

use rayon::prelude::*;
use walkdir::WalkDir;

use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::membership::WorkspaceMatcher;

use crate::error::Error;

/// What a file's inode, size, modification and change times were when it was last read. A file
/// whose stamp is unchanged is not read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    size: u64,
    modified: i64,
    modified_nanos: i64,
    changed: i64,
    changed_nanos: i64,
}

impl From<&Metadata> for Stamp {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified: metadata.mtime(),
            modified_nanos: metadata.mtime_nsec(),
            changed: metadata.ctime(),
            changed_nanos: metadata.ctime_nsec(),
        }
    }
}

fn stamp(path: &Path) -> Option<Stamp> {
    std::fs::metadata(path).ok().as_ref().map(Stamp::from)
}

/// What a sweep found on disk since the last one.
#[derive(Debug)]
pub(crate) enum Sweep {
    /// A file the workers or Composer read changed, so the workers restart and the worktree is
    /// analyzed again from scratch.
    Restart(String),
    /// The files the sweep added, updated or deleted in the database.
    Changed(Vec<FileId>),
}

/// The stamp of every database file read from disk, and of the restart set: the Composer files and
/// every PHP file the extension workers loaded. Paths the matcher refused stay refused, because
/// membership depends on the path alone.
#[derive(Debug)]
pub(crate) struct Index {
    workspace: PathBuf,
    matcher: WorkspaceMatcher,
    files: HashMap<PathBuf, (FileId, Option<Stamp>)>,
    ignored: HashSet<PathBuf>,
    restart: Vec<(PathBuf, Option<Stamp>)>,
}

impl Index {
    /// Stamps every file of `database` that came from disk. `matcher` admits the files a later
    /// sweep finds, as the loader would have.
    pub(crate) fn new(workspace: &Path, matcher: WorkspaceMatcher, database: &Database<'_>) -> Self {
        let files = database
            .files()
            .filter_map(|file| file.path.clone().map(|path| (path, file.id)))
            .collect::<Vec<_>>()
            .into_par_iter()
            .map(|(path, id)| {
                let stamp = stamp(&path);
                (path, (id, stamp))
            })
            .collect();
        let workspace = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());

        Self { workspace, matcher, files, ignored: HashSet::new(), restart: Vec::new() }
    }

    /// Stamps `paths` as the restart set, replacing the previous one.
    pub(crate) fn set_restart(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.restart = paths
            .into_iter()
            .map(|path| {
                let stamp = stamp(&path);
                (path, stamp)
            })
            .collect();
    }

    /// Brings `database` up to date with the disk: stats every file under the configured roots,
    /// reads only the files whose stamp changed, and admits new files through the matcher.
    pub(crate) fn sweep(&mut self, database: &mut Database<'static>) -> Result<Sweep, Error> {
        if let Some((path, _)) = self.restart.iter().find(|(path, stamp)| self::stamp(path) != *stamp) {
            return Ok(Sweep::Restart(format!("{} changed", path.display())));
        }

        let found = self.walk();
        let mut seen = HashSet::new();
        let mut changed = Vec::new();
        for (path, current) in found {
            match self.files.get_mut(&path) {
                Some((id, stamp)) => {
                    seen.insert(path.clone());
                    if *stamp != Some(current) {
                        *stamp = Some(current);
                        let contents = read(&path)?;
                        if database.get_ref(id).is_ok_and(|file| file.contents.as_ref() != contents.as_slice()) {
                            database.update(*id, Cow::Owned(contents));
                            changed.push(*id);
                        }
                    }
                }
                None => {
                    if self.ignored.contains(&path) {
                        continue;
                    }

                    let Some(file_type) = self.matcher.classify(&path) else {
                        self.ignored.insert(path);
                        continue;
                    };

                    let file = File::read(&self.workspace, &path, file_type)
                        .map_err(|error| Error::Server(format!("cannot read {}: {error}", path.display())))?;
                    let id = database.add(file);
                    seen.insert(path.clone());
                    self.files.insert(path, (id, Some(current)));
                    changed.push(id);
                }
            }
        }

        let deleted = self.files.keys().filter(|path| !seen.contains(*path)).cloned().collect::<Vec<_>>();
        for path in deleted {
            if let Some((id, _)) = self.files.remove(&path)
                && database.delete(id)
            {
                changed.push(id);
            }
        }

        Ok(Sweep::Changed(changed))
    }

    /// Every file under the configured roots, outside excluded folders, with its stamp.
    fn walk(&self) -> Vec<(PathBuf, Stamp)> {
        let mut roots = self.matcher.roots().map(Path::to_path_buf).collect::<Vec<_>>();
        roots.sort_unstable();
        roots.dedup_by(|inner, outer| inner.starts_with(outer));

        roots
            .into_par_iter()
            .flat_map_iter(|root| {
                WalkDir::new(root)
                    .follow_links(true)
                    .into_iter()
                    .filter_entry(|entry| !entry.file_type().is_dir() || !self.matcher.is_excluded(entry.path()))
                    .filter_map(Result::ok)
                    .filter(|entry| !entry.file_type().is_dir())
                    .filter_map(|entry| {
                        let stamp = Stamp::from(&entry.metadata().ok()?);
                        Some((entry.into_path(), stamp))
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|error| Error::Server(format!("cannot read {}: {error}", path.display())))
}
