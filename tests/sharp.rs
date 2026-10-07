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

/// A PHP file the linter reports for its missing `declare(strict_types=1);` and the formatter rewrites.
const MESSY: &str = "<?php\n\nnamespace Lib;\n\nfunction  one( ): int {return 1;}\n";

fn valid_report() -> String {
    REPORT.replace("let label = \"one\";", "let label = 1;")
}

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
    let directory = workspace(&valid_report());

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

#[test]
fn analyze_reports_a_sharp_error_without_an_extensions_setting() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src/Demo")).unwrap();
    std::fs::create_dir_all(directory.path().join("src/Lib")).unwrap();
    std::fs::write(directory.path().join("src/Lib/Calc.php"), include_str!("fixtures/sharp/Calc.php")).unwrap();
    std::fs::write(
        directory.path().join("src/Demo/Report.sharp"),
        "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        return $extra + 1;\n    }\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "analyze", "--reporting-format", "emacs"])
        .env("HOME", directory.path())
        .env("XDG_CONFIG_HOME", directory.path())
        .current_dir(directory.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.lines().any(|line| line.starts_with("src/Demo/Report.sharp:7:16:error - semantics:")), "{stdout}");
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
    let directory = workspace(&valid_report());
    let reader = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Lib;\n\nfunction read(): ?int\n{\n    $box = Box::maybe();\n    return $box->value;\n}\n";
    std::fs::write(directory.path().join("src/Lib/Box.php"), BOX).unwrap();
    std::fs::write(directory.path().join("src/Lib/read.php"), reader).unwrap();

    let output = run(directory.path(), "analyze", &["--fix", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!stderr.contains("does not support PHP# files yet"), "{stdout}{stderr}");
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
    assert!(stderr.contains("`mago analyze --fix` does not support PHP# files yet: src/Demo/Report.sharp"), "{stderr}");
    assert_eq!(report(directory.path()), source);
}

/// The standard library's `Text`, with a native body, spec section 29.
const TEXT: &str =
    "namespace Sharp.Text;\n\npublic static class Text\n{\n    public static extern string slug(string title);\n}\n";

/// A project whose `library/` folder holds the standard library's `Text`, vendored as `vendor/` is, beside the
/// project's `src/App/Page.sharp`.
fn library_workspace(page: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("library/Sharp/Text")).unwrap();
    std::fs::create_dir_all(directory.path().join("src/App")).unwrap();
    std::fs::write(
        directory.path().join("mago.toml"),
        "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\nincludes = [\"library\"]\n",
    )
    .unwrap();
    std::fs::write(directory.path().join("library/Sharp/Text/Text.sharp"), TEXT).unwrap();
    std::fs::write(directory.path().join("src/App/Page.sharp"), page).unwrap();
    directory
}

/// Every line `mago analyze` reports in the project's `src/App/Page.sharp`.
fn page_errors(page: &str) -> Vec<String> {
    let directory = library_workspace(page);
    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("src/App/Page.sharp:"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn analyze_finds_no_issues_in_a_call_of_the_standard_librarys_extern_method() {
    let directory = library_workspace(
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public string slug() => Text.slug(\"Hello\");\n}\n",
    );

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

#[test]
fn analyze_reports_new_on_a_static_class() {
    assert_eq!(
        page_errors(
            "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public Text make() => new Text();\n}\n"
        ),
        [
            "src/App/Page.sharp:7:31:error - abstract-instantiation: `Text` is a static class, so it has no instances: call its members on the class."
        ]
    );
}

#[test]
fn analyze_reports_a_class_that_extends_a_static_class() {
    assert_eq!(
        page_errors("namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page : Text\n{\n}\n"),
        ["src/App/Page.sharp:5:21:error - extend-final-class: `Text` is a static class, so no class can extend it."]
    );
}

/// Semantics refuses an `extern` method outside the namespace `Sharp`, and the analyzer refuses one in a project file
/// under it, which semantics cannot tell from the standard library.
#[test]
fn analyze_reports_an_extern_method_in_a_project_file() {
    for (namespace, code) in [("App", "semantics"), ("Sharp.Mine", "native-body-outside-library")] {
        let page = format!(
            "namespace {namespace};\n\npublic static class Page\n{{\n    public static extern string slug(string title);\n}}\n"
        );

        assert_eq!(
            page_errors(&page),
            [format!(
                "src/App/Page.sharp:5:33:error - {code}: Only the standard library declares native bodies: give `slug` a body."
            )],
            "{namespace}"
        );
    }
}

#[test]
fn linting_one_sharp_file_is_refused() {
    let service = LintService::new(ReadDatabase::empty(), Settings::default(), ParserSettings::default(), false);
    let file = File::ephemeral(Cow::Borrowed(b"src/Demo/Report.sharp"), Cow::Borrowed(REPORT.as_bytes()));

    let result = service.lint_file(&file, LintMode::Full, None, true);

    assert!(matches!(result, Err(OrchestratorError::SharpNotSupported { tool: "lint", .. })), "{result:?}");
}

fn messy_workspace() -> tempfile::TempDir {
    let directory = workspace(&valid_report());
    std::fs::write(directory.path().join("src/Lib/messy.php"), MESSY).unwrap();
    directory
}

/// Runs `mago` on a workspace holding `Report.sharp` and `messy.php`, checks that the PHP# file is unchanged,
/// and returns the output with the contents of `messy.php` afterwards.
fn run_beside_messy_php(command: &str, arguments: &[&str]) -> (Output, String) {
    run_in_messy_workspace(&messy_workspace(), command, arguments)
}

fn run_in_messy_workspace(directory: &tempfile::TempDir, command: &str, arguments: &[&str]) -> (Output, String) {
    let output = run(directory.path(), command, arguments);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!stderr.contains("panicked"), "{command} {arguments:?}: {stderr}");
    assert_eq!(report(directory.path()), valid_report(), "{command} {arguments:?} changed the PHP# file");

    (output, std::fs::read_to_string(directory.path().join("src/Lib/messy.php")).unwrap())
}

#[test]
fn lint_reports_the_php_file_and_skips_the_sharp_file() {
    let (output, _) = run_beside_messy_php("lint", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.lines().any(|line| line.starts_with("src/Lib/messy.php:1:1:warning - strict-types:")), "{stdout}");
}

#[test]
fn lint_fix_fixes_the_php_file_and_skips_the_sharp_file() {
    let (output, messy) = run_beside_messy_php("lint", &["--fix", "--potentially-unsafe"]);

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(messy.contains("declare(strict_types=1);"), "{messy}");
}

#[test]
fn format_formats_the_php_file_and_skips_the_sharp_file() {
    let (output, messy) = run_beside_messy_php("fmt", &[]);

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(messy.contains("function one(): int"), "{messy}");
}

#[test]
fn guard_checks_the_php_file_and_skips_the_sharp_file() {
    let (output, _) = run_beside_messy_php("guard", &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("No files found to check with guard."), "{stderr}");
}

#[test]
fn fix_formats_the_php_file_and_skips_the_sharp_file() {
    let (output, messy) = run_beside_messy_php("fix", &[]);

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(messy.contains("function one(): int"), "{messy}");
}

#[test]
fn lint_and_format_skip_a_staged_sharp_file() {
    let directory = messy_workspace();
    for arguments in [&["init", "--quiet"][..], &["add", "src"][..]] {
        assert!(Command::new("git").args(arguments).current_dir(directory.path()).status().unwrap().success());
    }

    let (output, _) = run_in_messy_workspace(&directory, "lint", &["--staged", "--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.lines().any(|line| line.starts_with("src/Lib/messy.php:1:1:warning - strict-types:")), "{stdout}");

    let (output, _) = run_in_messy_workspace(&directory, "fmt", &["--staged"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("Formatted and re-staged 1 file(s)."), "{stderr}");
}

#[cfg(unix)]
#[test]
fn lint_format_and_guard_never_read_a_discovered_sharp_file() {
    use std::os::unix::fs::PermissionsExt;

    let directory = workspace(&valid_report());
    let report = directory.path().join("src/Demo/Report.sharp");
    std::fs::set_permissions(&report, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&report).is_ok() {
        return;
    }

    for (command, arguments) in [("lint", &[][..]), ("fmt", &["--check"][..]), ("guard", &[][..])] {
        let output = run(directory.path(), command, arguments);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(output.status.success(), "{command}: {stderr}");
        assert!(!stderr.contains("Report.sharp"), "{command}: {stderr}");
        assert!(!stderr.contains("ERROR"), "{command}: {stderr}");
    }
}

#[test]
fn lint_format_and_guard_refuse_a_sharp_file_named_on_the_command_line() {
    let directory = workspace(REPORT);

    for (command, arguments, message) in [
        ("lint", &["src/Demo/Report.sharp"][..], "`mago lint` does not support PHP# files yet"),
        ("lint", &["--fix", "src/Demo/Report.sharp"][..], "`mago lint` does not support PHP# files yet"),
        ("fmt", &["src/Demo/Report.sharp"][..], "`mago format` does not support PHP# files yet"),
        ("guard", &["src/Demo/Report.sharp"][..], "`mago guard` does not support PHP# files yet"),
    ] {
        let output = run(directory.path(), command, arguments);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(!output.status.success(), "{command} {arguments:?} succeeded: {stderr}");
        assert!(stderr.contains(&format!("{message}: src/Demo/Report.sharp")), "{command} {arguments:?}: {stderr}");
        assert_eq!(report(directory.path()), REPORT, "{command} {arguments:?} changed the file");
    }
}
