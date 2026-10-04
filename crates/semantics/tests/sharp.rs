#![allow(clippy::panic, clippy::expect_used)]

use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_semantics::SemanticsChecker;
use mago_syntax::parser::parse_file;

/// Every semantic issue in the PHP# source, as `line:column message` at its primary span.
fn issues(code: &'static str) -> Vec<String> {
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Borrowed(b"src/Report.sharp"), Cow::Borrowed(code.as_bytes()));
    let program = parse_file(&arena, &file);
    assert!(program.errors.is_empty(), "test source did not parse: {:#?}", program.errors);

    let names = NameResolver::new(&arena).resolve(program);
    let issues = SemanticsChecker::new(PHPVersion::new(8, 4, 0)).check(&file, program, &names);

    issues
        .iter()
        .map(|issue| {
            let span = issue.primary_span().expect("a primary span");
            let offset = span.start.offset as usize;
            let line = code[..offset].matches('\n').count() + 1;
            let column = offset - code[..offset].rfind('\n').map_or(0, |newline| newline + 1) + 1;

            format!("{line}:{column} {}", issue.message)
        })
        .collect()
}

fn method(body: &str) -> String {
    format!("namespace App.Tenant;\n\nclass Report\n{{\n    public int run(int extra)\n    {{\n{body}    }}\n}}\n")
}

fn leak(code: String) -> &'static str {
    Box::leak(code.into_boxed_str())
}

#[test]
fn the_slice_has_no_semantic_issues() {
    let code = leak(method(
        "        let label = \"one\";\n        const base = 2;\n        label = \"two\";\n        {\n            let inner = base * extra - 1;\n            inner = inner / 2;\n        }\n        return this.total(base, extra) + Calc.make().add(label);\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn reassigning_a_const_local_is_an_error() {
    let code = leak(method("        const base = 2;\n        base = 3;\n        return base;\n"));

    assert_eq!(issues(code), ["8:9 Cannot assign to `base`: it is declared with `const`."]);
}

#[test]
fn reassigning_a_const_local_spelled_in_capitals_is_an_error() {
    let code = leak(method("        CONST base = 2;\n        base = 3;\n        return base;\n"));

    assert_eq!(issues(code), ["8:9 Cannot assign to `base`: it is declared with `const`."]);
}

#[test]
fn incrementing_or_decrementing_a_const_local_is_an_error() {
    let code = leak(method(
        "        const base = 2;\n        base++;\n        ++base;\n        base--;\n        --base;\n        return base;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:9 Cannot increment `base`: it is declared with `const`.",
            "9:11 Cannot increment `base`: it is declared with `const`.",
            "10:9 Cannot decrement `base`: it is declared with `const`.",
            "11:11 Cannot decrement `base`: it is declared with `const`.",
        ]
    );
}

#[test]
fn redeclaring_a_name_an_enclosing_block_declares_is_an_error() {
    let code = leak(method(
        "        let total = 1;\n        {\n            let total = 2;\n            let extra = 3;\n        }\n        return total;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "9:17 `total` is already declared in an enclosing block of this method.",
            "10:17 `extra` is already declared in an enclosing block of this method.",
        ]
    );
}

#[test]
fn using_a_local_after_its_block_closes_is_an_error() {
    let code = leak(method("        {\n            let inner = 1;\n        }\n        return inner;\n"));

    assert_eq!(issues(code), ["10:16 `inner` is used after the block that declares it closes."]);
}

#[test]
fn a_top_level_function_is_an_error() {
    let code = "namespace App.Tenant;\n\nfunction total(int extra) { return extra; }\n";

    assert_eq!(issues(code), ["3:10 PHP# has no top-level functions: move `total` into a class as a static method."]);
}

#[test]
fn dollar_variables_are_errors() {
    let code = leak(method("        $total = 1;\n        $$total;\n        return 1;\n"));

    assert_eq!(
        issues(code),
        [
            "7:9 PHP# variables have no `$`: write `total`.",
            "8:9 Variable variables are not part of PHP#.",
            "8:10 PHP# variables have no `$`: write `total`.",
        ]
    );
}

#[test]
fn a_dollar_parameter_is_an_error() {
    let code = "class Report\n{\n    public int run(int $extra) { return 1; }\n}\n";

    assert_eq!(issues(code), ["3:24 PHP# variables have no `$`: write `extra`."]);
}

#[test]
fn compact_extract_and_global_are_errors() {
    let code =
        leak(method("        compact(\"extra\");\n        extract([]);\n        global $config;\n        return 1;\n"));

    assert_eq!(
        issues(code),
        [
            "7:9 `compact()` is not part of PHP#.",
            "8:9 `extract()` is not part of PHP#.",
            "9:9 `global` is not part of PHP#.",
            "9:16 PHP# variables have no `$`: write `config`.",
        ]
    );
}

#[test]
fn a_full_name_inside_code_names_the_import_to_add() {
    let code = leak(method("        return App.Shared.Money.of(extra);\n"));

    assert_eq!(
        issues(code),
        ["7:16 Full names appear only in `import` lines: add `import App.Shared.Money;` and write `Money`."]
    );
}

#[test]
fn a_class_declared_in_the_file_is_never_the_root_of_a_full_name() {
    let code = leak(method("        return Report.Totals.of(extra);\n"));

    assert_eq!(issues(code), ["7:16 Reading `Report.Totals` without a call is not supported yet."]);
}

#[test]
fn reading_a_static_member_without_a_call_is_not_supported_yet() {
    let code = leak(method("        return Calc.rate;\n"));

    assert_eq!(issues(code), ["7:16 Reading `Calc.rate` without a call is not supported yet."]);
}

#[test]
fn a_reserved_class_name_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Mixed\n{\n}\n";

    assert_eq!(issues(code), ["3:7 Cannot use `Mixed` as a class name: it is reserved."]);
}

#[test]
fn methods_whose_names_differ_only_in_case_are_an_error() {
    let code = "class Report\n{\n    public int run() { return 1; }\n\n    public int Run() { return 2; }\n}\n";

    // The PHP checks already reject this, so PHP# adds no second error.
    assert_eq!(issues(code), ["5:16 class method `Report::Run` has already been defined"]);
}

#[test]
fn an_import_whose_short_name_is_reserved_is_an_error() {
    let code = "namespace App.Tenant;\n\nimport Lib.Int;\nimport Lib.Mixed;\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:8 Cannot import `Lib.Int` as `Int`: PHP reserves `Int` for a type.",
            "4:8 Cannot import `Lib.Mixed` as `Mixed`: PHP reserves `Mixed` for a type.",
        ]
    );
}

#[test]
fn two_imports_with_the_same_short_name_are_an_error() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Other.CALC;\n\nclass Report\n{\n}\n";

    assert_eq!(issues(code), ["4:8 Cannot import `Other.CALC` as `CALC`: `Lib.Calc` is already imported as `Calc`."]);
}

#[test]
fn a_parameter_or_local_named_this_is_an_error() {
    let code = "class Report\n{\n    public int run(int this) { return 1; }\n\n    public int total() { let this = 1; return 1; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:24 Cannot name a parameter `this`: `this` is the object the method runs on.",
            "5:30 Cannot name a local `this`: `this` is the object the method runs on.",
        ]
    );
}

#[test]
fn importing_the_class_the_file_declares_is_valid() {
    let code = "namespace App.Tenant;\n\nimport App.Tenant.Report;\n\nclass Report\n{\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_import_named_like_a_class_of_the_same_file_is_an_error() {
    let code = "namespace App.Tenant;\n\nimport App.Shared.Report;\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(code),
        ["3:8 Cannot import `App.Shared.Report` as `Report`: this file declares a class named `Report`."]
    );
}

#[test]
fn a_local_or_parameter_named_after_a_superglobal_is_an_error() {
    let code = leak(method("        let _GET = 1;\n        return _GET;\n").replace("int extra", "int GLOBALS"));

    assert_eq!(
        issues(code),
        [
            "5:24 `GLOBALS` is the name of a PHP superglobal: rename this parameter.",
            "7:13 `_GET` is the name of a PHP superglobal: rename this local.",
        ]
    );
}

#[test]
fn a_bare_member_name_is_not_supported_yet() {
    let code = "class Report\n{\n    private int $count = 0;\n\n    public int run() { return count + run(); }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:31 Using the member `count` without `this.` is not supported yet.",
            "5:39 Using the member `run` without `this.` is not supported yet.",
        ]
    );
}
