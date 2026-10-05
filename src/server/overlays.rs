use std::fs::File;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use xxhash_rust::xxh3::Xxh3;
use xxhash_rust::xxh3::xxh3_64;

use mago_analyzer::external::protocol::ANALYZER_PROTOCOL_MAJOR;
use mago_analyzer::external::protocol::ANALYZER_PROTOCOL_MINOR;
use mago_build_id::BUILD_ID;

use crate::error::Error;
use crate::server::fingerprint::Fingerprint;
use crate::server::vendor::VendorKey;

/// The key a persisted overlay is valid under: the build, the configuration fingerprint, the
/// vendor key, every PHP file the extension workers loaded with its content hash, and the analyzer
/// protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OverlayKey(pub u128);

/// A PHP file the extension workers loaded, relative to the workspace when it lies inside it, and
/// its content hash.
pub(crate) type WorkerFile = (PathBuf, u64);

pub(crate) fn key(fingerprint: Fingerprint, vendor: VendorKey, worker_files: &[WorkerFile]) -> OverlayKey {
    let mut hasher = Xxh3::new();
    hasher.update(&prefix(fingerprint, vendor).to_le_bytes());
    for (path, hash) in worker_files {
        let path = path.as_os_str().as_encoded_bytes();
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path);
        hasher.update(&hash.to_le_bytes());
    }
    hasher.update(&ANALYZER_PROTOCOL_MAJOR.to_le_bytes());
    hasher.update(&ANALYZER_PROTOCOL_MINOR.to_le_bytes());

    OverlayKey(hasher.digest128())
}

/// `loaded`, sorted, relative to `workspace` where it lies inside it, each with the hash of its
/// current contents. A file that no longer reads hashes as zero.
pub(crate) fn worker_files(workspace: &Path, loaded: &[PathBuf]) -> Vec<WorkerFile> {
    let mut files = loaded
        .iter()
        .map(|path| {
            let hash = std::fs::read(path).map(|contents| xxh3_64(&contents)).unwrap_or(0);
            (path.strip_prefix(workspace).unwrap_or(path).to_path_buf(), hash)
        })
        .collect::<Vec<_>>();
    files.sort_unstable();
    files
}

/// The overlay file of every worktree at `fingerprint` over `vendor`. The worker files complete
/// the key inside the file, because they are known only once workers have run.
pub(crate) fn path(cache: &Path, fingerprint: Fingerprint, vendor: VendorKey) -> PathBuf {
    cache.join("overlays").join(format!("{:032x}.bin", prefix(fingerprint, vendor)))
}

/// Writes `state` under `key` to `path`, through a temporary file renamed into place.
pub(crate) fn persist(path: &Path, key: OverlayKey, worker_files: &[WorkerFile], state: &[u8]) -> Result<(), Error> {
    let failed = |error: &dyn std::fmt::Display| Error::Server(format!("cannot persist {}: {error}", path.display()));
    let directory = path.parent().ok_or_else(|| failed(&"it has no parent folder"))?;
    std::fs::create_dir_all(directory).map_err(|error| failed(&error))?;

    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut writer = BufWriter::new(File::create(&temporary).map_err(|error| failed(&error))?);
    bincode::serde::encode_into_std_write((key.0, worker_files), &mut writer, bincode::config::standard())
        .map_err(|error| failed(&error))?;
    writer.write_all(state).map_err(|error| failed(&error))?;
    writer.into_inner().map_err(|error| failed(&error.error()))?.sync_all().map_err(|error| failed(&error))?;

    std::fs::rename(&temporary, path).map_err(|error| failed(&error))
}

/// The state persisted at `path` when its key still holds for `workspace`: every worker file it
/// lists, hashed again from disk, completes the same key. A file whose key no longer holds is
/// deleted.
pub(crate) fn load(path: &Path, workspace: &Path, fingerprint: Fingerprint, vendor: VendorKey) -> Option<Vec<u8>> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    let (stored, listed): (u128, Vec<WorkerFile>) =
        bincode::serde::decode_from_std_read(&mut reader, bincode::config::standard()).ok()?;

    let current = listed
        .iter()
        .map(|(file, _)| {
            let contents = std::fs::read(workspace.join(file));
            (file.clone(), contents.map(|contents| xxh3_64(&contents)).unwrap_or(0))
        })
        .collect::<Vec<_>>();
    if key(fingerprint, vendor, &current).0 != stored {
        tracing::info!("Discarding the overlay {}: an extension file changed since it was written.", path.display());
        let _ = std::fs::remove_file(path);

        return None;
    }

    let mut state = Vec::new();
    reader.read_to_end(&mut state).ok()?;
    Some(state)
}

fn prefix(fingerprint: Fingerprint, vendor: VendorKey) -> u128 {
    let mut hasher = Xxh3::new();
    hasher.update(&BUILD_ID.to_le_bytes());
    hasher.update(&fingerprint.0.to_le_bytes());
    hasher.update(&vendor.0.to_le_bytes());
    hasher.digest128()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn an_overlay_loads_until_a_worker_file_changes() {
        let workspace = tempfile::tempdir().unwrap();
        let extension = workspace.path().join("extension.php");
        std::fs::write(&extension, "<?php // one\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let (fingerprint, vendor) = (Fingerprint(1), VendorKey(2));
        let files = worker_files(workspace.path(), std::slice::from_ref(&extension));
        let path = path(cache.path(), fingerprint, vendor);

        persist(&path, key(fingerprint, vendor, &files), &files, b"state").unwrap();
        assert_eq!(files, vec![(PathBuf::from("extension.php"), xxh3_64(b"<?php // one\n"))]);
        assert_eq!(load(&path, workspace.path(), fingerprint, vendor).as_deref(), Some(b"state".as_slice()));

        std::fs::write(&extension, "<?php // two\n").unwrap();
        assert_eq!(load(&path, workspace.path(), fingerprint, vendor), None);
        assert!(!path.exists(), "a stale overlay is discarded");
    }
}
