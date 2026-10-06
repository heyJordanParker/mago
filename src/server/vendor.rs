use rayon::prelude::*;
use xxhash_rust::xxh3::Xxh3;
use xxhash_rust::xxh3::xxh3_64;

use mago_build_id::BUILD_ID;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::file::FileType;
use mago_php_version::PHPVersion;
use mago_syntax::settings::ParserSettings;

/// A digest of the vendored files a worktree loads, by workspace-relative name and content, with
/// the build, PHP version and parser settings that read them. Worktrees with identical vendored
/// files share a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct VendorKey(pub u128);

pub(crate) fn key(database: &Database<'_>, php_version: PHPVersion, parser: ParserSettings) -> VendorKey {
    let mut files = database
        .files()
        .filter(|file| file.file_type == FileType::Vendored)
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|file| (file.name.clone(), xxh3_64(&file.contents)))
        .collect::<Vec<_>>();
    files.sort_unstable();

    let mut hasher = Xxh3::new();
    hasher.update(&BUILD_ID.to_le_bytes());
    hasher.update(php_version.to_string().as_bytes());
    hasher.update(format!("{parser:?}").as_bytes());
    for (name, hash) in files {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(&name);
        hasher.update(&hash.to_le_bytes());
    }

    VendorKey(hasher.digest128())
}
