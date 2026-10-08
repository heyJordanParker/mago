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

/// A bare call is the global function's, so the analyzer names the member to write when no function of that name
/// exists. On a line that holds a refusal, the refusal shows alone.
#[test]
fn analyze_names_the_member_a_bare_call_meant_on_a_line_without_a_refusal() {
    let directory = workspace(
        "namespace Demo;\n\nclass Report\n{\n    public static int total() => 1;\n\n    public static int run() => total();\n\n    public static int line() => total(__LINE__);\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        issues_in(&stdout, "src/Demo/Report.sharp"),
        ["9:39 semantics", "7:32 non-existent-function", "7:32 mixed-return-statement"],
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "Write `Report.total()`: a static method reaches the members of its class through the class name."
        ),
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

/// The standard library's `Text`, with a native body, spec section 29, and the `@` the library writes before it throws
/// its own exception.
const TEXT: &str = "namespace Sharp.Text;\n\npublic static class Text\n{\n    public static extern string slug(string title);\n\n    public static string quiet(string title) => @trim(title);\n\n    public static string shout(string title) => strtoupper(title);\n}\n";

/// The standard library's `Int`, `Float` and `Bool`, spec section 24, with the library's signatures. Their names are
/// reserved, and only the standard library declares them.
const TYPE_CLASSES: [(&str, &str); 3] = [
    (
        "Int",
        "namespace Sharp;\n\nimport ValueError;\n\npublic static class Int\n{\n    public static int parse(Any? value)\n    {\n        const number = Int.tryParse(value);\n        if (number is int) {\n            return number;\n        }\n        throw new ValueError(\"Sharp\\\\Int::parse(): Argument #1 ($value) must hold an int\");\n    }\n\n    public static int? tryParse(Any? value)\n    {\n        return (value is string text) ? filter_var(text, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE) : null;\n    }\n}\n",
    ),
    (
        "Float",
        "namespace Sharp;\n\nimport ValueError;\n\npublic static class Float\n{\n    public static float parse(Any? value)\n    {\n        const number = Float.tryParse(value);\n        if (number is float) {\n            return number;\n        }\n        throw new ValueError(\"Sharp\\\\Float::parse(): Argument #1 ($value) must hold a float\");\n    }\n\n    public static float? tryParse(Any? value)\n    {\n        const number = (value is string text) && is_numeric(text) ? floatval(text) : null;\n        return (number is float) && is_finite(number) ? number : null;\n    }\n}\n",
    ),
    (
        "Bool",
        "namespace Sharp;\n\npublic static class Bool\n{\n    public static bool? tryParse(Any? value) => filter_var(value, FILTER_VALIDATE_BOOLEAN, FILTER_NULL_ON_FAILURE);\n}\n",
    ),
];

/// The `composer.json` of the standard library's package.
const LIBRARY_PACKAGE: &str = "{\n    \"name\": \"heyjordanparker/php-sharp-composer\"\n}\n";

fn write(root: &Path, name: &str, contents: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// The standard library's package under `root`: its `composer.json`, its `Text`, and its `Int`, `Float` and `Bool`.
fn write_library(root: &Path) {
    write(root, "composer.json", LIBRARY_PACKAGE);
    write(root, "library/Sharp/Text/Text.sharp", TEXT);
    for (class, source) in TYPE_CLASSES {
        write(root, &format!("library/Sharp/{class}.sharp"), source);
    }
}

/// A project whose `vendor/` holds the standard library's package, beside the project's `src/App/Page.sharp`.
fn library_workspace(page: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    write(
        directory.path(),
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\nincludes = [\"vendor\"]\n",
    );
    write(directory.path(), "composer.json", "{\n    \"name\": \"acme/app\"\n}\n");
    write_library(&directory.path().join("vendor/heyjordanparker/php-sharp-composer"));
    write(directory.path(), "src/App/Page.sharp", page);
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
fn analyze_finds_no_issues_in_a_call_of_an_extern_method_of_the_standard_library() {
    let directory = library_workspace(
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public string slug() => Text.slug(\"Hello\");\n}\n",
    );

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

/// `mago compile` checks the vendored library as a project file, and it stays the standard library, so its `extern`
/// method and its `@` compile.
#[test]
fn compile_accepts_extern_methods_and_silence_in_the_vendored_standard_library() {
    let directory = library_workspace(
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public string slug() => Text.slug(\"Hello\");\n}\n",
    );

    let output = run(directory.path(), "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "{printed}");
    for compiled in ["src/App/Page.sharpc", "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/Text.sharpc"]
    {
        assert!(directory.path().join(".sharp").join(compiled).is_file(), "{compiled}: {printed}");
    }
}

/// `mago compile` lowers the vendored library first, and a project file runs the library's one-call method as its
/// body: `Text.shout(name)` compiles to `strtoupper($name)`.
#[test]
fn compile_inlines_a_vendored_library_form_into_a_project_caller() {
    let directory = library_workspace(
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public string title(string name) => Text.shout(name);\n}\n",
    );

    let output = run(directory.path(), "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "{printed}");
    let page = std::fs::read(directory.path().join(".sharp/src/App/Page.sharpc")).unwrap();
    assert!(page.windows(b"strtoupper".len()).any(|window| window == b"strtoupper"), "the form is inlined");
}

/// In the standard library's own repository, whose root `composer.json` names the package, `library/Sharp/` is the
/// library: its `extern` method and its `@` compile, and a file of another package in the same repository inlines its
/// one-call method. That package is still a project, so its own `extern` method under `Sharp` is refused.
#[test]
fn compile_treats_the_repository_of_the_standard_library_as_the_library() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n");
    write_library(root);
    write(root, "example/composer.json", "{\n    \"name\": \"acme/example\"\n}\n");
    write(
        root,
        "example/App/Title.sharp",
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Title\n{\n    public string of(string name) => Text.shout(name);\n}\n",
    );
    write(
        root,
        "example/App/Native.sharp",
        "namespace Sharp.Example;\n\npublic static class Native\n{\n    public static extern string run(string value);\n}\n",
    );

    let output = run(root, "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert_eq!(output.status.code(), Some(1), "{printed}");
    assert!(printed.contains("Compiled 5 PHP# files into .sharp/. 1 PHP# file was refused"), "{printed}");
    assert!(root.join(".sharp/library/Sharp/Text/Text.sharpc").is_file(), "{printed}");
    let title = std::fs::read(root.join(".sharp/example/App/Title.sharpc")).unwrap();
    assert!(title.windows(b"strtoupper".len()).any(|window| window == b"strtoupper"), "the form is inlined");
    assert!(!root.join(".sharp/example/App/Native.sharpc").exists(), "{printed}");
    assert!(printed.contains("native-body-outside-library"), "{printed}");
    assert!(!printed.contains("silence-outside-library"), "{printed}");
}

/// Without a `composer.json` that names the package, `library/Sharp/` is project code: its `extern` method and its `@`
/// are refused.
#[test]
fn compile_refuses_extern_and_silence_under_library_sharp_without_the_composer_json_of_the_library() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n");
    write(root, "library/Sharp/Text/Text.sharp", TEXT);

    let output = run(root, "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert_eq!(output.status.code(), Some(1), "{printed}");
    assert!(!root.join(".sharp/library/Sharp/Text/Text.sharpc").exists(), "{printed}");
    assert!(printed.contains("native-body-outside-library"), "{printed}");
    assert!(printed.contains("silence-outside-library"), "{printed}");
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

/// Only the standard library declares native bodies, so a project file declares no `extern` method, whatever its
/// namespace.
#[test]
fn analyze_reports_an_extern_method_in_a_project_file() {
    for namespace in ["App", "Sharp.Mine"] {
        let page = format!(
            "namespace {namespace};\n\npublic static class Page\n{{\n    public static extern string slug(string title);\n}}\n"
        );

        assert_eq!(
            page_errors(&page),
            [
                "src/App/Page.sharp:5:33:error - native-body-outside-library: Only the standard library declares native bodies: give `slug` a body."
            ],
            "{namespace}"
        );
    }
}

/// A project file under `Sharp` passes the semantic checks, which know only the namespace, and the analyzer refuses
/// both forms only the standard library may write.
#[test]
fn analyze_reports_extern_and_silence_in_a_project_file_under_sharp() {
    assert_eq!(
        page_errors(
            "namespace Sharp.Mine;\n\npublic static class Page\n{\n    public static extern string slug(string title);\n\n    public static string quiet(string title) => @trim(title);\n}\n"
        ),
        [
            "src/App/Page.sharp:5:33:error - native-body-outside-library: Only the standard library declares native bodies: give `slug` a body.",
            "src/App/Page.sharp:7:49:error - silence-outside-library: `@` hides PHP's warnings, and only the standard library uses it: handle the failure where it happens.",
        ]
    );
}

/// A project calls the standard library's `Int`, `Float` and `Bool` by their bare names, with no import, spec
/// section 24. The checker reads them from the vendored package and gives each call the library's return type.
#[test]
fn analyze_types_the_parse_calls_of_the_standard_library_from_the_vendored_package() {
    let directory = library_workspace(
        "namespace App;\n\npublic class Page\n{\n    public int count() => Int.parse(\"1\");\n\n    public float? price() => Float.tryParse(\"x\");\n\n    public bool? flag() => Bool.tryParse(\"yes\");\n}\n",
    );

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");

    assert_eq!(
        page_errors(
            "namespace App;\n\npublic class Page\n{\n    public string count() => Int.parse(\"1\");\n\n    public string price() => Float.tryParse(\"x\");\n\n    public string flag() => Bool.tryParse(\"yes\");\n}\n"
        ),
        [
            "src/App/Page.sharp:5:30:error - invalid-return-statement: Invalid return type for function `App\\Page::count`: expected `string`, but found `int`.",
            "src/App/Page.sharp:7:30:error - nullable-return-statement: Function `App\\Page::price` is declared to return `string` but possibly returns a nullable value (inferred as `float|null`).",
            "src/App/Page.sharp:7:30:error - invalid-return-statement: Invalid return type for function `App\\Page::price`: expected `string`, but found `float|null`.",
            "src/App/Page.sharp:9:29:error - nullable-return-statement: Function `App\\Page::flag` is declared to return `string` but possibly returns a nullable value (inferred as `bool|null`).",
            "src/App/Page.sharp:9:29:error - invalid-return-statement: Invalid return type for function `App\\Page::flag`: expected `string`, but found `bool|null`.",
        ]
    );
}

/// The semantic checks let a file under the namespace `Sharp` declare the standard library's reserved type names,
/// and the analyzer refuses them in a project file.
#[test]
fn analyze_reports_a_type_class_of_the_standard_library_in_a_project_file() {
    assert_eq!(
        page_errors(
            "namespace Sharp;\n\npublic static class Bool\n{\n    public static bool? tryParse(Any? value) => null;\n}\n"
        ),
        [
            "src/App/Page.sharp:3:21:error - reserved-name-outside-library: Cannot use `Bool` as a class name: it is reserved."
        ]
    );
}

/// The standard library's own repository analyzes its sources as project code, and its root `composer.json` names
/// the package, so its `extern` methods, `@`, and its `Int`, `Float` and `Bool` are the library's.
#[test]
fn analyze_finds_no_issues_in_the_repository_of_the_standard_library() {
    let directory = tempfile::tempdir().unwrap();
    write(directory.path(), "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"library\"]\n");
    write_library(directory.path());

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

/// `mago analyze` reports nothing in vendored files, so another package's `extern` method under `Sharp` gets no
/// report. The engine registers no native function for it, so a call of it fails when it runs.
#[test]
fn analyze_trusts_a_vendored_file_of_another_package_under_sharp() {
    let directory = library_workspace(
        "namespace App;\n\nimport Sharp.Tools.Tools;\n\npublic class Page\n{\n    public string slug() => Tools.slug(\"Hello\");\n}\n",
    );
    write(directory.path(), "vendor/acme/tools/composer.json", "{\n    \"name\": \"acme/tools\"\n}\n");
    write(
        directory.path(),
        "vendor/acme/tools/src/Sharp/Tools/Tools.sharp",
        "namespace Sharp.Tools;\n\npublic static class Tools\n{\n    public static extern string slug(string title);\n}\n",
    );

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
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

#[test]
fn a_reporting_format_flag_wins_over_mago_reporting_format() {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(""), false);

    let output = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "analyze", "--reporting-format", "emacs"])
        .env("MAGO_REPORTING_FORMAT", "json")
        .current_dir(directory.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("src/Demo/Broken.sharp:8:") && line.contains("invalid-return-statement")),
        "{stdout}"
    );
}

#[test]
fn compile_and_analyze_stop_on_a_mago_reporting_format_that_names_no_format() {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(""), false);

    for command in ["compile", "analyze"] {
        let output = Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", command])
            .env("MAGO_REPORTING_FORMAT", "jsno")
            .current_dir(directory.path())
            .output()
            .unwrap();
        let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

        assert!(!output.status.success(), "{command}: {printed}");
        assert!(
            printed.contains("Invalid value for environment variable `MAGO_REPORTING_FORMAT`"),
            "{command}: {printed}"
        );
        assert!(!directory.path().join(".sharp").exists(), "{command}");
    }
}
