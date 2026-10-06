#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings as AnalyzerSettings;
use mago_database::Database;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;
use mago_php_version::PHPVersion;
use mago_reporting::IssueCollection;
use mago_server::Server;
use mago_server::Settings;
use mago_syntax::settings::ParserSettings;

mod common;

const USER: &str = "<?php\nfunction use_box(Box $box): int { return $box->value(); }\n";

const BOX: &str = "<?php\nfinal class Box { public function value(): int { return 1; } }\n";

fn database(files: &[(&'static str, &str)]) -> Database<'static> {
    let mut database = Database::new(common::database_configuration("/server-proof", vec![Cow::Borrowed(b"src")]));
    for (name, contents) in files {
        database.add(File::new(
            Cow::Borrowed(name.as_bytes()),
            FileType::Host,
            None,
            Cow::Owned(contents.as_bytes().to_vec()),
        ));
    }

    database
}

fn server(repository: &Path, database: Database<'static>) -> Server {
    let command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-server-worker.php"))
        .with_current_directory(repository);
    let pool = WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())
        .expect("server proof worker pool should start");
    let analyzer = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)
        .expect("server proof analyzer should initialize");
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));

    let settings = Settings {
        parser: ParserSettings::default(),
        analyzer: AnalyzerSettings::new(PHPVersion::PHP85),
        plugin_registry: Arc::new(registry),
        use_progress_bars: false,
    };

    Server::new(database, Default::default, settings)
}

fn codes(issues: &IssueCollection) -> BTreeMap<String, Vec<String>> {
    let mut codes = BTreeMap::<String, Vec<String>>::new();
    for issue in issues.iter() {
        codes.entry(issue.code.clone().unwrap_or_default()).or_default().push(issue.message.clone());
    }
    codes
}

#[test]
fn a_scan_only_server_reports_only_the_before_analysis_issues() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scan-only server test") {
        return;
    }

    let files = [
        ("src/marker.php", "<?php\nconst PROOF_BEFORE = 1;\n"),
        ("src/broken.php", "<?php\nfunction broken(): int { return 'text'; }\n"),
    ];

    let full = codes(&server(repository, database(&files)).analyze().expect("full analysis").issues);
    assert!(full.contains_key("invalid-return-statement"), "a full analysis analyzes files: {full:#?}");

    let scanned = codes(&server(repository, database(&files)).scan_only().analyze().expect("scan").issues);
    assert_eq!(scanned.keys().collect::<Vec<_>>(), ["server-proof/before"], "{scanned:#?}");
}

#[test]
fn a_failed_analysis_resets_the_server_so_the_next_answers_like_a_fresh_one() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the server reset test") {
        return;
    }

    let mut warm = server(
        repository,
        database(&[("src/user.php", USER), ("src/box.php", BOX), ("src/flag.php", "<?php\nconst PROOF_OK = 1;\n")]),
    );
    warm.analyze().expect("initial analysis");

    let flag = FileId::new(b"src/flag.php");
    warm.database_mut().update(flag, Cow::Borrowed(b"<?php\nconst PROOF_FAIL = 1;\n"));
    assert!(warm.analyze_incremental(&[flag]).is_err(), "the after-analysis hook fails the pass");

    let user = FileId::new(b"src/user.php");
    let edited = "<?php\nfunction use_box(Box $box): int { return $box->value() + 1; }\n";
    warm.database_mut().update(flag, Cow::Borrowed(b"<?php\nconst PROOF_OK = 1;\n"));
    warm.database_mut().update(user, Cow::Borrowed(edited.as_bytes()));
    let recovered = codes(&warm.analyze_incremental(&[flag, user]).expect("recovered analysis").issues);

    let fresh = codes(
        &server(
            repository,
            database(&[
                ("src/user.php", edited),
                ("src/box.php", BOX),
                ("src/flag.php", "<?php\nconst PROOF_OK = 1;\n"),
            ]),
        )
        .analyze()
        .expect("fresh analysis")
        .issues,
    );
    assert_eq!(recovered, fresh);
}

#[test]
fn every_pass_runs_a_fresh_after_analysis_hook() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the fresh hook test") {
        return;
    }

    let mut server = server(
        repository,
        database(&[
            ("src/counted.php", "<?php\nconst PROOF_CALLS = 1;\n"),
            ("src/body.php", "<?php\nfunction body(): int { return 1; }\n"),
        ]),
    );
    let calls = |issues: &IssueCollection| codes(issues).remove("server-proof/calls").unwrap_or_default();
    assert_eq!(calls(&server.analyze().expect("initial analysis").issues), ["After-analysis call 1 of this hook."]);

    let body = FileId::new(b"src/body.php");
    for value in 2..4 {
        let contents = format!("<?php\nfunction body(): int {{ return {value}; }}\n");
        server.database_mut().update(body, Cow::Owned(contents.into_bytes()));
        let warm = server.analyze_incremental(&[body]).expect("warm analysis").issues;
        assert_eq!(calls(&warm), ["After-analysis call 1 of this hook."], "pass {value}");
    }
}

#[test]
fn findings_an_after_analysis_hook_collects_in_an_array_start_empty_every_pass() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the fresh hook array test") {
        return;
    }

    let first = "<?php\n// route: /home\nfunction first(): int { return 1; }\n";
    let second_contents =
        |value: i32| format!("<?php\n// route: /home\nfunction second(): int {{ return {value}; }}\n");
    let mut server = server(repository, database(&[("src/first.php", first), ("src/second.php", &second_contents(1))]));
    let duplicates =
        |issues: &IssueCollection| codes(issues).remove("server-proof/duplicate-route").unwrap_or_default();
    let expected = ["Route `/home` is also declared in `src/first.php`."];
    assert_eq!(duplicates(&server.analyze().expect("initial analysis").issues), expected);

    let second = FileId::new(b"src/second.php");
    for value in 2..4 {
        server.database_mut().update(second, Cow::Owned(second_contents(value).into_bytes()));
        let warm = server.analyze_incremental(&[second]).expect("warm analysis").issues;
        assert_eq!(duplicates(&warm), expected, "pass {value}");
    }
}
