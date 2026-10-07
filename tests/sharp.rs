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

/// The issues `mago analyze --reporting-format emacs` printed for `file`, as `line:column code`.
fn issues_in(stdout: &str, file: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix(file)?.strip_prefix(':'))
        .map(|line| {
            let mut parts = line.splitn(3, ':');
            let (row, column, rest) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
            let code = rest.split_once(" - ").map_or("", |(_, rest)| rest.split(':').next().unwrap());

            format!("{row}:{column} {code}")
        })
        .collect()
}

/// A semantic error stops a PHP# file from running, so the analyzer issues its refused code causes add nothing:
/// each refused line shows its refusal alone. The analyzer still refuses `exit` with a `string`, which the semantic
/// checks accept, and a PHP file keeps every issue.
#[test]
fn analyze_reports_only_the_refusal_on_each_refused_line() {
    let directory = workspace(
        "namespace Demo;\n\nclass Report\n{\n    public const int LIMIT = __Foo__.y;\n\n    public void host()\n    {\n        const host = _SERVER[\"HTTP_HOST\"];\n    }\n\n    public void count()\n    {\n        _GET[\"n\"]++;\n    }\n\n    public void fallback(string host = _SERVER[\"x\"])\n    {\n    }\n\n    public void code()\n    {\n        exit(_SERVER[\"code\"]);\n    }\n\n    public void file()\n    {\n        exit(__FILE__);\n    }\n\n    public void call()\n    {\n        _SERVER.read();\n    }\n\n    public void rows(int row)\n    {\n        const data = (array)row;\n    }\n\n    public void names()\n    {\n        const all = GLOBALS;\n        const env = _ENV;\n        const magic = __Something__;\n        const dollar = $_SERVER[\"HTTP_HOST\"];\n        const member = _SERVER.x;\n        __Foo__.bar();\n    }\n\n    public void reason(string reason)\n    {\n        exit(reason);\n    }\n\n    public void status(int code)\n    {\n        exit(code);\n    }\n\n    public void message()\n    {\n        exit(\"m\");\n    }\n\n    public void template()\n    {\n        exit(`m`);\n    }\n\n    public void parenthesized()\n    {\n        exit(((\"m\")));\n    }\n\n    public void wrapped()\n    {\n        exit(\n            _SERVER[\"code\"]\n        );\n    }\n}\n",
    );
    std::fs::write(
        directory.path().join("src/Demo/Twin.php"),
        "<?php\n\nnamespace Demo;\n\nclass Twin\n{\n    public const int LIMIT = __Foo__::y;\n\n    public function read(): void\n    {\n        $host = _SERVER[\"HTTP_HOST\"];\n        $all = GLOBALS;\n        $env = _ENV;\n        $magic = __Something__;\n        $dollar = $_SERVER[\"HTTP_HOST\"];\n        $member = _SERVER::x;\n        _SERVER::read();\n        __Foo__::bar();\n    }\n\n    public function reason(string $reason): void\n    {\n        exit($reason);\n    }\n}\n",
    )
    .unwrap();

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        issues_in(&stdout, "src/Demo/Report.sharp"),
        [
            "5:30 semantics",
            "9:22 semantics",
            "14:9 semantics",
            "17:40 semantics",
            "23:14 semantics",
            "28:14 semantics",
            "33:9 semantics",
            "38:22 semantics",
            "43:21 semantics",
            "44:21 semantics",
            "45:23 semantics",
            "46:24 semantics",
            "47:24 semantics",
            "48:9 semantics",
            "63:9 semantics",
            "68:9 semantics",
            "73:9 semantics",
            "79:13 semantics",
            "53:9 invalid-argument",
        ],
        "{stdout}"
    );
    assert_eq!(
        issues_in(&stdout, "src/Demo/Twin.php"),
        [
            "7:30 non-existent-class-like",
            "11:17 non-existent-constant",
            "11:9 mixed-assignment",
            "12:16 non-existent-constant",
            "12:9 mixed-assignment",
            "13:16 non-existent-constant",
            "13:9 mixed-assignment",
            "14:18 non-existent-constant",
            "14:9 mixed-assignment",
            "16:19 non-existent-class-like",
            "16:9 impossible-assignment",
            "17:18 non-existent-method",
            "18:18 non-existent-method",
        ],
        "{stdout}"
    );
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

/// A PHP# class whose method returns a string where it declares `int`, with `pragma` on the line before.
fn broken_sharp(pragma: &str) -> String {
    format!(
        "namespace Demo;\n\npublic class Broken\n{{\n    public int total()\n    {{\n        {pragma}\n        return \"one\";\n    }}\n}}\n"
    )
}

/// The PHP twin of [`broken_sharp`].
fn broken_php(pragma: &str) -> String {
    format!(
        "<?php\n\nnamespace Demo;\n\nfinal class Broken\n{{\n    public function total(): int\n    {{\n        {pragma}\n        return 'one';\n    }}\n}}\n"
    )
}

const EXPECT_PRAGMA: &str = "// @mago-expect analysis:invalid-return-statement";

/// A workspace with `src/Demo/{name}`, and with an analyzer `ignore` entry for `invalid-return-statement` when
/// `ignored`.
fn suppressed_workspace(name: &str, contents: &str, ignored: bool) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src/Demo")).unwrap();
    let ignore = if ignored { "\n[analyzer]\nignore = [\"invalid-return-statement\"]\n" } else { "" };
    std::fs::write(
        directory.path().join("mago.toml"),
        format!("php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\n{ignore}"),
    )
    .unwrap();
    std::fs::write(directory.path().join("src/Demo").join(name), contents).unwrap();
    directory
}

#[test]
fn analyze_reports_a_sharp_error_an_expect_pragma_targets_and_points_at_the_pragma() {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(EXPECT_PRAGMA), false);

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("src/Demo/Broken.sharp:8:16:error - invalid-return-statement:"), "{stdout}");
    assert!(
        stdout.contains(
            "src/Demo/Broken.sharp:7:12:warning - unsuppressible-error: An error can't be suppressed in PHP#."
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("unfulfilled-expect"), "{stdout}");
}

#[test]
fn a_pragma_still_hides_a_php_error() {
    let directory = suppressed_workspace("Broken.php", &broken_php(EXPECT_PRAGMA), false);

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(!stdout.contains("invalid-return-statement"), "{stdout}");
    assert!(!stdout.contains("unsuppressible-error"), "{stdout}");
}

#[test]
fn a_pragma_still_hides_a_sharp_warning() {
    let source = "namespace Demo;\n\npublic class Sure\n{\n    public int one()\n    {\n        // @mago-expect analysis:redundant-condition\n        if (true) {\n            return 1;\n        }\n        return 2;\n    }\n}\n";
    let directory = suppressed_workspace("Sure.sharp", source, false);

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(!stdout.contains("redundant-condition"), "{stdout}");
    assert!(!stdout.contains("unsuppressible-error"), "{stdout}");
}

#[test]
fn compile_refuses_a_sharp_file_whose_error_a_pragma_an_ignore_entry_or_a_baseline_entry_targets() {
    let pragma = suppressed_workspace("Broken.sharp", &broken_sharp(EXPECT_PRAGMA), false);
    let ignored = suppressed_workspace("Broken.sharp", &broken_sharp(""), true);
    let baselined = suppressed_workspace("Broken.sharp", &broken_sharp(""), false);
    std::fs::write(
        baselined.path().join("mago.toml"),
        "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\n\n[analyzer]\nbaseline = \"baseline.toml\"\n",
    )
    .unwrap();
    let generated = run(baselined.path(), "analyze", &["--generate-baseline"]);
    assert!(baselined.path().join("baseline.toml").is_file(), "{}", String::from_utf8_lossy(&generated.stderr));

    for (suppression, directory) in
        [("a pragma", &pragma), ("an ignore entry", &ignored), ("a baseline entry", &baselined)]
    {
        let output = run(directory.path(), "compile", &[]);
        let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

        assert_eq!(output.status.code(), Some(1), "{suppression}: {printed}");
        assert!(printed.contains("invalid-return-statement"), "{suppression}: {printed}");
        assert!(!directory.path().join(".sharp/src/Demo/Broken.sharpc").exists(), "{suppression}");
    }
}

#[test]
fn compile_and_analyze_print_json_under_mago_reporting_format_in_github_actions() {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(""), false);

    for command in ["compile", "analyze"] {
        let output = Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", command])
            .env("GITHUB_ACTIONS", "true")
            .env("MAGO_REPORTING_FORMAT", "json")
            .current_dir(directory.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let report: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{command}: {error}: {stdout}"));

        assert_eq!(report["issues"][0]["code"], "invalid-return-statement", "{command}: {stdout}");
        assert_eq!(
            report["issues"][0]["annotations"][0]["span"]["file_id"]["name"], "src/Demo/Broken.sharp",
            "{command}: {stdout}"
        );
    }
}
