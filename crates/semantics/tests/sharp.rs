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

/// One file that uses every construct `check_slice` accepts. The engine's bridge lowers the same file.
#[test]
fn the_slice_fixture_has_no_semantic_issues() {
    assert_eq!(issues(include_str!("fixtures/slice.sharp")), Vec::<String>::new());
}

#[test]
fn every_construct_outside_the_slice_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nlet top = 1;\necho 1;\n\ninterface Shape\n{\n}\n\nenum Suit\n{\n}\n\nclass Report\n{\n    public int run(int extra)\n    {\n        switch (extra) {\n            default: return 1;\n        }\n        echo extra;\n        const made = new Report;\n        const arrow = fn() => 1;\n        const closure = function () { return 1; };\n        const partial = this.run(...);\n        const text = \"total {$extra}\";\n        return extra;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:1 This statement is not supported yet in PHP#.",
            "4:1 This statement is not supported yet in PHP#.",
            "6:1 This statement is not supported yet in PHP#.",
            "10:1 This statement is not supported yet in PHP#.",
            "18:9 This statement is not supported yet in PHP#.",
            "21:9 This statement is not supported yet in PHP#.",
            "22:22 This `new` without arguments is not supported yet in PHP#.",
            "23:23 This expression is not supported yet in PHP#.",
            "24:25 This expression is not supported yet in PHP#.",
            "25:25 This expression is not supported yet in PHP#.",
            "26:22 This expression is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn if_else_if_and_else_with_braces_are_in_the_slice() {
    let code = leak(method(
        "        if (extra > 1) {\n            return 1;\n        } else if (extra < 0) {\n            return 2;\n        } else {\n            return 3;\n        }\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_if_or_else_without_braces_is_not_supported_yet() {
    let code = leak(method(
        "        if (extra > 1) return 1;\n        if (extra > 2) {\n            return 2;\n        } else return 3;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This statement without braces is not supported yet in PHP#.",
            "8:9 This statement without braces is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn elseif_and_a_colon_delimited_if_are_not_supported_yet() {
    let code = leak(method(
        "        if (extra > 1) {\n            return 1;\n        } elseif (extra > 2) {\n            return 2;\n        }\n        if (extra > 3):\n            return 3;\n        endif;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        ["9:11 This `elseif` is not supported yet in PHP#.", "12:23 This construct is not supported yet in PHP#."]
    );
}

#[test]
fn a_local_declared_in_an_if_branch_is_out_of_scope_after_it() {
    let code = leak(method(
        "        if (extra > 1) {\n            let inner = 1;\n        } else {\n            let inner = 2;\n        }\n        return inner;\n",
    ));

    assert_eq!(issues(code), ["12:16 `inner` is used after the block that declares it closes."]);
}

#[test]
fn while_and_do_while_with_braces_are_in_the_slice() {
    let code = leak(method(
        "        while (extra > 0) {\n            extra -= 1;\n        }\n        do {\n            extra += 1;\n        } while (extra < 3);\n        return extra;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_loop_without_braces_or_with_a_colon_body_is_not_supported_yet() {
    let code = leak(method(
        "        while (extra > 0) extra -= 1;\n        do extra += 1; while (extra < 3);\n        while (extra > 1):\n            extra -= 1;\n        endwhile;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This statement without braces is not supported yet in PHP#.",
            "8:9 This statement without braces is not supported yet in PHP#.",
            "9:26 This construct is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_local_declared_in_a_loop_body_is_out_of_scope_after_it() {
    let code = leak(method(
        "        while (extra > 0) {\n            let step = 1;\n            extra -= step;\n        }\n        do {\n            let step = 2;\n        } while (extra < 0);\n        return step;\n",
    ));

    assert_eq!(issues(code), ["14:16 `step` is used after the block that declares it closes."]);
}

#[test]
fn break_and_continue_without_a_level_are_in_the_slice() {
    let code = leak(method(
        "        while (extra > 0) {\n            extra -= 1;\n            if (extra > 5) {\n                continue;\n            }\n            break;\n        }\n        return extra;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn break_and_continue_with_a_level_are_not_supported_yet() {
    let code = leak(method(
        "        while (extra > 0) {\n            while (extra > 1) {\n                break 2;\n            }\n            continue 1;\n        }\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        ["9:17 This statement is not supported yet in PHP#.", "11:13 This statement is not supported yet in PHP#."]
    );
}

#[test]
fn a_for_loop_with_a_let_counter_or_expressions_is_in_the_slice() {
    let code = leak(method(
        "        let total = 0;\n        for (let step = 0; step < extra; step++) {\n            total += step;\n        }\n        for (total = 0; total < 3; total++, extra--) {\n        }\n        for (;;) {\n            break;\n        }\n        return total;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_for_counter_is_out_of_scope_after_its_loop_and_const_when_declared_so() {
    let code = leak(method("        for (const step = 0; step < extra; step++) {\n        }\n        return step;\n"));

    assert_eq!(
        issues(code),
        [
            "9:16 `step` is used after the block that declares it closes.",
            "7:44 Cannot increment `step`: it is declared with `const`.",
        ]
    );
}

#[test]
fn a_for_loop_without_braces_or_with_a_colon_body_is_not_supported_yet() {
    let code = leak(method(
        "        for (let step = 0; step < 3; step++) extra++;\n        for (;;):\n            break;\n        endfor;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This statement without braces is not supported yet in PHP#.",
            "8:17 This construct is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn for_of_over_values_or_keys_and_values_is_in_the_slice() {
    let code = leak(method(
        "        for (const value of Store.values()) {\n            extra += value;\n        }\n        for (let [key, value] of Store.values()) {\n            value += key;\n            extra += value;\n        }\n        return extra;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_const_loop_variable_cannot_be_assigned_and_is_out_of_scope_after_its_loop() {
    let code = leak(method(
        "        for (const [key, value] of Store.values()) {\n            value = key;\n        }\n        return value;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "10:16 `value` is used after the block that declares it closes.",
            "8:13 Cannot assign to `value`: it is declared with `const`.",
        ]
    );
}

#[test]
fn a_loop_variable_named_this_or_a_superglobal_is_an_error() {
    let code = leak(method(
        "        for (const this of Store.values()) {\n        }\n        for (const [_GET, value] of Store.values()) {\n        }\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:20 Cannot name a loop variable `this`: `this` is the object the method runs on.",
            "9:21 `_GET` is the name of a PHP superglobal: rename this loop variable.",
        ]
    );
}

#[test]
fn a_for_of_loop_without_braces_is_not_supported_yet() {
    let code = leak(method("        for (const value of Store.values()) extra += value;\n        return extra;\n"));

    assert_eq!(issues(code), ["7:9 This statement without braces is not supported yet in PHP#."]);
}

#[test]
fn reassigning_a_const_local_is_an_error() {
    let code = leak(method("        const base = 2;\n        base = 3;\n        return base;\n"));

    assert_eq!(issues(code), ["8:9 Cannot assign to `base`: it is declared with `const`."]);
}

#[test]
fn null_coalescing_assignment_to_a_const_local_is_an_error() {
    let code = leak(method("        const base = null;\n        base ??= 3;\n        return base;\n"));

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
fn writing_a_const_local_after_its_block_closes_reports_only_the_scope_error() {
    let code =
        leak(method("        {\n            const inner = 1;\n        }\n        inner = 2;\n        return 1;\n"));

    assert_eq!(issues(code), ["10:9 `inner` is used after the block that declares it closes."]);
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
        ["7:9 PHP# variables have no `$`: write `total`.", "8:9 Variable variables are not part of PHP#.",]
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
        ]
    );
}

#[test]
fn a_dollar_variable_in_a_string_reports_one_error() {
    let code = leak(method("        return \"{$extra}\";\n"));

    assert_eq!(issues(code), ["7:16 This expression is not supported yet in PHP#."]);
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
fn writing_a_static_member_is_not_supported_yet() {
    let code = leak(method(
        "        Calc.rate = 2;\n        Calc.count++;\n        Calc.rate.cents = 3;\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 Writing `Calc.rate` is not supported yet.",
            "8:9 Writing `Calc.count` is not supported yet.",
            "9:9 Reading `Calc.rate` without a call is not supported yet.",
        ]
    );
}

#[test]
fn null_safe_access_on_a_class_is_an_error_that_names_the_dot() {
    let code = leak(method("        Calc?.make();\n        return Calc?.rate;\n"));

    assert_eq!(
        issues(code),
        [
            "7:9 `Calc` is a class, which is never null: write `Calc.make`.",
            "8:16 `Calc` is a class, which is never null: write `Calc.rate`.",
        ]
    );
}

#[test]
fn a_write_through_null_safe_access_is_not_supported() {
    let code = leak(method(
        "        this?.count = 1;\n        this?.count ??= 2;\n        this?.count++;\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This write target is not supported yet in PHP#.",
            "8:9 This write target is not supported yet in PHP#.",
            "9:9 This write target is not supported yet in PHP#.",
        ]
    );
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
fn a_bare_member_name_is_an_error_that_names_this() {
    let code =
        "class Report\n{\n    public int count() { return 0; }\n\n    public int run() { return count + run(); }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:31 Write `this.count()`: members of the same object are always written with `this.`.",
            "5:39 Write `this.run()`: members of the same object are always written with `this.`.",
        ]
    );
}

#[test]
fn a_bare_method_name_matches_ignoring_case_as_in_php() {
    let code = "class Report\n{\n    public int total() { return 0; }\n\n    public int run() { return Total(); }\n}\n";

    assert_eq!(
        issues(code),
        ["5:31 Write `this.total()`: members of the same object are always written with `this.`."]
    );
}

#[test]
fn a_bare_member_in_a_static_method_names_the_class() {
    let code = "class Report\n{\n    public static int helper() { return 0; }\n\n    public static int run() { return helper(); }\n}\n";

    assert_eq!(
        issues(code),
        ["5:38 Write `Report.helper()`: a static method reaches the members of its class through the class name."]
    );
}

#[test]
fn extends_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Base\n{\n}\n\nclass Report extends Base\n{\n}\n";

    assert_eq!(issues(code), ["7:14 This `extends` clause is not supported yet in PHP#."]);
}

#[test]
fn implements_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nimport Lib.Shape;\n\nclass Report implements Shape\n{\n}\n";

    assert_eq!(issues(code), ["5:14 This `implements` clause is not supported yet in PHP#."]);
}

#[test]
fn attributes_on_a_class_its_members_and_their_parameters_are_in_the_slice() {
    let code = "namespace App.Tenant;\n\n[Entity(label: \"Order Items\"), Searchable]\n[Table(\"orders\")]\nclass Report\n{\n    [Field] private int count = 0;\n    [Field(label: \"Name\", width: 2 * 3, limit: PHP_INT_MAX)] public string name { get; set; }\n\n    public Report([Field(label: null)] public string tenant { get; }, [Field] private int page = 1)\n    {\n    }\n\n    [Action(true, -1.5)]\n    [Retry(3)]\n    public int run([Field(\"extra\")] int extra)\n    {\n        return extra;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn attribute_arguments_outside_the_slice_are_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    [Field(typeof(Report))]\n    [Field(Mode.Write)]\n    [Field([\"a\"])]\n    [Field(label: new Report())]\n    public int run(int extra)\n    {\n        return extra;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:12 This expression is not supported yet in PHP#.",
            "6:12 This expression is not supported yet in PHP#.",
            "7:12 This expression is not supported yet in PHP#.",
            "8:19 This expression is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn final_abstract_and_readonly_are_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nfinal class Report\n{\n    final public int run()\n    {\n        return 1;\n    }\n}\n\nabstract class Shape\n{\n    abstract public int area();\n}\n\nreadonly class Point\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:1 This modifier is not supported yet in PHP#.",
            "5:5 This modifier is not supported yet in PHP#.",
            "11:1 This modifier is not supported yet in PHP#.",
            "13:5 This modifier is not supported yet in PHP#.",
            "16:1 This modifier is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_method_name_starting_with_two_underscores_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public void __clone() { }\n    public int __get(string name) { return 1; }\n    public int __invoke() { return 1; }\n    public string __toString() { return \"report\"; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:17 This method name is not supported yet in PHP#.",
            "6:16 This method name is not supported yet in PHP#.",
            "7:16 This method name is not supported yet in PHP#.",
            "8:19 This method name is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_method_without_an_access_modifier_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    int run() { return 1; }\n    static int make() { return 1; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:9 A method without `public`, `protected` or `private` is not supported yet in PHP#.",
            "6:16 A method without `public`, `protected` or `private` is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_method_named_after_its_class_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int report() { return 1; }\n}\n\nclass Calc\n{\n    public int CALC() { return 1; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:16 A method named after its class is not supported yet in PHP#.",
            "10:16 A method named after its class is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn the_constructor_is_named_after_its_class_without_a_return_type() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private int count;\n\n    public Report(int start)\n    {\n        this.count = start;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_constructor_parameter_with_an_access_modifier_declares_a_field_or_a_property() {
    let code = "namespace App.Tenant;\n\nimport Lib.Planner;\n\nclass Report\n{\n    public Report(\n        private Planner planner,\n        protected int count = 0,\n        public int id { get; },\n        public string name { get; private set; },\n        private int hidden { get; set; },\n        int extra,\n    ) {\n        this.id = extra + count;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_promoted_member_read_without_this_is_an_error_that_names_this() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(private int count)\n    {\n        count = 1;\n    }\n\n    public int total()\n    {\n        return count;\n    }\n}\n";

    assert_eq!(issues(code), ["12:16 Write `this.count`: members of the same object are always written with `this.`."]);
}

#[test]
fn a_public_constructor_parameter_without_accessors_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(public int id) {}\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:19 A `public` constructor parameter needs accessors: a public member is a property, as in `public int id { get; }`."
        ]
    );
}

#[test]
fn a_promoted_member_follows_the_rules_of_the_same_declaration_in_the_class_body() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(public int a { set; }, private int b { get; private set; }, public int c { get => 1; }) {}\n\n    public void reset()\n    {\n        this.a = 1;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:30 A PHP# property needs a `get` accessor.",
            "5:63 The `set` accessor of a PHP# property must be narrower than the property.",
            "5:94 This accessor is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_promoted_member_outside_the_slice_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(private readonly int a, private static int b) {}\n\n    public void run(private int c) {}\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:21 Promoted properties are not allowed outside of constructors.",
            "5:51 Parameter `b` cannot have the `static` modifier.",
            "5:27 This modifier is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_method_without_a_return_type_named_otherwise_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public report() {}\n    public Calc() {}\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:12 A PHP# method needs a return type: only the constructor, named after its class, has none.",
            "6:12 A PHP# method needs a return type: only the constructor, named after its class, has none.",
        ]
    );
}

#[test]
fn a_static_constructor_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public static Report() {}\n}\n";

    assert_eq!(issues(code), ["5:19 A static constructor is not supported yet in PHP#."]);
}

#[test]
fn a_method_without_a_body_reports_only_the_php_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int run();\n}\n";

    assert_eq!(issues(code), ["5:21 Non-Abstract method `Report::run` must have a concrete body."]);
}

#[test]
fn a_field_is_private_or_protected_with_a_type_and_an_optional_constant_initial_value() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private int count = 0;\n    protected float rate = 1.5 * -PHP_INT_MAX;\n    private string label;\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_initial_value_may_be_any_expression_a_method_body_has_without_this() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private Calc calc = new Calc(1, rate: 2);\n    private int made = Calc.make() + 1;\n    public int total { get; set; } = new Calc(2).add(1, 2);\n    private int count = this.made;\n}\n";

    assert_eq!(
        issues(code),
        ["10:25 An initial value cannot use `this`: it runs before the constructor body, while the object is built."]
    );
}

#[test]
fn a_public_field_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int count = 0;\n}\n";

    assert_eq!(issues(code), ["5:5 A PHP# field cannot be `public`: a field is `private` or `protected`."]);
}

#[test]
fn a_field_without_an_access_modifier_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    int count = 0;\n}\n";

    assert_eq!(issues(code), ["5:9 A field without `private` or `protected` is not supported yet in PHP#."]);
}

#[test]
fn a_field_ended_by_a_closing_tag_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private int count = 0 ?><?php\n}\n";

    assert_eq!(issues(code), ["5:27 This construct is not supported yet in PHP#."]);
}

#[test]
fn fields_outside_the_slice_are_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private static int count = 0;\n    private int first, second;\n    private int? maybe;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:13 This modifier is not supported yet in PHP#.",
            "6:5 A field declaring several names is not supported yet in PHP#.",
            "7:13 This type is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn an_auto_property_has_get_an_optional_narrower_set_and_an_optional_constant_initial_value() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; set; } = \"none\";\n    public float rate { get; protected set; }\n    protected int total { get; private set; }\n    public int id { get; }\n    private int hidden { get; set; }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_auto_property_breaking_the_accessor_rules_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int a { get; public set; }\n    private int b { get; private set; }\n    protected int c { get; public set; }\n    public int d { set; }\n    public int e { get; get; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:25 The `set` accessor of a PHP# property must be narrower than the property.",
            "6:26 The `set` accessor of a PHP# property must be narrower than the property.",
            "7:28 The `set` accessor of a PHP# property must be narrower than the property.",
            "8:16 A PHP# property needs a `get` accessor.",
            "9:25 A PHP# property declares each accessor once.",
        ]
    );
}

#[test]
fn properties_outside_the_slice_are_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    int a { get; set; }\n    public static int b { get; }\n    public int c { private get; set; }\n    public int d { get => 1; }\n    public int e { get; set { } }\n    public int f { get; init; }\n    public int g = 0 { get; }\n    public int h { get; set(int value); }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:9 A property without `public`, `protected` or `private` is not supported yet in PHP#.",
            "6:12 This modifier is not supported yet in PHP#.",
            "7:20 This accessor is not supported yet in PHP#.",
            "8:20 This accessor is not supported yet in PHP#.",
            "9:25 This accessor is not supported yet in PHP#.",
            "10:25 This accessor is not supported yet in PHP#.",
            "11:16 An initial value before the accessors is not supported yet in PHP#.",
            "12:25 This accessor is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_variadic_parameter_is_not_supported_yet() {
    let code = leak(method("        return 1;\n").replace("int extra", "int ...extra"));

    assert_eq!(issues(code), ["5:24 This variadic parameter is not supported yet in PHP#."]);
}

#[test]
fn operators_outside_the_slice_are_not_supported_yet() {
    let code = leak(method(
        "        let a = extra;\n        a = @extra;\n        a = (int) extra;\n        a = extra ** 2;\n        a = extra & 1;\n        a = extra | 1;\n        a = extra ^ 1;\n        a = extra << 1;\n        a = extra >> 1;\n        a = ~extra;\n        a = extra xor true;\n        a = extra and true;\n        a = extra or true;\n        a = extra <=> 1;\n        a = extra <> 1;\n        a %= 2;\n        a **= 2;\n        a &= 2;\n        return a;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:13 This operator is not supported yet in PHP#.",
            "9:13 This operator is not supported yet in PHP#.",
            "10:19 This operator is not supported yet in PHP#.",
            "11:19 This operator is not supported yet in PHP#.",
            "12:19 This operator is not supported yet in PHP#.",
            "13:19 This operator is not supported yet in PHP#.",
            "14:19 This operator is not supported yet in PHP#.",
            "15:19 This operator is not supported yet in PHP#.",
            "16:13 This operator is not supported yet in PHP#.",
            "17:19 This operator is not supported yet in PHP#.",
            "18:19 This operator is not supported yet in PHP#.",
            "19:19 This operator is not supported yet in PHP#.",
            "20:19 This operator is not supported yet in PHP#.",
            "21:19 This operator is not supported yet in PHP#.",
            "22:11 This operator is not supported yet in PHP#.",
            "23:11 This operator is not supported yet in PHP#.",
            "24:11 This operator is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_member_name_written_as_an_expression_is_not_supported_yet() {
    let code = leak(method("        return this.{\"run\"}(extra);\n"));

    assert_eq!(issues(code), ["7:21 This member name is not supported yet in PHP#."]);
}

#[test]
fn types_outside_the_slice_are_not_supported_yet() {
    let code = "class Report\n{\n    public mixed run(iterable? a, int|string b, iterable c, callable d, (Lib&Other)|null e)\n    {\n        return 1;\n    }\n\n    public self make(Lib f, float g, bool h, string i)\n    {\n        return this;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:12 This type is not supported yet in PHP#.",
            "3:22 This type is not supported yet in PHP#.",
            "3:35 This type is not supported yet in PHP#.",
            "3:49 This type is not supported yet in PHP#.",
            "3:61 This type is not supported yet in PHP#.",
            "3:73 This type is not supported yet in PHP#.",
            "8:12 This type is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_typed_local_takes_the_types_of_the_slice_but_not_void() {
    let code = leak(method(
        "        void nothing = null;\n        iterable items = null;\n        mixed? anything = null;\n        const int? kept = null;\n        kept = 1;\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 A local cannot be `void`: `void` is only a return type.",
            "8:9 This type is not supported yet in PHP#.",
            "9:9 Type `mixed` cannot be nullable.",
            "9:9 This type is not supported yet in PHP#.",
            "11:9 Cannot assign to `kept`: it is declared with `const`.",
        ]
    );
}

#[test]
fn a_typed_for_counter_takes_the_types_of_the_slice_but_not_void() {
    let code = leak(method(
        "        for (void step = null; ; ) {\n        }\n        for (iterable items = null; ; ) {\n        }\n        for (const int? kept = null; ; kept = 1) {\n        }\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:14 A local cannot be `void`: `void` is only a return type.",
            "9:14 This type is not supported yet in PHP#.",
            "11:40 Cannot assign to `kept`: it is declared with `const`.",
        ]
    );
}

#[test]
fn a_nullable_void_reports_only_the_php_error() {
    let code = "class Report\n{\n    public void? run()\n    {\n    }\n}\n";

    assert_eq!(issues(code), ["3:12 Type `void` cannot be nullable."]);
}

#[test]
fn a_by_reference_parameter_is_not_supported_yet() {
    let code = leak(method("        return 1;\n").replace("int extra", "&extra"));

    assert_eq!(issues(code), ["5:20 A by-reference parameter is not supported yet in PHP#."]);
}

#[test]
fn a_dollar_parameter_without_a_type_reports_only_the_dollar_error() {
    let code = leak(method("        return 1;\n").replace("int extra", "$extra"));

    assert_eq!(issues(code), ["5:20 PHP# variables have no `$`: write `extra`."]);
}

#[test]
fn a_void_parameter_reports_only_the_php_error() {
    let code = "class Report\n{\n    public void run(void nothing)\n    {\n    }\n}\n";

    assert_eq!(issues(code), ["3:21 Invalid parameter type: bottom type `void` cannot be used as a parameter type."]);
}

#[test]
fn a_parameter_default_is_a_constant_expression() {
    let code = "class Report\n{\n    public int run(int a = -1 + 2 * PHP_INT_MAX, int b = a, int c = this.run(), bool d = !true)\n    {\n        return a;\n    }\n}\n";

    assert_eq!(
        issues(code),
        ["3:58 This expression is not supported yet in PHP#.", "3:69 This expression is not supported yet in PHP#.",]
    );
}

#[test]
fn a_write_goes_only_to_a_local_a_parameter_or_a_member() {
    let code = leak(method(
        "        1 = 2;\n        FOO = 1;\n        Calc = 1;\n        this = extra;\n        this++;\n        extra.total() = 3;\n        --FOO;\n        FOO *= 2;\n        this.count = 1;\n        extra = 2;\n        extra++;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This write target is not supported yet in PHP#.",
            "8:9 This write target is not supported yet in PHP#.",
            "9:9 This write target is not supported yet in PHP#.",
            "10:9 This write target is not supported yet in PHP#.",
            "11:9 This write target is not supported yet in PHP#.",
            "12:9 This write target is not supported yet in PHP#.",
            "13:11 This write target is not supported yet in PHP#.",
            "14:9 This write target is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_second_namespace_is_not_supported_yet() {
    let code = "namespace A;\n\nclass Y\n{\n}\n\nnamespace B;\n\nimport B.X;\n\nclass X\n{\n}\n";

    assert_eq!(issues(code), ["7:1 This namespace is not supported yet in PHP#."]);
}

#[test]
fn a_braced_or_global_namespace_is_not_supported_yet() {
    for code in ["namespace A\n{\n    class Y\n    {\n    }\n}\n", "namespace\n{\n    class Y\n    {\n    }\n}\n"] {
        assert_eq!(issues(code), ["1:1 This namespace is not supported yet in PHP#."], "{code}");
    }
}

#[test]
fn an_invalid_codepoint_escape_is_an_error_as_in_php() {
    let code =
        leak(method("        const big = \"\\u{110000}\";\n        const empty = \"\\u{}\";\n        return 1;\n"));

    assert_eq!(
        issues(code),
        ["7:21 Invalid UTF-8 codepoint escape sequence.", "8:23 Invalid UTF-8 codepoint escape sequence.",]
    );
}

#[test]
fn new_creates_a_class_written_by_its_short_name_with_its_arguments() {
    let code = leak(method("        const made = new Report(extra, total: 2);\n        return made.run(1);\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn new_outside_the_slice_is_not_supported_yet() {
    let code = leak(method(
        "        const kind = new (Report);\n        const other = new class {};\n        const spread = new Report(...extra);\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:22 This expression is not supported yet in PHP#.",
            "8:23 This expression is not supported yet in PHP#.",
            "9:35 This spread argument is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_spread_argument_is_not_supported_yet() {
    let code = leak(method("        const parts = this.parts();\n        return this.total(...parts);\n"));

    assert_eq!(issues(code), ["8:27 This spread argument is not supported yet in PHP#."]);
}
