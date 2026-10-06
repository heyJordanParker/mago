use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::error::Error;
use crate::server::message::Check;

/// A digest of everything a worktree's analysis depends on in a check, except where the worktree
/// lies and which paths it reports: two worktrees of one repository at one configuration share a
/// fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Fingerprint(pub u128);

/// The fingerprint of `check`'s configuration, configuration file and analysis switches.
///
/// Paths inside the workspace are hashed relative to it. Object keys are hashed in sorted order, so
/// maps that serialize in hash order still digest the same in every process.
pub(crate) fn of(check: &Check) -> Result<Fingerprint, Error> {
    let workspace = check.configuration.source.workspace.to_string_lossy();
    let config_file = check.config_file.as_ref().map(|file| Value::String(file.to_string_lossy().into_owned()));
    let mut hasher = Xxh3::new();
    hash(&mut hasher, &serde_json::to_value(&check.configuration)?, &workspace);
    hash(&mut hasher, &config_file.unwrap_or(Value::Null), &workspace);
    hasher.update(&[u8::from(check.stubs)]);

    Ok(Fingerprint(hasher.digest128()))
}

fn hash(hasher: &mut Xxh3, value: &Value, workspace: &str) {
    match value {
        Value::Null => hasher.update(b"n"),
        Value::Bool(value) => hasher.update(&[b'b', u8::from(*value)]),
        Value::Number(number) => {
            hasher.update(b"#");
            hasher.update(number.to_string().as_bytes());
        }
        Value::String(text) => {
            let text = text.strip_prefix(workspace).unwrap_or(text);
            hasher.update(b"s");
            hasher.update(&(text.len() as u64).to_le_bytes());
            hasher.update(text.as_bytes());
        }
        Value::Array(items) => {
            hasher.update(b"[");
            hasher.update(&(items.len() as u64).to_le_bytes());
            items.iter().for_each(|item| hash(hasher, item, workspace));
        }
        Value::Object(entries) => {
            let mut keys = entries.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            hasher.update(b"{");
            hasher.update(&(keys.len() as u64).to_le_bytes());
            for key in keys {
                hash(hasher, &Value::String(key.clone()), workspace);
                hash(hasher, &entries[key], workspace);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use crate::config::Configuration;

    use super::*;

    fn check(workspace: &str, config_file: Option<&str>) -> Check {
        Check {
            configuration: Box::new(Configuration::from_workspace(PathBuf::from(workspace))),
            config_file: config_file.map(PathBuf::from),
            stubs: true,
            paths: Vec::new(),
        }
    }

    #[test]
    fn worktrees_at_one_configuration_share_a_fingerprint_and_a_setting_changes_it() {
        let fingerprint = of(&check("/worktrees/first", None)).unwrap();
        assert_eq!(fingerprint, of(&check("/worktrees/second", None)).unwrap());
        assert_eq!(
            of(&check("/worktrees/first", Some("/worktrees/first/mago.toml"))).unwrap(),
            of(&check("/worktrees/second", Some("/worktrees/second/mago.toml"))).unwrap()
        );

        let mut excluded = check("/worktrees/first", None);
        excluded.configuration.source.excludes.push("cache".to_string());
        let mut bare = check("/worktrees/first", None);
        bare.stubs = false;
        for changed in [excluded, bare, check("/worktrees/first", Some("/worktrees/first/mago.dist.toml"))] {
            assert_ne!(fingerprint, of(&changed).unwrap(), "{changed:?}");
        }
    }
}
