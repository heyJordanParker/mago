//! Runs `mago compile` on a project that mixes PHP# and PHP files, with a PHP# package in `vendor/`.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use mago_sharp_bridge::unit::header;
use mago_sharp_bridge::unit::source_hash;

const ORDER: &str = "namespace App;\n\npublic class Order\n{\n    public int total(int extra)\n    {\n        return extra + 1;\n    }\n}\n";
const BROKEN_ORDER: &str = "namespace App;\n\npublic class Order\n{\n    public int total(int extra)\n    {\n        return \"one\";\n    }\n}\n";
const MONEY: &str =
    "namespace Acme;\n\npublic class Money\n{\n    public int cents()\n    {\n        return 100;\n    }\n}\n";
const BROKEN_MONEY: &str =
    "namespace Acme;\n\npublic class Money\n{\n    public int cents()\n    {\n        return \"one\";\n    }\n}\n";
const LEGACY: &str = "<?php\n\nnamespace App;\n\nfinal class Legacy {}\n";
const MONEY_PATH: &str = "vendor/acme/money/src/Money.sharp";

/// A project with `app/Order.sharp`, `app/Legacy.php` and the package file `vendor/acme/money/src/Money.sharp`.
fn project(order: &str, money: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    write(
        directory.path(),
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n",
    );
    write(directory.path(), "composer.lock", "{}\n");
    write(directory.path(), "app/Order.sharp", order);
    write(directory.path(), "app/Legacy.php", LEGACY);
    write(directory.path(), MONEY_PATH, money);
    directory
}

fn write(root: &Path, name: &str, contents: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn compile(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "compile"])
        .env("MAGO_LOG", "info")
        .current_dir(root)
        .output()
        .unwrap()
}

fn printed(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// Every file under `folder`, relative to it, in path order.
fn files_under(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(next) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                files.push(path.strip_prefix(folder).unwrap().to_path_buf());
            }
        }
    }
    files.sort();
    files
}

fn compiled(root: &Path, name: &str) -> PathBuf {
    root.join(".sharp").join(name).with_extension("sharpc")
}

#[test]
fn compile_writes_each_accepted_file_into_the_sharp_folder_at_its_source_path() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let vendor = files_under(&root.join("vendor"));

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        files_under(&root.join(".sharp")),
        [Path::new("app/Order.sharpc"), Path::new("vendor/acme/money/src/Money.sharpc")]
    );
    for (name, source) in [("app/Order.sharp", ORDER), (MONEY_PATH, MONEY)] {
        let bytes = std::fs::read(compiled(root, name)).unwrap();
        let header = header(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(header.source_hash, source_hash(source.as_bytes()), "{name}");
    }
    assert_eq!(files_under(&root.join("vendor")), vendor, "nothing is written under vendor/");
    assert!(printed(&output).contains("Compiled 2 PHP# files into .sharp/."), "{}", printed(&output));
}

#[test]
fn compile_refuses_a_file_with_an_error_writes_the_others_and_deletes_its_old_compiled_file() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    assert!(compile(root).status.success());

    write(root, "app/Order.sharp", BROKEN_ORDER);
    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(!compiled(root, "app/Order.sharp").exists());
    assert!(compiled(root, MONEY_PATH).exists());
    assert!(printed(&output).contains("app/Order.sharp"), "{}", printed(&output));
    assert!(printed(&output).contains("1 PHP# file was refused"), "{}", printed(&output));
}

#[test]
fn compile_refuses_a_package_file_with_an_error_and_names_its_package() {
    let directory = project(ORDER, BROKEN_MONEY);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(!compiled(root, MONEY_PATH).exists());
    assert!(compiled(root, "app/Order.sharp").exists());
    assert!(printed(&output).contains("Composer package acme/money"), "{}", printed(&output));
}

#[test]
fn compile_deletes_the_compiled_file_of_a_renamed_or_removed_source() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    assert!(compile(root).status.success());

    std::fs::rename(root.join("app/Order.sharp"), root.join("app/Invoice.sharp")).unwrap();
    assert!(compile(root).status.success());
    assert!(!compiled(root, "app/Order.sharp").exists());
    assert!(compiled(root, "app/Invoice.sharp").exists());

    std::fs::remove_file(root.join("app/Invoice.sharp")).unwrap();
    assert!(compile(root).status.success());
    assert_eq!(files_under(&root.join(".sharp")), [Path::new("vendor/acme/money/src/Money.sharpc")]);
}

#[test]
fn compile_writes_no_sharp_folder_in_a_project_without_php_sharp_files() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n");
    write(root, "app/Legacy.php", LEGACY);

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(!root.join(".sharp").exists());
}

#[test]
fn compile_reports_a_sharp_folder_inside_a_package() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "vendor/acme/money/.sharp/src/Money.sharpc", "stale");

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("Delete vendor/acme/money/.sharp."), "{}", printed(&output));
    assert!(root.join("vendor/acme/money/.sharp/src/Money.sharpc").exists(), "nothing under vendor/ changes");
}

#[cfg(unix)]
#[test]
fn compile_reports_a_link_to_a_file_outside_the_project_and_writes_no_compiled_file_for_it() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let elsewhere = tempfile::tempdir().unwrap();
    write(elsewhere.path(), "Money.sharp", MONEY);
    std::fs::remove_file(root.join(MONEY_PATH)).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("Money.sharp"), root.join(MONEY_PATH)).unwrap();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("vendor/acme/money/src/Money.sharp is a link to"), "{}", printed(&output));
    assert_eq!(files_under(&root.join(".sharp")), [Path::new("app/Order.sharpc")]);
}

#[test]
fn compile_replaces_a_compiled_file_by_rename_so_no_reader_sees_a_partial_file() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    assert!(compile(root).status.success());
    let before = std::fs::read(compiled(root, "app/Order.sharp")).unwrap();
    let mut open = std::fs::File::open(compiled(root, "app/Order.sharp")).unwrap();

    write(root, "app/Order.sharp", &ORDER.replace("extra + 1", "extra + 2"));
    assert!(compile(root).status.success());

    let mut still_open = Vec::new();
    std::io::Read::read_to_end(&mut open, &mut still_open).unwrap();
    assert_eq!(still_open, before, "the old compiled file stays whole for a reader that opened it");
    assert_ne!(std::fs::read(compiled(root, "app/Order.sharp")).unwrap(), before);
    assert_eq!(files_under(&root.join(".sharp/app")), [Path::new("Order.sharpc")], "no temporary file is left");
}
