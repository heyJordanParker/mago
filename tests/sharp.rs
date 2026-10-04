//! Runs `mago` on a project that mixes PHP# and PHP files.

use std::path::Path;
use std::process::Command;
use std::process::Output;

const REPORT: &str = include_str!("fixtures/sharp/Report.sharp");

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
fn lint_format_guard_and_fix_refuse_a_sharp_file_and_leave_it_unchanged() {
    let directory = workspace(REPORT);

    for (command, arguments) in
        [("lint", &[][..]), ("format", &["--dry-run"][..]), ("format", &[][..]), ("guard", &[][..]), ("fix", &[][..])]
    {
        let output = run(directory.path(), command, arguments);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(!output.status.success(), "{command} {arguments:?} succeeded: {stderr}");
        assert!(stderr.contains("not supported yet"), "{command} {arguments:?}: {stderr}");
        assert!(stderr.contains("src/Demo/Report.sharp"), "{command} {arguments:?}: {stderr}");
        assert_eq!(report(directory.path()), REPORT, "{command} {arguments:?} changed the file");
    }
}
