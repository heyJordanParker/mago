#![allow(dead_code)]

use std::borrow::Cow;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use mago_database::DatabaseConfiguration;
use mago_database::GlobSettings;

pub fn php_sdk_is_available(repository: &std::path::Path, test: &str) -> bool {
    let available = repository.join("vendor/autoload.php").is_file()
        && Command::new("php").arg("--version").output().is_ok_and(|output| output.status.success());
    assert!(
        available || std::env::var_os("MAGO_REQUIRE_PHP_SDK_TESTS").is_none(),
        "PHP and vendor dependencies are required for {test}"
    );
    available
}

/// Runs `command` against an analysis server of its own, which stops once its runtime folder is
/// removed after the command returns. The folder lies under `/tmp`, so the socket path stays within
/// the Unix limit, and the cache folder cannot be created, so the server leaves no overlay behind.
pub fn output_with_own_server(command: &mut Command) -> std::io::Result<Output> {
    let runtime = tempfile::Builder::new().prefix("mago").tempdir_in("/tmp")?;
    command.env("XDG_RUNTIME_DIR", runtime.path()).env("XDG_CACHE_HOME", "/dev/null").output()
}

pub fn database_configuration(
    workspace: impl Into<PathBuf>,
    paths: Vec<Cow<'static, [u8]>>,
) -> DatabaseConfiguration<'static> {
    DatabaseConfiguration {
        workspace: Cow::Owned(workspace.into()),
        paths,
        includes: Vec::new(),
        patches: Vec::new(),
        excludes: Vec::new(),
        extensions: vec![Cow::Borrowed(b"php")],
        glob: GlobSettings::default(),
    }
}
