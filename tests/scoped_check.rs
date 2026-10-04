#![allow(clippy::expect_used, clippy::missing_panics_doc)]

//! Runs `mago analyze` on named paths of a project whose extension judges files against each other.

use std::path::Path;
use std::process::Command;
use std::process::Output;

use serde_json::Value;

mod common;

const FIRST: &str = "<?php\n\n// route: /home\nfunction first(): int { return 'text'; }\n";

const SECOND: &str = "<?php\n\n// route: /home\nfunction second(): int { return 2; }\n";

fn workspace(repository: &Path) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let worker = repository.join("composer/tests/Sdk/Fixtures/analyzer-server-worker.php");
    std::fs::create_dir_all(directory.path().join("src")).expect("src directory");
    std::fs::write(
        directory.path().join("mago.toml"),
        format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\n\n[extension-hosts.proof]\ncommand = [\"php\", {:?}]\nworkers = 1\n",
            worker.display().to_string()
        ),
    )
    .expect("mago.toml");
    std::fs::write(directory.path().join("src/First.php"), FIRST).expect("first file");
    std::fs::write(directory.path().join("src/Second.php"), SECOND).expect("second file");
    directory
}

fn analyze(workspace: &Path, paths: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "analyze", "--reporting-format", "json"])
        .args(paths)
        .current_dir(workspace)
        .output()
        .expect("mago runs")
}

fn issues(output: &Output) -> Vec<Value> {
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("{error}: {}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
    });
    report["issues"].as_array().expect("an issue list").clone()
}

fn primary_file(issue: &Value) -> Option<&str> {
    issue["annotations"]
        .as_array()?
        .iter()
        .find(|annotation| annotation["kind"] == "Primary")
        .and_then(|annotation| annotation["span"]["file_id"]["name"].as_str())
}

#[test]
fn a_scoped_check_reports_what_a_whole_check_reports_in_the_named_file() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scoped check test") {
        return;
    }

    let directory = workspace(repository);

    let whole = issues(&analyze(directory.path(), &[]));
    let scoped_output = analyze(directory.path(), &["src/Second.php"]);
    let scoped = issues(&scoped_output);

    assert!(
        scoped
            .iter()
            .any(|issue| issue["code"] == "server-proof/duplicate-route"
                && primary_file(issue) == Some("src/Second.php")),
        "the cross-file rule sees the file the check did not name: {scoped:#?}"
    );
    assert!(
        whole.iter().any(|issue| primary_file(issue) == Some("src/First.php")),
        "the whole check reports in the unnamed file: {whole:#?}"
    );

    let mut filtered = whole
        .iter()
        .filter(|issue| primary_file(issue).is_none_or(|file| file == "src/Second.php"))
        .collect::<Vec<_>>();
    let mut scoped = scoped.iter().collect::<Vec<_>>();
    filtered.sort_by_key(|issue| issue.to_string());
    scoped.sort_by_key(|issue| issue.to_string());
    assert_eq!(scoped, filtered);
    assert!(!scoped_output.status.success(), "the duplicate route fails the check");
}
