//! Runs `mago` on a project that mixes PHP# and PHP files.

use std::borrow::Cow;
use std::path::Path;
use std::process::Command;
use std::process::Output;

use mago_database::ReadDatabase;
use mago_database::file::File;
use mago_linter::settings::Settings;
use mago_orchestrator::OrchestratorError;
use mago_orchestrator::service::lint::LintMode;
use mago_orchestrator::service::lint::LintService;
use mago_syntax::settings::ParserSettings;

const REPORT: &str = include_str!("fixtures/sharp/Report.sharp");

/// A PHP class whose property read on a possibly `null` value gets the analyzer's `?->` fix.
const BOX: &str = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Lib;\n\nfinal class Box\n{\n    public int $value = 0;\n\n    public static function maybe(): ?self\n    {\n        return null;\n    }\n}\n";

fn workspace(report: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src/Demo")).unwrap();
    std::fs::create_dir_all(directory.path().join("src/Lib")).unwrap();
    std::fs::write(directory.path().join("mago.toml"), include_str!("fixtures/sharp/mago.toml")).unwrap();
    std::fs::write(directory.path().join("src/Lib/Calc.php"), include_str!("fixtures/sharp/Calc.php")).unwrap();
    std::fs::write(directory.path().join("src/Demo/Report.sharp"), report).unwrap();
    directory
}

fn run(workspace: &Path, command: &str, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", command])
        .args(arguments)
        .env("MAGO_LOG", "info")
        .current_dir(workspace)
        .output()
        .unwrap()
}

fn report(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join("src/Demo/Report.sharp")).unwrap()
}

#[test]
fn analyze_reports_a_string_passed_to_a_php_int_parameter_at_the_sharp_position() {
    let directory = workspace(REPORT);

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert!(
        stdout.lines().any(|line| line.starts_with("src/Demo/Report.sharp:12:25:error - invalid-argument:")),
        "{stdout}"
    );
}

#[test]
fn analyze_finds_no_issues_once_the_argument_is_an_int() {
    let directory = workspace(&REPORT.replace("let label = \"one\";", "let label = 1;"));

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

#[test]
fn analyze_reports_only_the_scope_error_for_a_local_used_after_its_block_closes() {
    let directory = workspace(
        "namespace Demo;\n\nclass Report\n{\n    public static int total()\n    {\n        {\n            let inner = 1;\n        }\n        return inner;\n    }\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let errors: Vec<&str> = stdout.lines().filter(|line| line.starts_with("src/Demo/Report.sharp:")).collect();
    assert_eq!(errors.len(), 1, "{stdout}");
    assert!(errors[0].starts_with("src/Demo/Report.sharp:10:16:error"), "{stdout}");
}

#[test]
fn analyze_reports_only_the_parse_error_for_php_syntax() {
    let directory = workspace(
        "namespace Demo;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        return this?->total(extra);\n    }\n\n    public int total(int extra)\n    {\n        return extra;\n    }\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let errors: Vec<&str> = stdout.lines().filter(|line| line.starts_with("src/Demo/Report.sharp:")).collect();
    assert_eq!(errors.len(), 1, "{stdout}");
    assert!(errors[0].starts_with("src/Demo/Report.sharp:7:20:error - parse:"), "{stdout}");
}

/// A PHP sum and call chain 1,000 levels deep overflowed the stack of a debug `mago analyze`, and a PHP# sum 100,000
/// levels deep overflowed any build. Now the PHP file analyzes, and the PHP# file gets its nesting error.
#[test]
fn analyze_finishes_on_deeply_nested_files() {
    let directory = workspace(REPORT);
    let sum = |terms: usize| vec!["value"; terms];
    std::fs::write(
        directory.path().join("src/Lib/Deep.php"),
        format!(
            "<?php\n\nnamespace Lib;\n\nfinal class Deep\n{{\n    public function sum(int $value): int\n    {{\n        return ${};\n    }}\n\n    public function chain(): self\n    {{\n        return $this{};\n    }}\n}}\n",
            sum(1_000).join(" + $"),
            "->chain()".repeat(1_000)
        ),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("src/Demo/Deep.sharp"),
        format!(
            "namespace Demo;\n\nclass Deep\n{{\n    public int sum(int value)\n    {{\n        return {};\n    }}\n}}\n",
            sum(100_000).join(" + ")
        ),
    )
    .unwrap();

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.lines().any(|line| line.starts_with("src/Demo/Deep.sharp:7:16:error - parse:")), "{stdout}{stderr}");
    assert!(!stdout.lines().any(|line| line.starts_with("src/Lib/Deep.php:")), "{stdout}{stderr}");
}

#[test]
fn analyze_fix_runs_on_php_files_beside_a_valid_sharp_file() {
    let directory = workspace(&REPORT.replace("let label = \"one\";", "let label = 1;"));
    let reader = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Lib;\n\nfunction read(): ?int\n{\n    $box = Box::maybe();\n    return $box->value;\n}\n";
    std::fs::write(directory.path().join("src/Lib/Box.php"), BOX).unwrap();
    std::fs::write(directory.path().join("src/Lib/read.php"), reader).unwrap();

    let output = run(directory.path(), "analyze", &["--fix", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!stderr.contains("not supported yet"), "{stdout}{stderr}");
    assert!(stdout.contains("$box?->value"), "{stdout}{stderr}");
    assert_eq!(std::fs::read_to_string(directory.path().join("src/Lib/read.php")).unwrap(), reader);
}

#[test]
fn analyze_fix_refuses_a_fix_that_would_edit_a_sharp_file() {
    let source = "namespace Demo;\n\nimport Lib.Box;\n\nclass Report\n{\n    public static int total()\n    {\n        const box = Box.maybe();\n        return box.value;\n    }\n}\n";
    let directory = workspace(source);
    std::fs::write(directory.path().join("src/Lib/Box.php"), BOX).unwrap();

    let output = run(directory.path(), "analyze", &["--fix"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("not supported yet"), "{stderr}");
    assert!(stderr.contains("src/Demo/Report.sharp"), "{stderr}");
    assert_eq!(report(directory.path()), source);
}

#[test]
fn linting_one_sharp_file_is_refused() {
    let service = LintService::new(ReadDatabase::empty(), Settings::default(), ParserSettings::default(), false);
    let file = File::ephemeral(Cow::Borrowed(b"src/Demo/Report.sharp"), Cow::Borrowed(REPORT.as_bytes()));

    let result = service.lint_file(&file, LintMode::Full, None, true);

    assert!(matches!(result, Err(OrchestratorError::SharpNotSupported { tool: "lint", .. })), "{result:?}");
}

#[test]
fn lint_format_guard_and_fixes_refuse_a_sharp_file_and_leave_it_unchanged() {
    let directory = workspace(REPORT);

    for (command, arguments) in [
        ("lint", &[][..]),
        ("format", &["--dry-run"][..]),
        ("format", &[][..]),
        ("guard", &[][..]),
        ("fix", &[][..]),
        ("fix", &["--no-guard"][..]),
    ] {
        let output = run(directory.path(), command, arguments);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(!output.status.success(), "{command} {arguments:?} succeeded: {stderr}");
        assert!(stderr.contains("not supported yet"), "{command} {arguments:?}: {stderr}");
        assert!(stderr.contains("src/Demo/Report.sharp"), "{command} {arguments:?}: {stderr}");
        assert_eq!(report(directory.path()), REPORT, "{command} {arguments:?} changed the file");
    }
}
