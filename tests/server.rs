#![cfg(unix)]
#![allow(clippy::expect_used, clippy::missing_panics_doc)]

//! Runs `mago analyze` through the analysis server and with `--no-server`, and checks that both
//! print the same report and exit with the same code after each change to the project.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

use mago_build_id::BUILD_ID;
use mago_extension::Frame;
use serde_json::Value;
use tempfile::TempDir;

mod common;

const FIRST: &str = "<?php\n\n// route: /home\nfunction first(): int { return 'text'; }\n";

const SECOND: &str = "<?php\n\n// route: /home\nfunction second(): int { return 2; }\n";

/// A worker whose node hook reports the message `extension/message.php` returns, and appends the
/// file it inspects to `node-hooks.log`.
const MESSAGE_WORKER: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\NodeAnalysisContext;
use Mago\Sdk\Analyzer\NodeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Reporting\Issue;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Worker;

require 'REPOSITORY/vendor/autoload.php';

final class MessagePlugin implements Plugin, NodeAnalysisHook
{
    public function __construct(private readonly string $message) {}

    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('message', 'Message', 'Reports the message of the extension.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerNodeAnalysisHook($this);
    }

    public function getTargets(): array
    {
        return [NodeKind::Function];
    }

    public function getRequirements(): array
    {
        return [];
    }

    public function analyze(NodeAnalysisContext $context): void
    {
        file_put_contents(dirname(__DIR__) . '/node-hooks.log', $context->analysis->file . "\n", FILE_APPEND);
        $context->report(Level::Warning, 'message', Issue::new($this->message, $context->node->span));
    }
}

(new Worker(new Extension(
    identifier: 'test/message',
    name: 'Message',
    version: '1.0.0',
    analyzerPlugins: [new MessagePlugin(require __DIR__ . '/message.php')],
)))->run();
"#;

/// A worker that runs the server proof fixture over a copy of the SDK's analyzer protocol that
/// speaks another minor version.
const OTHER_MINOR_WORKER: &str = r"<?php

declare(strict_types=1);

require 'REPOSITORY/vendor/autoload.php';

spl_autoload_register(static function (string $class): void {
    if ($class === 'Mago\Sdk\Internal\Analyzer\Protocol') {
        require __DIR__ . '/Protocol.php';
    }
}, true, true);

require 'REPOSITORY/composer/tests/Sdk/Fixtures/analyzer-server-worker.php';
";

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn php() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    std::env::split_paths(&path).map(|folder| folder.join("php")).find(|php| php.is_file()).expect("php on PATH")
}

/// One analysis server, reached through its own runtime folder, with its own overlay cache.
struct Server {
    runtime: TempDir,
    cache: TempDir,
}

impl Server {
    fn new() -> Self {
        let folder = || tempfile::Builder::new().prefix("mago").tempdir_in("/tmp").expect("server folder");
        Self { runtime: folder(), cache: folder() }
    }

    fn mago(&self, workspace: &Path, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mago"));
        command
            .args(["--no-version-check", "--colors", "never"])
            .args(arguments)
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("XDG_CACHE_HOME", self.cache.path())
            .env("MAGO_LOG", "info")
            .current_dir(workspace);
        command
    }

    fn analyze(&self, workspace: &Path, arguments: &[&str]) -> Output {
        self.mago(workspace, &[&["analyze", "--reporting-format", "emacs"], arguments].concat())
            .output()
            .expect("mago runs")
    }

    /// Checks `workspace` through the server and with `--no-server`, asserts both print the same
    /// report and exit with the same code, and returns the server's output.
    fn check(&self, workspace: &Path, arguments: &[&str]) -> Output {
        let served = self.analyze(workspace, arguments);
        let cold = self.analyze(workspace, &[arguments, &["--no-server"]].concat());

        assert_eq!(
            String::from_utf8_lossy(&served.stdout),
            String::from_utf8_lossy(&cold.stdout),
            "the server's report differs from a cold run's.\nserver stderr: {}\ncold stderr: {}\nserver log:\n{}",
            String::from_utf8_lossy(&served.stderr),
            String::from_utf8_lossy(&cold.stderr),
            self.log()
        );
        assert_eq!(served.status.code(), cold.status.code(), "{}", String::from_utf8_lossy(&served.stderr));

        served
    }

    fn folder(&self) -> PathBuf {
        let mago = self.runtime.path().join("mago");
        std::fs::read_dir(&mago)
            .expect("the server's runtime folder")
            .map(|entry| entry.expect("runtime entry").path())
            .find(|path| path.file_name().is_some_and(|name| name == format!("{BUILD_ID:032x}").as_str()))
            .expect("the runtime folder of this build")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.runtime.path().join("mago").join(format!("{BUILD_ID:032x}")).join("server.log"))
            .unwrap_or_default()
    }

    fn occurrences(&self, line: &str) -> usize {
        self.log().matches(line).count()
    }

    fn stop(&self, workspace: &Path) {
        let output = self.mago(workspace, &["server", "stop"]).output().expect("mago runs");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
}

fn configuration(worker: &Path, extra: &str) -> String {
    format!(
        "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\n{extra}\n[extension-hosts.proof]\ncommand = [{:?}, {:?}]\nworkers = 1\ninherit-environment = false\n",
        php().display().to_string(),
        worker.display().to_string(),
    )
}

/// A project of two files that declare the same route, checked by the server proof extension.
fn project(extra: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let worker = repository().join("composer/tests/Sdk/Fixtures/analyzer-server-worker.php");
    write(directory.path(), "mago.toml", &configuration(&worker, extra));
    write(directory.path(), "src/First.php", FIRST);
    write(directory.path(), "src/Second.php", SECOND);
    directory
}

/// A project whose extension reports the message in its own `extension/message.php`.
fn message_project() -> TempDir {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let worker = directory.path().join("extension/worker.php");
    write(directory.path(), "mago.toml", &configuration(&worker, ""));
    write(
        directory.path(),
        "extension/worker.php",
        &MESSAGE_WORKER.replace("REPOSITORY", &repository().display().to_string()),
    );
    write(directory.path(), "extension/message.php", "<?php\n\nreturn 'First message.';\n");
    write(directory.path(), "src/First.php", FIRST);
    directory
}

fn write(workspace: &Path, file: &str, contents: &str) {
    let path = workspace.join(file);
    std::fs::create_dir_all(path.parent().expect("a parent folder")).expect("parent folder");
    std::fs::write(path, contents).expect("file written");
}

fn git(workspace: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "user.name=Mago", "-c", "user.email=mago@example.com"])
        .args(arguments)
        .current_dir(workspace)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {arguments:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn available() -> bool {
    common::php_sdk_is_available(repository(), "the analysis server tests")
}

#[test]
fn edits_additions_deletions_and_renames_answer_like_a_cold_run() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    let workspace = workspace.path();
    server.check(workspace, &[]);

    write(workspace, "src/Second.php", &SECOND.replace("return 2;", "return 'two';"));
    server.check(workspace, &[]);
    server.check(workspace, &["src/Second.php"]);

    write(workspace, "src/Second.php", &SECOND.replace("(): int { return 2; }", "(): string { return 'two'; }"));
    write(workspace, "src/Caller.php", "<?php\n\nfunction caller(): int { return second(); }\n");
    server.check(workspace, &[]);

    write(workspace, "src/Third.php", "<?php\n\n// route: /home\nfunction third(): void {}\n");
    server.check(workspace, &["src/Third.php"]);

    std::fs::remove_file(workspace.join("src/First.php")).expect("first file removed");
    server.check(workspace, &[]);

    std::fs::rename(workspace.join("src/Second.php"), workspace.join("src/Renamed.php")).expect("second file renamed");
    let renamed = server.check(workspace, &[]);
    assert!(String::from_utf8_lossy(&renamed.stdout).contains("src/Renamed.php"), "{renamed:?}");
}

#[test]
fn a_branch_switch_answers_like_a_cold_run() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    let workspace = workspace.path();
    git(workspace, &["init", "--quiet", "--initial-branch", "main"]);
    git(workspace, &["add", "."]);
    git(workspace, &["commit", "--quiet", "--message", "main"]);
    git(workspace, &["checkout", "--quiet", "-b", "other"]);
    write(workspace, "src/Second.php", &SECOND.replace("/home", "/other"));
    write(workspace, "src/Third.php", "<?php\n\nfunction third(): int { return 'three'; }\n");
    std::fs::remove_file(workspace.join("src/First.php")).expect("first file removed");
    git(workspace, &["add", "--all"]);
    git(workspace, &["commit", "--quiet", "--message", "other"]);

    server.check(workspace, &[]);
    git(workspace, &["checkout", "--quiet", "main"]);
    server.check(workspace, &[]);
    git(workspace, &["checkout", "--quiet", "other"]);
    server.check(workspace, &[]);
}

#[test]
fn a_vendor_update_answers_like_a_cold_run() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("includes = [\"vendor\"]\n");
    let workspace = workspace.path();
    write(workspace, "vendor/acme/answer.php", "<?php\n\nnamespace Acme;\n\nfunction answer(): int { return 42; }\n");
    write(workspace, "src/Uses.php", "<?php\n\nfunction uses(): string { return \\Acme\\answer(); }\n");
    let before = server.check(workspace, &[]);

    write(
        workspace,
        "vendor/acme/answer.php",
        "<?php\n\nnamespace Acme;\n\nfunction answer(): string { return 'yes'; }\n",
    );
    let after = server.check(workspace, &[]);
    assert_ne!(before.stdout, after.stdout, "the vendor update changes the report");
}

#[test]
fn a_configuration_edit_answers_like_a_cold_run() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    let workspace = workspace.path();
    server.check(workspace, &[]);

    let configuration = std::fs::read_to_string(workspace.join("mago.toml")).expect("mago.toml");
    let excluded = configuration.replace("paths = [\"src\"]", "paths = [\"src\"]\nexcludes = [\"src/First.php\"]");
    write(workspace, "mago.toml", &excluded);
    let after = server.check(workspace, &[]);
    assert!(!String::from_utf8_lossy(&after.stdout).contains("src/First.php"), "{after:?}");
}

#[test]
fn an_extension_edit_restarts_the_workers() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = message_project();
    let workspace = workspace.path();
    let before = server.check(workspace, &[]);
    assert!(String::from_utf8_lossy(&before.stdout).contains("First message."), "{before:?}");

    write(workspace, "extension/message.php", "<?php\n\nreturn 'Second message.';\n");
    let after = server.check(workspace, &[]);
    assert!(String::from_utf8_lossy(&after.stdout).contains("Second message."), "{after:?}");
    assert_eq!(server.occurrences("restarting the extension workers"), 1, "{}", server.log());
}

#[test]
fn a_client_of_another_build_is_told_the_server_is_stale() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    server.check(workspace.path(), &[]);

    let mut stream = UnixStream::connect(server.folder().join("server.sock")).expect("the server listens");
    let hello = format!(r#"{{"Hello":{{"protocol":1,"build":{}}}}}"#, BUILD_ID ^ 1);
    Frame::request(0, hello.into_bytes()).write_to(&mut stream, usize::MAX).expect("hello sent");
    stream.flush().expect("hello flushed");
    let answer = Frame::read_from(&mut stream, usize::MAX).expect("an answer").expect("a frame");
    let answer: Value = serde_json::from_slice(&answer.payload).expect("a JSON answer");

    let reason = answer["Stale"]["reason"].as_str().expect("a stale answer");
    assert!(reason.contains(&format!("{:032x}", BUILD_ID ^ 1)), "{reason}");
    assert!(reason.contains(&format!("{BUILD_ID:032x}")), "{reason}");
}

#[test]
fn worktrees_answer_alone_on_one_server() {
    if !available() {
        return;
    }

    let server = Server::new();
    let first = project("includes = [\"vendor\"]\n");
    let second = project("includes = [\"vendor\"]\n");
    let third = project("includes = [\"vendor\"]\n");
    let vendor = "<?php\n\nnamespace Acme;\n\nfunction answer(): int { return 42; }\n";
    write(first.path(), "vendor/acme/answer.php", vendor);
    write(second.path(), "vendor/acme/answer.php", vendor);
    write(third.path(), "vendor/acme/answer.php", &vendor.replace("int { return 42; }", "string { return 'yes'; }"));
    for workspace in [&first, &second, &third] {
        write(workspace.path(), "src/Uses.php", "<?php\n\nfunction uses(): int { return \\Acme\\answer(); }\n");
        server.check(workspace.path(), &[]);
    }

    write(first.path(), "src/Second.php", &SECOND.replace("/home", "/first"));
    for workspace in [&first, &second, &third] {
        server.check(workspace.path(), &[]);
    }
    assert_eq!(server.occurrences("starting from a sibling worktree's overlay"), 1, "{}", server.log());
}

#[test]
fn thirty_concurrent_clients_start_one_server_and_get_one_answer() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    let workspace = workspace.path();
    let clients = (0..30)
        .map(|_| {
            server
                .mago(workspace, &["analyze", "--reporting-format", "emacs"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("mago runs")
        })
        .collect::<Vec<_>>();
    let outputs =
        clients.into_iter().map(|client| client.wait_with_output().expect("mago finishes")).collect::<Vec<_>>();

    let cold = server.analyze(workspace, &["--no-server"]);
    for output in outputs {
        assert_eq!(String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&cold.stdout), "{output:?}");
        assert_eq!(output.status.code(), cold.status.code());
    }
    assert_eq!(server.occurrences("started as process"), 1, "{}", server.log());
}

#[test]
fn a_server_killed_mid_request_restarts_once_and_answers() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    let workspace = workspace.path();
    let client = server
        .mago(workspace, &["analyze", "--reporting-format", "emacs"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("mago runs");

    let deadline = Instant::now() + Duration::from_secs(60);
    let state = loop {
        let progress = std::fs::read_dir(server.runtime.path().join("mago"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .find(|folder| folder.join("progress.json").exists());
        if let Some(folder) = progress {
            break folder.join("server.json");
        }
        assert!(Instant::now() < deadline, "the server never started a registration");
        std::thread::sleep(Duration::from_millis(1));
    };
    let state: Value = serde_json::from_str(&std::fs::read_to_string(state).expect("server.json")).expect("JSON state");
    let pid = state["pid"].as_u64().expect("a pid").to_string();
    assert!(Command::new("kill").args(["-9", &pid]).status().expect("kill runs").success());

    let served = client.wait_with_output().expect("mago finishes");
    let cold = server.analyze(workspace, &["--no-server"]);
    assert_eq!(String::from_utf8_lossy(&served.stdout), String::from_utf8_lossy(&cold.stdout), "{served:?}");
    assert_eq!(served.status.code(), cold.status.code());
    assert_eq!(server.occurrences("started as process"), 2, "{}", server.log());
}

#[test]
fn a_worker_that_dies_between_checks_restarts_and_answers() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    let workspace = workspace.path();
    server.check(workspace, &[]);

    let state = std::fs::read_to_string(server.folder().join("server.json")).expect("server.json");
    let state: Value = serde_json::from_str(&state).expect("JSON state");
    let pid = state["pid"].as_u64().expect("a pid").to_string();
    assert!(Command::new("pkill").args(["-9", "-P", &pid]).status().expect("pkill runs").success());

    write(workspace, "src/Second.php", &SECOND.replace("/home", "/other"));
    server.check(workspace, &[]);
}

#[test]
fn a_restored_overlay_answers_an_edit_like_a_cold_run() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    let workspace = workspace.path();
    server.check(workspace, &[]);
    server.stop(workspace);

    write(workspace, "src/Second.php", &SECOND.replace("/home", "/other"));
    server.check(workspace, &[]);
    assert_eq!(server.occurrences("restored its overlay"), 1, "{}", server.log());
}

#[test]
fn an_extension_edit_or_a_configuration_change_discards_the_overlay() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = message_project();
    let workspace = workspace.path();
    server.check(workspace, &[]);
    server.stop(workspace);

    write(workspace, "extension/message.php", "<?php\n\nreturn 'Second message.';\n");
    server.check(workspace, &[]);
    server.stop(workspace);

    let configuration = std::fs::read_to_string(workspace.join("mago.toml")).expect("mago.toml");
    write(workspace, "mago.toml", &configuration.replace("php-version = \"8.4\"", "php-version = \"8.3\""));
    server.check(workspace, &[]);

    assert_eq!(server.occurrences("restored its overlay"), 0, "{}", server.log());
    assert_eq!(server.occurrences("will analyze from scratch"), 3, "{}", server.log());
}

#[test]
fn a_worker_speaking_another_minor_version_is_refused() {
    if !available() {
        return;
    }

    let server = Server::new();
    let directory = tempfile::tempdir().expect("temporary workspace");
    let workspace = directory.path();
    let protocol = std::fs::read_to_string(repository().join("composer/src/Sdk/Internal/Analyzer/Protocol.php"))
        .expect("the SDK's analyzer protocol");
    let minor = protocol.find("private const MINOR = ").expect("the SDK's minor version");
    let (start, end) =
        (minor + "private const MINOR = ".len(), protocol[minor..].find(';').expect("a constant") + minor);
    let version = protocol[start..end].parse::<u16>().expect("a minor version");
    let other = version + 1;
    write(workspace, "extension/Protocol.php", &format!("{}{other}{}", &protocol[..start], &protocol[end..]));
    write(
        workspace,
        "extension/worker.php",
        &OTHER_MINOR_WORKER.replace("REPOSITORY", &repository().display().to_string()),
    );
    write(workspace, "mago.toml", &configuration(&workspace.join("extension/worker.php"), ""));
    write(workspace, "src/First.php", FIRST);

    let refused = server.check(workspace, &[]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{stderr}");
    assert!(stderr.contains("extension host \"proof\""), "{stderr}");
    assert!(stderr.contains(&format!("1.{version}")) && stderr.contains(&format!("1.{other}")), "{stderr}");
}

#[test]
fn a_host_that_inherits_the_environment_is_analyzed_in_process() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    let workspace = workspace.path();
    let configuration = std::fs::read_to_string(workspace.join("mago.toml")).expect("mago.toml");
    write(workspace, "mago.toml", &configuration.replace("inherit-environment = false", ""));

    let in_process = server.check(workspace, &[]);
    let stderr = String::from_utf8_lossy(&in_process.stderr);
    assert!(String::from_utf8_lossy(&in_process.stdout).contains("server-proof/duplicate-route"), "{in_process:?}");
    assert_eq!(stderr.matches("Set `inherit-environment = false`").count(), 1, "{stderr}");
    assert!(server.log().is_empty(), "no server starts: {}", server.log());

    write(workspace, "mago.toml", &configuration);
    let served = server.check(workspace, &[]);
    assert!(!String::from_utf8_lossy(&served.stderr).contains("inherit-environment"), "{served:?}");
    assert_eq!(server.occurrences("started as process"), 1, "{}", server.log());
}

#[test]
fn a_check_runs_the_node_hooks_of_the_files_it_names_once() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = message_project();
    let workspace = workspace.path();
    write(workspace, "src/Second.php", SECOND);
    let hooked = |file: &str| {
        let log = std::fs::read_to_string(workspace.join("node-hooks.log")).unwrap_or_default();
        log.lines().filter(|line| *line == file).count()
    };

    server.analyze(workspace, &["src/First.php"]);
    assert_eq!((hooked("src/First.php"), hooked("src/Second.php")), (1, 0), "{}", server.log());

    server.analyze(workspace, &["src/Second.php"]);
    server.analyze(workspace, &["src/Second.php"]);
    server.analyze(workspace, &["src/First.php"]);
    assert_eq!((hooked("src/First.php"), hooked("src/Second.php")), (1, 1), "{}", server.log());

    for file in ["src/First.php", "src/Second.php"] {
        assert!(String::from_utf8_lossy(&server.check(workspace, &[file]).stdout).contains("message"));
    }
}

#[test]
fn verify_finds_the_warm_analysis_equal_to_a_fresh_one() {
    if !available() {
        return;
    }

    let server = Server::new();
    let workspace = project("");
    let workspace = workspace.path();
    server.check(workspace, &[]);
    write(workspace, "src/Second.php", &SECOND.replace("return 2;", "return 'two';"));
    write(workspace, "src/Third.php", "<?php\n\n// route: /home\nfunction third(): void {}\n");
    server.check(workspace, &[]);

    let verified = server.mago(workspace, &["server", "verify"]).output().expect("mago runs");
    assert!(verified.status.success(), "{verified:?}");
    assert!(String::from_utf8_lossy(&verified.stdout).contains("equals a fresh one"), "{verified:?}");
}
