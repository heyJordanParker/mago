#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;

use foldhash::HashSet;

use mago_allocator::LocalArena;
use mago_analyzer::Analyzer;
use mago_analyzer::analysis_result::AnalysisResult;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::plugin::context::HookContext;
use mago_analyzer::plugin::hook::ExpressionHook;
use mago_analyzer::plugin::hook::ExpressionHookResult;
use mago_analyzer::plugin::hook::HookResult;
use mago_analyzer::plugin::hook::StaticCall;
use mago_analyzer::plugin::hook::StaticMethodCallHook;
use mago_analyzer::plugin::provider::Provider;
use mago_analyzer::plugin::provider::ProviderMeta;
use mago_analyzer::settings::Settings;
use mago_codex::populator::populate_codebase;
use mago_codex::scanner::scan_program;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_names::CHANGING_COLLECTION_METHODS;
use mago_names::resolver::NameResolver;
use mago_prelude::Prelude;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::parser::parse_file;
use mago_word::WordSet;

static PRELUDE: LazyLock<Prelude> = LazyLock::new(Prelude::build);
static PLUGIN_REGISTRY: LazyLock<PluginRegistry> = LazyLock::new(PluginRegistry::with_library_providers);

const CALC: &str = "<?php\n\nnamespace Lib;\n\nfinal class Calc\n{\n    public static function make(): self\n    {\n        return new self();\n    }\n\n    public function add(int $a, int $b): int\n    {\n        return $a + $b;\n    }\n}\n";

/// The settings every test analyzes with, unless it names its own.
fn settings() -> Settings {
    Settings { find_unused_expressions: true, find_unused_definitions: true, check_throws: true, ..Default::default() }
}

/// Analyzes `analyzed` together with `others`, and returns its issues as `line:column code`.
fn issues(analyzed: (&'static str, &'static str), others: &[(&'static str, &'static str)]) -> Vec<String> {
    issues_with(settings(), analyzed, others)
}

/// Analyzes `analyzed` together with `others` under `settings`, and returns its issues as `line:column code`.
fn issues_with(
    settings: Settings,
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
) -> Vec<String> {
    issues_in(&PLUGIN_REGISTRY, settings, analyzed, others)
}

/// Analyzes `analyzed` together with `others` under `settings` and the plugins of `registry`, and returns its issues
/// as `line:column code`.
fn issues_in(
    registry: &PluginRegistry,
    settings: Settings,
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
) -> Vec<String> {
    let code = analyzed.1;

    analyze(registry, settings, analyzed, others)
        .iter()
        .map(|issue| {
            let offset = issue.primary_span().expect("a primary span").start.offset as usize;
            let line = code[..offset].matches('\n').count() + 1;
            let column = offset - code[..offset].rfind('\n').map_or(0, |newline| newline + 1) + 1;

            format!("{line}:{column} {}", issue.code.as_deref().unwrap_or("none"))
        })
        .collect()
}

/// Analyzes `analyzed` together with `others` under `settings` and the plugins of `registry`, and returns its issues.
fn analyze(
    registry: &PluginRegistry,
    settings: Settings,
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
) -> Vec<Issue> {
    let Prelude { mut database, mut metadata, mut symbol_references } = PRELUDE.clone();

    let file_ids: Vec<_> = std::iter::once(&analyzed)
        .chain(others)
        .map(|(name, code)| {
            (*name, database.add(File::ephemeral(Cow::Borrowed(name.as_bytes()), Cow::Borrowed(code.as_bytes()))))
        })
        .collect();

    let arena = LocalArena::new();
    let mut programs = Vec::new();
    for (name, file_id) in &file_ids {
        let file = database.get_ref(file_id).expect("file was just added");
        let program = parse_file(&arena, file);
        assert!(programs.is_empty() || !program.has_errors(), "{name} did not parse: {:?}", program.errors);

        let names = NameResolver::new(&arena).resolve(program);
        metadata.extend(scan_program(&arena, file, program, &names, settings.version));
        programs.push((file, program, names));
    }

    populate_codebase(&mut metadata, &mut symbol_references, WordSet::default(), HashSet::default());

    let (file, program, names) = &programs[0];
    let mut result = AnalysisResult::new(symbol_references);
    Analyzer::new(&arena, file, names, &metadata, registry, settings)
        .analyze(program, &mut result)
        .expect("analysis succeeds");

    // The analyzed file's parse errors come first, as `mago analyze` reports them beside the analysis.
    program.errors.iter().map(Issue::from).chain(result.issues).collect()
}

#[test]
fn adding_a_mixed_operand_reports_mixed_operand_as_in_php() {
    let any = "<?php\n\nnamespace Lib;\n\nfinal class Any\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Any;\n\nclass Report\n{\n    public static int total()\n    {\n        return Any.value() + 1;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Any;\n\nclass Report\n{\n    public static function total(): int\n    {\n        return Any::value() + 1;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Any.php", any)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Any.php", any)]);

    assert!(codes(&sharp_issues).contains(&"mixed-operand"), "{sharp_issues:?}");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn the_analyzer_reads_the_dialect_from_the_program_not_the_file_name() {
    const CODE: &str = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        let label = \"one\";\n        return extra + label;\n    }\n}\n";
    let Prelude { mut metadata, mut symbol_references, .. } = PRELUDE.clone();
    let settings = Settings::default();
    let arena = LocalArena::new();
    let sharp_file = File::ephemeral(Cow::Borrowed(b"src/Demo/Report.sharp"), Cow::Borrowed(CODE.as_bytes()));
    let php_named_file = File::ephemeral(Cow::Borrowed(b"src/Demo/Report.php"), Cow::Borrowed(CODE.as_bytes()));
    let program = parse_file(&arena, &sharp_file);
    let names = NameResolver::new(&arena).resolve(program);
    metadata.extend(scan_program(&arena, &sharp_file, program, &names, settings.version));
    populate_codebase(&mut metadata, &mut symbol_references, WordSet::default(), HashSet::default());

    let mut result = AnalysisResult::new(symbol_references);
    Analyzer::new(&arena, &php_named_file, &names, &metadata, &PLUGIN_REGISTRY, settings)
        .analyze(program, &mut result)
        .expect("analysis succeeds");

    assert!(result.issues.iter().any(|issue| issue.code.as_deref() == Some("invalid-operand")), "{:#?}", result.issues);
}

/// The binder records a call of a changing collection method as a write to its local, so a lambda captures that
/// local by reference. It knows the methods only by name, so its list must name every collection method without
/// `@mutation-free`, which changes the collection.
#[test]
fn the_binder_knows_every_changing_collection_method() {
    for class in ["Sharp\\ListMethods", "Sharp\\MapMethods"] {
        let metadata =
            PRELUDE.metadata.get_class_like(class.as_bytes()).expect("the collection stub is in the prelude");
        let mut changing: Vec<&str> = metadata
            .methods
            .iter()
            .filter(|method| {
                !PRELUDE
                    .metadata
                    .get_method(class.as_bytes(), method.as_bytes())
                    .expect("a method")
                    .flags
                    .is_mutation_free()
            })
            .map(|method| std::str::from_utf8(method.as_bytes()).expect("an ASCII name"))
            .collect();
        changing.sort_unstable();
        assert!(!changing.is_empty(), "{class} has no changing method, so the stub was not read");

        let known: Vec<&str> =
            changing.iter().copied().filter(|method| CHANGING_COLLECTION_METHODS.contains(method)).collect();
        assert_eq!(known, changing, "{class} changes the collection in a method CHANGING_COLLECTION_METHODS lacks");
    }
}

/// The issue codes alone, in order.
fn codes(issues: &[String]) -> Vec<&str> {
    issues.iter().map(|issue| issue.split_once(' ').map_or("", |(_, code)| code)).collect()
}

#[test]
fn passing_a_string_to_a_php_int_parameter_is_an_invalid_argument_at_the_sharp_position() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        let label = \"one\";\n        const base = 2;\n        const calc = Calc.make();\n        return calc.add(label, base * extra);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        $label = \"one\";\n        $base = 2;\n        $calc = Calc::make();\n        return $calc->add($label, $base * $extra);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(sharp_issues, ["12:25 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn correct_sharp_code_has_no_issues() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        let label = 1;\n        const base = 2;\n        const calc = Calc.make();\n        return calc.add(label, base * extra);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]), Vec::<String>::new());
}

#[test]
fn assigning_to_a_let_local_changes_its_type_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total()\n    {\n        let value = 1;\n        value = \"one\";\n        return value;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(): int\n    {\n        $value = 1;\n        $value = \"one\";\n        return $value;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["9:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn incrementing_and_decrementing_a_let_local_changes_its_type_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string total()\n    {\n        let up = 0;\n        let down = 0;\n        up++;\n        down--;\n        ++up;\n        --down;\n        return up * down;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(): string\n    {\n        $up = 0;\n        $down = 0;\n        $up++;\n        $down--;\n        ++$up;\n        --$down;\n        return $up * $down;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["13:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn this_calls_the_instance_method_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int total()\n    {\n        return this.label();\n    }\n\n    public string label()\n    {\n        return \"one\";\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function total(): int\n    {\n        return $this->label();\n    }\n\n    public function label(): string\n    {\n        return \"one\";\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["7:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_class_in_the_same_namespace_takes_static_calls_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Helper\n{\n    public static int twice(int value)\n    {\n        return value * 2;\n    }\n}\n\nclass Report\n{\n    public static int total()\n    {\n        return Helper.twice(\"one\") - Helper.missing();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Helper\n{\n    public static function twice(int $value): int\n    {\n        return $value * 2;\n    }\n}\n\nclass Report\n{\n    public static function total(): int\n    {\n        return Helper::twice(\"one\") - Helper::missing();\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues[0], "15:29 invalid-argument");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_local_reads_a_missing_property_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total()\n    {\n        const calc = Calc.make();\n        return calc.missing;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function total(): int\n    {\n        $calc = Calc::make();\n        return $calc->missing;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert!(!sharp_issues.is_empty());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn bare_constants_resolve_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total()\n    {\n        return PHP_INT_MAX - MISSING;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(): int\n    {\n        return PHP_INT_MAX - MISSING;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues[0], "7:30 non-existent-constant");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn php_calls_a_sharp_method_by_position_and_by_name() {
    let sharp =
        "namespace Demo;\n\nclass Report\n{\n    public int total(int extra)\n    {\n        return extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace App;\n\nfunction run(\\Demo\\Report $report): int\n{\n    return $report->total(extra: 1) + $report->total(\"one\");\n}\n";

    let php_issues = issues(("src/App/run.php", php), &[("src/Demo/Report.sharp", sharp)]);

    assert_eq!(php_issues, ["7:54 invalid-argument"]);
}

#[test]
fn sharp_arguments_follow_strict_conversion_rules() {
    let money = "<?php\n\nnamespace Lib;\n\nfinal class Money\n{\n    public static function of(float $amount): self\n    {\n        return new self();\n    }\n\n    public static function named(string $name): self\n    {\n        return new self();\n    }\n\n    public function __toString(): string\n    {\n        return 'money';\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Money;\n\nclass Report\n{\n    public static Money total(int extra)\n    {\n        const money = Money.of(extra);\n        return Money.named(money);\n    }\n}\n";

    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Money;\n\nclass Report\n{\n    public static function total(int $extra): Money\n    {\n        $money = Money::of($extra);\n        return Money::named($money);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Money.php", money)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Money.php", money)]);

    assert_eq!(sharp_issues, ["10:28 invalid-argument"]);
    // Mago applies strict conversion rules to every file, whether or not it declares `strict_types=1`.
    assert_eq!(php_issues, ["12:29 invalid-argument"]);
}

/// Spec section 14.3: a method named without parentheses is a function value, typed as PHP types `$calc->add(...)`.
#[test]
fn a_method_named_without_a_call_is_a_closure_of_its_signature() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public int total()\n    {\n        const calc = Calc.make();\n        Function<int(int, int)> add = calc.add;\n        const Function<int()> again = this.total;\n        return add(1, 2) + again() + calc.add(\"x\", 1) + add(\"y\", 2);\n    }\n\n    private int hidden() => 1;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public function total(): int\n    {\n        $calc = Calc::make();\n        $add = $calc->add(...);\n        $again = $this->total(...);\n        return $add(1, 2) + $again() + $calc->add(\"x\", 1) + $add(\"y\", 2);\n    }\n\n    private function hidden(): int { return 1; }\n}\n";
    let others = [("src/Lib/Calc.php", CALC)];

    assert_eq!(
        codes(&issues(("src/Demo/Report.php", php), &others)),
        ["invalid-argument", "invalid-argument", "unused-method"]
    );
    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &others),
        ["12:47 invalid-argument", "12:61 invalid-argument", "15:17 unused-method"]
    );
}

/// A method read as a value obeys the method's visibility, as `$order->secret(...)` does in PHP.
#[test]
fn a_private_method_read_as_a_value_from_outside_its_class_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public Function<int()> run(Order order) => order.secret;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function run(Order $order): \\Closure { return $order->secret(...); }\n}\n";
    let order = "<?php\n\nnamespace Demo;\n\nfinal class Order\n{\n    public function __construct() { $this->secret(); }\n\n    private function secret(): int { return 1; }\n}\n";
    let others = [("src/Demo/Order.php", order)];

    let php_codes =
        codes(&issues(("src/Demo/Report.php", php), &others)).into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(codes(&issues(("src/Demo/Report.sharp", sharp), &others)), php_codes);
    assert!(php_codes.iter().any(|code| code.contains("method")), "{php_codes:?}");
}

#[test]
fn a_local_passed_by_reference_to_a_php_method_changes_as_in_php() {
    let counter = "<?php\n\nnamespace Lib;\n\nfinal class Counter\n{\n    public static function make(): self\n    {\n        return new self();\n    }\n\n    public function fill(?string &$value): void\n    {\n        $value = 'filled';\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Counter;\n\nclass Report\n{\n    public static int total()\n    {\n        let value = null;\n        const counter = Counter.make();\n        counter.fill(value);\n        return value;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Counter;\n\nclass Report\n{\n    public static function total(): int\n    {\n        $value = null;\n        $counter = Counter::make();\n        $counter->fill($value);\n        return $value;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Counter.php", counter)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Counter.php", counter)]);

    assert_eq!(sharp_issues, ["12:16 nullable-return-statement", "12:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn unused_parameters_are_found_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int used, int unused, int _skipped)\n    {\n        return used;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $used, int $unused, int $_skipped): int\n    {\n        return $used;\n    }\n}\n";
    let settings = || Settings { find_unused_parameters: true, ..settings() };

    let sharp_issues = issues_with(settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(settings(), ("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["5:39 unused-parameter"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn unused_parameters_of_an_expression_body_are_found_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int count = 0;\n\n    public Report(private int start, int unused) => this.count = start;\n\n    public int total(int used, int unused) => used;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    private int $count = 0;\n    public function __construct(private int $start, int $unused) { $this->count = $start; }\n\n    public function total(int $used, int $unused): int { return $used; }\n}\n";
    let settings = || Settings { find_unused_parameters: true, ..settings() };

    let sharp_issues = issues_with(settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(settings(), ("src/Demo/Report.php", php), &[]);

    assert_eq!(
        sharp_issues,
        ["7:38 unused-parameter", "9:32 unused-parameter", "7:31 unused-property", "5:17 write-only-property"]
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_for_counter_has_the_type_of_its_value_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string total(int extra)\n    {\n        let total = 0;\n        for (let step = 0; step < extra; step++) {\n            total += step;\n        }\n        return total;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $extra): string\n    {\n        $total = 0;\n        for ($step = 0; $step < $extra; $step++) {\n            $total += $step;\n        }\n        return $total;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["11:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_parameter_read_only_by_a_for_counter_is_used() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int start)\n    {\n        let total = 0;\n        for (let step = start; step < 3; step++) {\n            total += step;\n        }\n        return total;\n    }\n}\n";
    let settings = Settings { find_unused_parameters: true, ..settings() };

    assert_eq!(issues_with(settings, ("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn for_of_loop_variables_have_the_key_and_value_types_as_in_php() {
    let store = "<?php\n\nnamespace Demo;\n\nclass Store\n{\n    /** @return array<int, string> */\n    public static function names(): array\n    {\n        return ['a'];\n    }\n}\n";
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int last()\n    {\n        let found = 0;\n        for (const [key, name] of Store.names()) {\n            found = key;\n            found = name;\n        }\n        return found;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function last(): int\n    {\n        $found = 0;\n        foreach (Store::names() as $key => $name) {\n            $found = $key;\n            $found = $name;\n        }\n        return $found;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Demo/Store.php", store)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Demo/Store.php", store)]);

    assert_eq!(sharp_issues, ["12:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_parameter_read_only_by_a_for_of_collection_is_used() {
    let store = "<?php\n\nnamespace Demo;\n\nclass Store\n{\n    /** @return list<int> */\n    public function values(): array\n    {\n        return [1];\n    }\n}\n";
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(Store source)\n    {\n        let total = 0;\n        for (const value of source.values()) {\n            total += value;\n        }\n        return total;\n    }\n}\n";
    let settings = Settings { find_unused_parameters: true, ..settings() };

    assert_eq!(
        issues_with(settings, ("src/Demo/Report.sharp", sharp), &[("src/Demo/Store.php", store)]),
        Vec::<String>::new()
    );
}

#[test]
fn null_coalescing_a_local_narrows_it_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? first, int? second)\n    {\n        if (first !== null || second !== null) {\n            return first ?? second;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(?int $first, ?int $second): int\n    {\n        if ($first !== null || $second !== null) {\n            return $first ?? $second;\n        }\n        return 0;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn nullable_values_flow_in_and_out_with_no_issues() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static Calc? find(int? id, Calc? fallback = null)\n    {\n        if (id === null) {\n            return null;\n        }\n        return fallback;\n    }\n\n    public static int? total()\n    {\n        const found = Report.find(null);\n        if (found === null) {\n            return null;\n        }\n        return found.add(1, 2);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]), Vec::<String>::new());
}

#[test]
fn null_coalescing_assignment_makes_a_local_non_null_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? extra)\n    {\n        let total = extra;\n        total ??= 0;\n        extra ??= 1;\n        return total + extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(?int $extra): int\n    {\n        $total = $extra;\n        $total ??= 0;\n        $extra ??= 1;\n        return $total + $extra;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn null_safe_access_gives_a_nullable_value_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int? maybe(Calc? calc)\n    {\n        return calc?.add(1, 2);\n    }\n\n    public static int total(Calc? calc)\n    {\n        return calc?.add(1, 2);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function maybe(?Calc $calc): ?int\n    {\n        return $calc?->add(1, 2);\n    }\n\n    public static function total(?Calc $calc): int\n    {\n        return $calc?->add(1, 2);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(sharp_issues, ["14:16 nullable-return-statement", "14:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_method_read_through_null_safe_access_is_a_closure_or_null() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(Calc? calc)\n    {\n        Function<int(int, int)>? add = calc?.add;\n        const Function<int(int, int)> call = add ?? ((a, b) => 0);\n        return call(1, 2);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]), Vec::<String>::new());
}

#[test]
fn a_nullable_value_where_a_value_is_required_is_reported_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(int? extra, Calc? calc)\n    {\n        const sum = calc.add(extra, 1);\n        return extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function total(?int $extra, ?Calc $calc): int\n    {\n        $sum = $calc->add($extra, 1);\n        return $extra;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(
        sharp_issues,
        [
            "9:21 possible-method-access-on-null",
            "9:30 possibly-null-argument",
            "9:15 mixed-assignment",
            "10:16 nullable-return-statement",
            "10:16 invalid-return-statement",
        ]
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_typed_local_takes_values_of_its_written_type_with_no_issues() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(int? value, bool make)\n    {\n        Calc? found = null;\n        if (make) {\n            found = Calc.make();\n        }\n        int? first = null;\n        first ??= value;\n        const int base = 10;\n        int total = base;\n        total += first ?? 1;\n        return found?.add(total, 1) ?? total;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]), Vec::<String>::new());
}

#[test]
fn locals_of_sibling_blocks_keep_their_own_written_types() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static void total(bool flag)\n    {\n        if (flag) {\n            int value = 1;\n            value = 2;\n        } else {\n            string value = \"one\";\n            value = \"two\";\n        }\n        {\n            float value = 1.5;\n            value = 2.5;\n        }\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_value_that_is_not_the_written_type_of_a_local_is_reported() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? value)\n    {\n        int count = \"one\";\n        count = 2;\n        count = 2.5;\n        count = value;\n        const string label = 1;\n        return count;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "7:21 invalid-local-assignment-value",
            "9:17 invalid-local-assignment-value",
            "10:17 invalid-local-assignment-value",
            "11:30 invalid-local-assignment-value",
        ]
    );

    let first = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]).remove(0);
    assert_eq!(first.message, "Invalid assignment to `count`: it is declared as `int`.");
}

#[test]
fn a_typed_for_counter_takes_only_values_of_its_written_type() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? extra)\n    {\n        for (int step = \"one\"; step < 3; step++) {\n        }\n        for (int? found = null; found === null; ) {\n            found = extra;\n        }\n        for (int count = 0; count < 3; count++) {\n            count = 1.5;\n        }\n        return 0;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["7:25 invalid-local-assignment-value", "13:21 invalid-local-assignment-value"]
    );
}

#[test]
fn the_nullable_return_help_writes_the_nullable_type_as_the_file_does() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? extra)\n    {\n        return extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(?int $extra): int\n    {\n        return $extra;\n    }\n}\n";

    let help = |analyzed| {
        analyze(&PLUGIN_REGISTRY, settings(), analyzed, &[])
            .into_iter()
            .find(|issue| issue.code.as_deref() == Some("nullable-return-statement"))
            .and_then(|issue| issue.help)
            .expect("a nullable-return-statement help")
    };

    assert!(help(("src/Demo/Report.sharp", sharp)).contains("(e.g., 'int?')"));
    assert!(help(("src/Demo/Report.php", php)).contains("(e.g., '?int')"));
}

#[test]
fn assigning_a_property_of_one_local_keeps_the_memoized_calls_of_another_as_in_php() {
    let box_class = "<?php\n\nnamespace Lib;\n\nfinal class Box\n{\n    public int $value = 0;\n\n    /** @mutation-free */\n    public function count(): ?int\n    {\n        return null;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Box;\n\nclass Report\n{\n    public static int total(Box box, Box other)\n    {\n        if (other.count() !== null) {\n            box.value = 1;\n            return other.count();\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Box;\n\nclass Report\n{\n    public static function total(Box $box, Box $other): int\n    {\n        if ($other->count() !== null) {\n            $box->value = 1;\n            return $other->count();\n        }\n        return 0;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Box.php", box_class)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Box.php", box_class)]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_sharp_import_passes_the_use_statement_and_casing_checks_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\nimport Lib.Money;\nimport Lib.missing;\n\nclass Report\n{\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\nuse Lib\\Money;\nuse Lib\\missing;\n\nclass Report\n{\n}\n";
    let money = "<?php\n\nnamespace Lib;\n\nfinal class money\n{\n}\n";
    let settings = || Settings { check_use_statements: true, check_name_casing: true, ..settings() };

    let others = [("src/Lib/Calc.php", CALC), ("src/Lib/money.php", money)];

    let sharp_issues = issues_with(settings(), ("src/Demo/Report.sharp", sharp), &others);
    let php_issues = issues_with(settings(), ("src/Demo/Report.php", php), &others);

    assert_eq!(sharp_issues, ["4:8 incorrect-class-like-casing", "5:8 non-existent-use-import"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_field_is_the_php_property_of_the_same_name() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int count = 0;\n    protected string label = \"one\";\n\n    public int total(int extra)\n    {\n        this.count += extra;\n        this.label = 2;\n        return this.count;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    private int $count = 0;\n    protected string $label = \"one\";\n\n    public function total(int $extra): int\n    {\n        $this->count += $extra;\n        $this->label = 2;\n        return $this->count;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["11:22 invalid-property-assignment-value"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn new_checks_the_constructor_arguments_as_in_php() {
    let money = "<?php\n\nnamespace Lib;\n\nfinal class Money\n{\n    public function __construct(public int $cents)\n    {\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Money;\n\nclass Report\n{\n    public static int total()\n    {\n        const money = new Money(\"one\");\n        return new Money(cents: 2).cents + money.cents;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Money;\n\nclass Report\n{\n    public static function total(): int\n    {\n        $money = new Money(\"one\");\n        return new Money(cents: 2)->cents + $money->cents;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Money.php", money)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Money.php", money)]);

    assert_eq!(sharp_issues, ["9:33 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn the_constructor_is_the_php_constructor() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int count;\n\n    public Report(int start)\n    {\n        this.count = start;\n    }\n\n    public int total()\n    {\n        return this.count;\n    }\n\n    public static int make()\n    {\n        return new Report(\"one\").total() + new Report(start: 2).total();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    private int $count;\n\n    public function __construct(int $start)\n    {\n        $this->count = $start;\n    }\n\n    public function total(): int\n    {\n        return $this->count;\n    }\n\n    public static function make(): int\n    {\n        return new Report(\"one\")->total() + new Report(start: 2)->total();\n    }\n}\n";
    let caller =
        "<?php\n\nnamespace App;\n\nfunction run(): int\n{\n    return (new \\Demo\\Report(\"one\"))->total();\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["19:27 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
    assert_eq!(issues(("src/App/run.php", caller), &[("src/Demo/Report.sharp", sharp)]), ["7:30 invalid-argument"]);
}

#[test]
fn an_auto_property_is_the_php_property_with_its_set_visibility() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; set; }\n    public int id { get; }\n\n    public Report(int id)\n    {\n        this.id = id;\n        this.name = \"report\";\n    }\n\n    public int bump()\n    {\n        this.views++;\n        return this.views + this.id;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public private(set) int $views = 0;\n    public string $name;\n    public readonly int $id;\n\n    public function __construct(int $id)\n    {\n        $this->id = $id;\n        $this->name = \"report\";\n    }\n\n    public function bump(): int\n    {\n        $this->views++;\n        return $this->views + $this->id;\n    }\n}\n";
    let caller = "<?php\n\nnamespace App;\n\nfunction run(\\Demo\\Report $report): string\n{\n    $report->name = 'other';\n    $report->views = 2;\n    $report->id = 3;\n    return $report->name . $report->views . $report->id;\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);
    let caller_of_sharp = issues(("src/App/run.php", caller), &[("src/Demo/Report.sharp", sharp)]);
    let caller_of_php = issues(("src/App/run.php", caller), &[("src/Demo/Report.php", php)]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
    assert_eq!(caller_of_sharp.len(), 2, "{caller_of_sharp:?}");
    assert_eq!(caller_of_sharp, caller_of_php);
}

#[test]
fn a_promoted_member_is_the_promoted_php_property() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public Report(private int count, public int id { get; }, public string name { get; protected set; })\n    {\n    }\n\n    public int total()\n    {\n        return this.count + this.id;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function __construct(private int $count, public readonly int $id, public protected(set) string $name)\n    {\n    }\n\n    public function total(): int\n    {\n        return $this->count + $this->id;\n    }\n}\n";
    let caller = "<?php\n\nnamespace App;\n\nfunction run(\\Demo\\Report $report): string\n{\n    $report->id = 3;\n    $report->name = 'other';\n    return $report->name . $report->id;\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);
    let caller_of_sharp = issues(("src/App/run.php", caller), &[("src/Demo/Report.sharp", sharp)]);
    let caller_of_php = issues(("src/App/run.php", caller), &[("src/Demo/Report.php", php)]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
    assert_eq!(caller_of_sharp.len(), 2, "{caller_of_sharp:?}");
    assert_eq!(caller_of_sharp, caller_of_php);
}

#[test]
fn an_initial_value_that_is_not_constant_is_checked_against_the_member_type() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private Clock clock = new Clock();\n    public int ticks { get; private set; } = new Clock().tick();\n    private string label = new Clock();\n\n    public string read()\n    {\n        return this.label;\n    }\n\n    public Clock now()\n    {\n        return this.clock;\n    }\n}\n\nclass Clock\n{\n    public int tick()\n    {\n        return 1;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["7:28 invalid-property-default-value"]);
}

#[test]
fn a_get_only_property_is_readonly_and_set_once_in_the_constructor() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public string code { get; } = \"none\";\n    public int count { get; }\n\n    public Report(public int id { get; }, int start)\n    {\n        this.count = start;\n        this.count = 2;\n        this.id = 3;\n        this.code = \"x\";\n    }\n\n    public void reset(Report other)\n    {\n        other.count = 1;\n        this.count = 0;\n    }\n}\n";

    // The first write in the constructor sets `count`. Every other write would throw `Error` on `readonly`: a second
    // write, a property set from its parameter or its initial value, another object's property, and a write outside
    // the constructor.
    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "11:14 invalid-property-write",
            "12:14 invalid-property-write",
            "13:14 invalid-property-write",
            "18:15 invalid-property-write",
            "19:14 invalid-property-write",
        ]
    );
}

/// A parse error reports its spot once, so returning what failed to parse adds no `never-return`, in PHP# and PHP.
#[test]
fn returning_a_value_that_failed_to_parse_adds_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public Report make(string json)\n    {\n        return new Report.fromJson(json);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfunction make(): int\n{\n    return );\n}\n";

    assert_eq!(issues(("src/Demo/make.php", php), &[]), ["7:12 parse"]);
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["7:20 parse"]);
}

#[test]
fn a_catch_variable_has_the_caught_classes_as_its_type_as_in_php() {
    let sharp = "namespace Demo;\n\nimport RuntimeException;\nimport LogicException;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        try {\n            if (extra < 0) {\n                throw new RuntimeException(\"negative\");\n            }\n        } catch (RuntimeException | LogicException failure) {\n            return failure.getLine();\n        } finally {\n            extra += 1;\n        }\n        return extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse RuntimeException;\nuse LogicException;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        try {\n            if ($extra < 0) {\n                throw new RuntimeException(\"negative\");\n            }\n        } catch (RuntimeException | LogicException $failure) {\n            return $failure->getLine();\n        } finally {\n            $extra += 1;\n        }\n        return $extra;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn calling_php_built_in_functions_has_no_issues() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        const name = sprintf(\"%d items\", count([extra, 2], mode: 0));\n        return strlen(name) + strlen(random_bytes(extra + 1));\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

/// Spec section 31 makes an exception a bug and an expected failure a `Result`, so PHP# declares no thrown exceptions.
#[test]
fn check_throws_skips_sharp_files_and_keeps_reporting_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string token()\n    {\n        return random_bytes(16);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function token(): string\n    {\n        return random_bytes(16);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["9:16 unhandled-thrown-type"]);
}

#[test]
fn a_template_is_a_string_as_php_interpolation_is() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string label(int count, string name)\n    {\n        return `${name} has ${count + 1} items`;\n    }\n\n    public static int size(string name)\n    {\n        return `${name}`;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function label(int $count, string $name): string\n    {\n        return \"{$name} has \" . ($count + 1) . \" items\";\n    }\n\n    public static function size(string $name): int\n    {\n        return \"{$name}\";\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["12:16 invalid-return-statement"]);
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["14:16 invalid-return-statement"]);
}

#[test]
fn a_global_function_the_app_declares_checks_its_arguments_as_in_php() {
    let helpers = "<?php\n\nfunction helper(int $value): int\n{\n    return $value;\n}\n";
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        return helper(extra) + helper(\"one\");\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        return helper($extra) + helper(\"one\");\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/helpers.php", helpers)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/helpers.php", helpers)]);

    assert_eq!(sharp_issues, ["7:39 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn calling_a_function_that_does_not_exist_is_reported_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        return missing(extra);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        return missing($extra);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["7:16 non-existent-function", "7:16 mixed-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// The engine calls a bare function name as the global function, so a call the analyzer resolves to a namespaced
/// function would run another function than the one it checked.
#[test]
fn calling_a_namespaced_function_is_not_supported_yet() {
    let helpers = "<?php\n\nnamespace Demo;\n\nfunction helper(int $value): int\n{\n    return $value;\n}\n\nfunction strlen(string $value): int\n{\n    return 0;\n}\n";
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        return helper(extra) + strlen(\"one\");\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        return helper($extra) + strlen(\"one\");\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Demo/helpers.php", helpers)]),
        ["7:16 not-supported-yet", "7:32 not-supported-yet"]
    );
    assert_eq!(issues(("src/Demo/Report.php", php), &[("src/Demo/helpers.php", helpers)]), Vec::<String>::new());
}

#[test]
fn plus_joins_two_strings_into_the_string_dot_gives_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(string name)\n    {\n        const label = \"Order \" + name + `!`;\n        let line = label;\n        line += \"\\n\";\n        return line;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(string $name): int\n    {\n        $label = \"Order \" . $name . \"!\";\n        $line = $label;\n        $line .= \"\\n\";\n        return $line;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["10:16 invalid-return-statement"]);
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["12:16 invalid-return-statement"]);
}

#[test]
fn exponentiation_has_the_type_it_has_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int power(int base, int exponent, float rate)\n    {\n        let result = base ** exponent;\n        result **= 2;\n        return result + rate ** 2;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function power(int $base, int $exponent, float $rate): int\n    {\n        $result = $base ** $exponent;\n        $result **= 2;\n        return $result + $rate ** 2;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert!(!php_issues.is_empty(), "{php_issues:?}");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn plus_on_a_string_and_a_value_that_may_not_be_one_is_an_invalid_operand() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string total(int count, float rate, string name, string? maybe)\n    {\n        const a = name + count;\n        const b = rate + name;\n        const c = maybe + name;\n        const d = count + rate;\n        return name;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["7:19 invalid-operand", "8:19 invalid-operand", "9:19 invalid-operand"]
    );
}

#[test]
fn a_ternary_with_a_bool_condition_has_the_type_it_has_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string? host(string host, bool secure)\n    {\n        const scheme = secure ? \"https\" : \"http\";\n        return host != \"\" ? scheme + host : (secure ? 1 : null);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function host(string $host, bool $secure): ?string\n    {\n        $scheme = $secure ? \"https\" : \"http\";\n        return $host != \"\" ? $scheme . $host : ($secure ? 1 : null);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert!(!php_issues.is_empty(), "{php_issues:?}");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_ternary_whose_condition_is_not_bool_is_an_invalid_operand() {
    let any = "<?php\n\nnamespace Lib;\n\nfinal class Any\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Any;\n\nclass Report\n{\n    public static int pick(int count, string name, bool? maybe)\n    {\n        const a = count ? 1 : 2;\n        const b = name ? 1 : 2;\n        const c = maybe ? 1 : 2;\n        const d = Any.value() ? 1 : 2;\n        const e = count ?: 2;\n        return a + b + c + d + e;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Any.php", any)]),
        ["9:19 invalid-operand", "10:19 invalid-operand", "11:19 invalid-operand", "12:19 invalid-operand"]
    );
}

#[test]
fn every_condition_keeps_testing_truthiness_in_php() {
    let php = "<?php\n\nnamespace Demo;\n\nfunction pick(int $count, string $name, int $a, string $b): int\n{\n    if ($count) {\n        $count++;\n    } elseif ($name) {\n        $count--;\n    }\n    while ($count) {\n        $count--;\n    }\n    do {\n        $count++;\n    } while ($count < 3 && $name);\n    for ($i = 3; $i; $i--) {\n        $count++;\n    }\n    return $a || !$b ? $count : 2;\n}\n";

    assert_eq!(issues(("src/Demo/report.php", php), &[]), Vec::<String>::new());
}

#[test]
fn every_condition_that_is_bool_has_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int pick(int count, string name, bool on)\n    {\n        if (count > 0) {\n            count++;\n        } else if (name != \"\") {\n            count--;\n        }\n        while (count > 10 && on) {\n            count--;\n        }\n        do {\n            count++;\n        } while (count < 3 || !on);\n        for (let i = 3; i > 0; i--) {\n            count++;\n        }\n        while (true) {\n            break;\n        }\n        return on ? count : 0;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_condition_that_is_not_bool_is_an_invalid_operand_everywhere() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int pick(int count, string name, bool on, int n, string s)\n    {\n        if (count) {\n            count++;\n        } else if (name) {\n            count--;\n        }\n        while (count) {\n            count--;\n        }\n        do {\n            count++;\n        } while (count % 3);\n        for (let i = 3; i; i--) {\n            count += s ? 1 : 2;\n        }\n        const a = n && on;\n        const b = on || s;\n        const c = !n;\n        return a && b && c ? 1 : 0;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "7:13 invalid-operand",
            "9:20 invalid-operand",
            "12:16 invalid-operand",
            "17:18 invalid-operand",
            "19:22 invalid-operand",
            "18:25 invalid-operand",
            "21:19 invalid-operand",
            "22:25 invalid-operand",
            "23:20 invalid-operand",
        ]
    );
}

#[test]
fn casts_between_numbers_have_the_types_they_have_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string cents(float price, int count)\n    {\n        const cents = (int)(price * 100);\n        const share = (float)count / 3;\n        return (string)cents + (string)share + (string)(int)share;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function cents(float $price, int $count): string\n    {\n        $cents = (int)($price * 100);\n        $share = (float)$count / 3;\n        return (string)$cents . (string)$share . (string)(int)$share;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_cast_of_a_value_that_is_not_a_number_is_an_invalid_operand() {
    let any = "<?php\n\nnamespace Lib;\n\nfinal class Any\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Any;\n\nclass Report\n{\n    public static string pick(string name, bool flag, int? maybe)\n    {\n        const a = (int)name;\n        const b = (float)flag;\n        const c = (string)name;\n        const d = (int)maybe;\n        const e = (string)Any.value();\n        return `${a}${b}${c}${d}${e}`;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Any.php", any)]),
        [
            "9:19 invalid-operand",
            "10:19 invalid-operand",
            "11:19 invalid-operand",
            "12:19 invalid-operand",
            "13:19 invalid-operand",
        ]
    );
}

#[test]
fn int_and_float_parse_have_the_types_of_the_sharp_library_classes() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int count(string text, int? fallback)\n    {\n        const price = Float.parse(text) + (Float.tryParse(fallback) ?? 0.0);\n        const count = Int.parse(text) + (Int.tryParse(null) ?? 0);\n        return price > 1.0 ? count : Int.tryParse(text);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function count(string $text, ?int $fallback): int\n    {\n        $price = \\Sharp\\Float::parse($text) + (\\Sharp\\Float::tryParse($fallback) ?? 0.0);\n        $count = \\Sharp\\Int::parse($text) + (\\Sharp\\Int::tryParse(null) ?? 0);\n        return $price > 1.0 ? $count : \\Sharp\\Int::tryParse($text);\n    }\n}\n";

    // `check_throws` skips `.sharp` files, so the PHP twin is analyzed without it.
    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(Settings { check_throws: false, ..settings() }, ("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["9:16 nullable-return-statement", "9:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn expression_bodies_and_computed_properties_are_checked_as_php_checks_their_twins() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private string name = \"\";\n    private int count = 0;\n\n    public Report(string name) => this.name = name;\n\n    public string slug => strtolower(this.name);\n    public int size => this.name;\n    public int total() => this.count + 1;\n    public string label() => this.count;\n    public void touch() => this.count++;\n    public void reset(int unused) => this.count = 0;\n    public void rename() => this.slug = \"x\";\n}\n";
    let php = "<?php\n\nnamespace Demo;\nclass Report\n{\n    private string $name = \"\";\n    private int $count = 0;\n\n    public function __construct(string $name) { $this->name = $name; }\n\n    public string $slug { get => strtolower($this->name); }\n    public int $size { get => $this->name; }\n    public function total(): int { return $this->count + 1; }\n    public function label(): string { return $this->count; }\n    public function touch(): void { $this->count++; }\n    public function reset(int $unused): void { $this->count = 0; }\n    public function rename(): void { $this->slug = \"x\"; }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(
        sharp_issues,
        ["11:24 invalid-return-statement", "13:30 invalid-return-statement", "16:34 invalid-property-write"]
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn plus_keeps_adding_numbers_and_rejecting_nothing_new_in_php() {
    let php = "<?php\n\nnamespace Demo;\n\nfunction total(int $count, string $name): int|float\n{\n    return $count + \"1\";\n}\n";

    assert_eq!(issues(("src/Demo/report.php", php), &[]), Vec::<String>::new());
}

/// Each hook event, with the spans of the call's class, method, arguments and whole call.
type StaticCallEvents = Vec<(&'static str, [Span; 4])>;

/// Records every static call its hooks see.
#[derive(Clone, Default)]
struct StaticCallRecorder(Arc<Mutex<StaticCallEvents>>);

impl Provider for StaticCallRecorder {
    fn meta() -> &'static ProviderMeta {
        static META: ProviderMeta = ProviderMeta::new("test::static-call", "static call", "Records static calls.");

        &META
    }
}

impl StaticCallRecorder {
    fn record(&self, event: &'static str, call: &StaticCall<'_, '_>) {
        let spans = [call.class.span(), call.method.span(), call.argument_list.span(), call.span];
        self.0.lock().unwrap().push((event, spans));
    }

    /// The recorded calls, each span read back from `code`.
    fn seen(&self, code: &str) -> Vec<String> {
        let text = |span: Span| &code[span.start.offset as usize..span.end.offset as usize];

        self.0.lock().unwrap().iter().map(|(event, spans)| format!("{event} {}", spans.map(text).join(" "))).collect()
    }
}

impl StaticMethodCallHook for StaticCallRecorder {
    fn before_static_method_call(
        &self,
        call: &StaticCall<'_, '_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<ExpressionHookResult> {
        self.record("before", call);

        Ok(ExpressionHookResult::Continue)
    }

    fn after_static_method_call(
        &self,
        call: &StaticCall<'_, '_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<()> {
        self.record("after", call);

        Ok(())
    }
}

/// Records each method call an expression hook sees, as static or instance by the binding of its object.
#[derive(Clone, Default)]
struct CallKindRecorder(Arc<Mutex<Vec<String>>>);

impl Provider for CallKindRecorder {
    fn meta() -> &'static ProviderMeta {
        static META: ProviderMeta = ProviderMeta::new("test::call-kind", "call kind", "Records method call kinds.");

        &META
    }
}

impl ExpressionHook for CallKindRecorder {
    fn before_expression(
        &self,
        expression: &Expression<'_>,
        context: &mut HookContext<'_, '_>,
    ) -> HookResult<ExpressionHookResult> {
        if let Expression::Call(Call::Method(call)) = expression
            && let ClassLikeMemberSelector::Identifier(method) = &call.method
        {
            let is_static = StaticCall::from_method_call(call, context.resolved_names()).is_some();
            let kind = if is_static { "static" } else { "instance" };
            self.record(format!("{} {kind}", String::from_utf8_lossy(method.value)));
        }

        Ok(ExpressionHookResult::Continue)
    }
}

impl CallKindRecorder {
    fn record(&self, call: String) {
        self.0.lock().unwrap().push(call);
    }
}

#[test]
fn an_expression_hook_tells_a_sharp_static_call_by_the_binding_of_its_object() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total()\n    {\n        return Calc.make().add(1, 2);\n    }\n}\n";
    let recorder = CallKindRecorder::default();
    let mut registry = PluginRegistry::default();
    registry.register_expression_hook(recorder.clone());

    let issues = issues_in(&registry, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(issues, Vec::<String>::new());
    assert_eq!(*recorder.0.lock().unwrap(), ["add instance", "make static"]);
}

#[test]
fn static_call_hooks_see_a_sharp_static_call_as_its_parts_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static Calc total()\n    {\n        return Calc.make();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function total(): Calc\n    {\n        return Calc::make();\n    }\n}\n";

    let seen = |analyzed: (&'static str, &'static str)| {
        let recorder = StaticCallRecorder::default();
        let mut registry = PluginRegistry::default();
        registry.register_static_method_call_hook(recorder.clone());

        let issues = issues_in(&registry, settings(), analyzed, &[("src/Lib/Calc.php", CALC)]);
        assert_eq!(issues, Vec::<String>::new());

        recorder.seen(analyzed.1)
    };

    assert_eq!(
        seen(("src/Demo/Report.sharp", sharp)),
        ["before Calc make () Calc.make()", "after Calc make () Calc.make()"]
    );
    assert_eq!(
        seen(("src/Demo/Report.php", php)),
        ["before Calc make () Calc::make()", "after Calc make () Calc::make()"]
    );
}

#[test]
fn a_lambda_reads_and_writes_the_locals_around_it_as_its_php_closure_does() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        let count = 0;\n        const add = (int a, int b) => a + b + extra;\n        const increment = () => { count += 1; };\n        increment();\n        return add(count, 1);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function run(int $extra): int\n    {\n        $count = 0;\n        $add = fn (int $a, int $b) => $a + $b + $extra;\n        $increment = function () use (&$count) { $count += 1; };\n        $increment();\n        return $add($count, 1);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_lambda_called_with_a_wrong_argument_or_returning_a_wrong_type_is_an_error_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        const twice = (int a) => a * 2;\n        const label = () => \"none\";\n        twice(\"x\");\n        return label();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function run(int $extra): int\n    {\n        $twice = fn (int $a) => $a * 2;\n        $label = fn () => \"none\";\n        $twice(\"x\");\n        return $label();\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);

    assert_eq!(sharp_issues, ["9:15 invalid-argument", "10:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&issues(("src/Demo/Report.php", php), &[])));
}

#[test]
fn a_lambda_passed_to_php_takes_its_parameter_types_from_the_php_signature_as_in_php() {
    let numbers = "<?php\n\nnamespace Lib;\n\nfinal class Numbers\n{\n    /**\n     * @param \\Closure(int): bool $keep\n     */\n    public static function count(\\Closure $keep): int\n    {\n        return $keep(1) ? 1 : 0;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Numbers;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        return Numbers.count(n => n > extra) + Numbers.count((string s) => s == \"\");\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Numbers;\n\nclass Report\n{\n    public function run(int $extra): int\n    {\n        return Numbers::count(fn ($n) => $n > $extra) + Numbers::count(fn (string $s) => $s == \"\");\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Numbers.php", numbers)]);

    assert_eq!(sharp_issues, ["9:62 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&issues(("src/Demo/Report.php", php), &[("src/Lib/Numbers.php", numbers)])));
}

#[test]
fn a_function_type_is_a_closure_type_that_takes_lambdas_and_calls_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private Function<int(int)> scale;\n\n    public Report(int factor)\n    {\n        this.scale = n => n * factor;\n    }\n\n    public Function<bool(int)> above(int floor) => n => n > floor;\n\n    public int run(int extra)\n    {\n        const scale = this.scale;\n        const check = this.above(extra);\n        Function<int(int)> twice = n => n * 2;\n        return check(twice(scale(extra))) ? 1 : 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    /** @var \\Closure(int): int */\n    private \\Closure $scale;\n\n    public function __construct(int $factor)\n    {\n        $this->scale = fn (int $n) => $n * $factor;\n    }\n\n    /** @return \\Closure(int): bool */\n    public function above(int $floor): \\Closure { return fn (int $n) => $n > $floor; }\n\n    public function run(int $extra): int\n    {\n        $scale = $this->scale;\n        $check = $this->above($extra);\n        $twice = fn (int $n) => $n * 2;\n        return $check($twice($scale($extra))) ? 1 : 0;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_function_type_refuses_a_lambda_of_another_type_and_a_string_or_array_callable() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int apply(Function<int(int)> step, int value) => step(value);\n\n    public static int run(int extra)\n    {\n        Report.apply((string s) => 1, extra);\n        Report.apply(n => \"text\", extra);\n        Report.apply(\"abs\", extra);\n        return Report.apply([\"Demo\\\\Report\", \"run\"], extra);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["9:22 invalid-argument", "10:22 invalid-argument", "11:22 invalid-argument", "12:29 invalid-argument"]
    );
}

/// Spec section 12's methods that take a function give each lambda the element type, and type what they return from
/// it: `filter` and `sortedBy` keep the element type, `map` and `sumOf` take the lambda's return type, `groupBy` and
/// `associateBy` key a map by it, and `filterValues` keeps a map's keys.
#[test]
fn collection_methods_that_take_a_lambda_type_its_parameter_and_their_result() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(List<int> numbers, Map<string, int> prices)\n    {\n        List<int> evens = numbers.filter(n => n % 2 == 0);\n        List<string> labels = numbers.map(n => `#${n}`);\n        int total = evens.sumOf(n => n * 2);\n        List<string> sorted = labels.sortedBy(s => strlen(s));\n        Map<int, List<int>> groups = numbers.groupBy(n => n % 3);\n        Map<string, string> byLabel = labels.associateBy(s => s);\n        Map<string, int> cheap = prices.filterValues(p => p < 100);\n        List<int> kept = prices.filter(p => p > 0);\n        List<float> halves = prices.map(p => p / 2);\n        int first = numbers.first(n => n > 2);\n        bool expensive = prices.any(p => p > 1000);\n        return total + first + count(sorted) + count(groups) + count(byLabel) + count(cheap) + count(kept) + count(halves) + (expensive ? 1 : 0);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

/// A method value passed to a collection method types what it returns, as a lambda does.
#[test]
fn a_method_value_passed_to_a_collection_method_types_its_result() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public List<string> run(List<int> numbers)\n    {\n        List<string> labels = numbers.map(this.label);\n        List<int> wrong = numbers.map(this.label);\n        return labels;\n    }\n\n    private string label(int n) => (string) n;\n}\n";

    assert_eq!(codes(&issues(("src/Demo/Report.sharp", sharp), &[])), ["invalid-local-assignment-value"]);
}

/// A lambda passed to a collection method is checked against the element type, as any argument is.
#[test]
fn a_collection_method_refuses_a_lambda_of_another_element_type() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(List<int> numbers)\n    {\n        List<int> kept = numbers.filter((string s) => s == \"\");\n        List<string> labels = numbers.map(n => n * 2);\n        return count(kept) + count(labels);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["7:41 invalid-argument", "8:31 invalid-local-assignment-value"]
    );
}

/// Spec section 14 calls a property that holds a function as a method: `x.priceOf(line)` calls the property when the
/// class has a property `priceOf` and no method `priceOf`, on `this`, on another object and on an inherited
/// property, and checks the call against the property's function type as `($x->priceOf)($line)` is checked in PHP.
#[test]
fn a_call_of_a_property_holding_a_function_is_checked_against_its_function_type() {
    let pricing = "<?php\n\nnamespace Lib;\n\nabstract class BasePricing\n{\n    /** @var \\Closure(int): int */\n    public \\Closure $priceOf;\n}\n\nfinal class Pricing extends BasePricing\n{\n    public function __construct()\n    {\n        $this->priceOf = fn (int $amount): int => $amount * 2;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Pricing;\n\nclass Report\n{\n    private Function<int(int)> scale;\n\n    public Report(int factor)\n    {\n        this.scale = n => n * factor;\n    }\n\n    public int run(Pricing pricing, int extra) => this.scale(extra) + pricing.priceOf(extra);\n\n    public int wrong(Pricing pricing) => this.scale(\"x\") + strlen(pricing.priceOf(1));\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Pricing;\n\nclass Report\n{\n    /** @var \\Closure(int): int */\n    private \\Closure $scale;\n\n    public function __construct(int $factor)\n    {\n        $this->scale = fn (int $n) => $n * $factor;\n    }\n\n    public function run(Pricing $pricing, int $extra): int { return ($this->scale)($extra) + ($pricing->priceOf)($extra); }\n\n    public function wrong(Pricing $pricing): int { return ($this->scale)(\"x\") + strlen(($pricing->priceOf)(1)); }\n}\n";
    let others = [("src/Lib/Pricing.php", pricing)];

    let codes = |issues: Vec<String>| -> Vec<String> {
        issues.into_iter().map(|issue| issue.split_once(' ').unwrap().1.to_owned()).collect()
    };
    let php_issues = codes(issues(("src/Demo/Report.php", php), &others));

    assert_eq!(php_issues, ["invalid-argument", "invalid-argument"]);
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &others), ["16:53 invalid-argument", "16:67 invalid-argument"]);
}

#[test]
fn attribute_arguments_are_checked_against_the_attribute_constructor_as_in_php() {
    let field = "<?php\n\nnamespace Lib;\n\n#[\\Attribute]\nfinal class Field\n{\n    public function __construct(public string $label, public int $width = 1)\n    {\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Field;\n\n[Field(\"Report\")]\nclass Report\n{\n    [Field(label: 3)] private int count = 0;\n\n    [Field(\"run\", width: \"wide\")]\n    public int run([Field(width: 2)] int extra, [Field(\"page\", 2, 3)] int page)\n    {\n        return extra + page + this.count;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Field;\n\n#[Field(\"Report\")]\nclass Report\n{\n    #[Field(label: 3)] private int $count = 0;\n\n    #[Field(\"run\", width: \"wide\")]\n    public function run(#[Field(width: 2)] int $extra, #[Field(\"page\", 2, 3)] int $page): int\n    {\n        return $extra + $page + $this->count;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Field.php", field)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Field.php", field)]);

    assert_eq!(
        sharp_issues,
        ["8:19 invalid-argument", "10:26 invalid-argument", "11:26 too-few-arguments", "11:67 too-many-arguments"]
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

const TOTALS: &str = "<?php\n\nnamespace Lib;\n\nfinal class Totals\n{\n    /** @param list<int> $values */\n    public static function sum(array $values): int\n    {\n        return array_sum($values);\n    }\n\n    /** @return list<int> */\n    public static function sizes(): array\n    {\n        return [1, 2];\n    }\n}\n";

#[test]
fn lists_and_maps_type_their_literals_indexes_and_loops_as_php_arrays() {
    let sharp = "namespace Demo;\n\nimport Lib.Totals;\n\nclass Order\n{\n    public List<Line> lines { get; private set; } = [];\n    private Map<string, int> counts = [:];\n\n    public int total(List<Line> extra)\n    {\n        List<Line> all = [new Line(1), extra[0]];\n        this.lines = all;\n        this.counts[\"lines\"] = count(this.lines);\n        const named = [\"a\": 1, \"b\": 2];\n        let sum = (named[\"a\"] ?? 0) + Totals.sum(Totals.sizes()) + all[1].cents;\n        for (const line of all) {\n            sum += line.cents;\n        }\n        for (const [name, size] of this.counts) {\n            if (name == \"lines\") {\n                sum += size;\n            }\n        }\n        for (const line of this.lines) {\n            sum += line.cents;\n        }\n        if (in_array(2, Totals.sizes(), true)) {\n            sum += 1;\n        }\n        return sum + (this.counts[\"lines\"] ?? 0);\n    }\n}\n\nclass Line\n{\n    public Line(public int cents { get; })\n    {\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Totals.php", TOTALS)]), Vec::<String>::new());
}

#[test]
fn a_wrong_element_type_is_reported_where_it_enters_the_collection() {
    let sharp = "namespace Demo;\n\nimport Lib.Totals;\n\nclass Order\n{\n    public List<Line> lines { get; private set; } = [];\n    private Map<string, int> counts = [:];\n\n    public List<int> wrong(List<Line> extra)\n    {\n        List<Line> all = [1];\n        Map<string, int> sizes = [\"a\": \"one\"];\n        this.counts[\"lines\"] = \"many\";\n        this.lines = [2];\n        Totals.sum(extra);\n        return extra;\n    }\n}\n\nclass Line\n{\n}\n";

    assert_eq!(
        issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Totals.php", TOTALS)]),
        [
            "12:26 invalid-local-assignment-value",
            "13:34 invalid-local-assignment-value",
            "14:9 invalid-property-assignment-value",
            "15:22 invalid-property-assignment-value",
            "16:20 invalid-argument",
            "17:16 invalid-return-statement",
        ]
    );
}

/// Spec section 12 writes a change to a collection in a property back through the property's `set`, so code that
/// cannot reach the `set` cannot change the collection, as PHP refuses the same write when it runs.
#[test]
fn an_index_write_to_a_property_needs_its_set() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public Map<int, int> lines { get; private set; } = [:];\n    public Map<string, int> codes { get; }\n\n    public Order()\n    {\n        this.codes = [:];\n    }\n\n    public void change(Order other)\n    {\n        this.lines[0] = 1;\n        other.lines[0] = 1;\n        this.codes[\"a\"] = 1;\n    }\n}\n\nclass Shop\n{\n    public void change(Order order)\n    {\n        order.lines[0] = 1;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public private(set) array $lines = [];\n    public readonly array $codes;\n\n    public function __construct()\n    {\n        $this->codes = [];\n    }\n\n    public function change(Order $other): void\n    {\n        $this->lines[0] = 1;\n        $other->lines[0] = 1;\n        $this->codes['a'] = 1;\n    }\n}\n\nclass Shop\n{\n    public function change(Order $order): void\n    {\n        $order->lines[0] = 1;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Order.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Order.php", php), &[]);

    assert_eq!(sharp_issues, ["17:14 invalid-property-write", "25:15 invalid-property-write"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// A bare `x[i]` throws when the key is missing, which fits a `List`, whose keys run without gaps. A `Map` key is
/// often missing, so a `Map` is read with `??`, as spec section 12 decides. A compound assignment reads first.
#[test]
fn a_bare_index_read_on_a_map_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Prices\n{\n    private Map<string, int> prices = [\"pro\": 5];\n\n    public int read(Map<string, int> plans, List<int> sizes, string plan)\n    {\n        let total = sizes[0] + (plans[plan] ?? 0) + (this.prices[plan] ?? 0);\n        total += plans[plan];\n        this.prices[plan] += 1;\n        const named = [\"a\": 1];\n        return total + named[\"a\"];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Prices.sharp", sharp), &[]),
        [
            "10:18 possibly-undefined-array-index",
            "11:9 possibly-undefined-array-index",
            "13:24 possibly-undefined-array-index",
        ]
    );
}

/// `?.` reads a missing key as null, as `??` does, because `x[k]?.name` runs as `($x[$k] ?? null)?->name`.
#[test]
fn a_null_safe_access_on_an_index_reads_a_missing_key_as_null() {
    let sharp = "namespace Demo;\n\nclass Shelf\n{\n    public string? first(Map<string, Item> items, string key)\n    {\n        return items[key]?.name ?? items[key]?.label();\n    }\n}\n\nclass Item\n{\n    public Item(public string name { get; })\n    {\n    }\n\n    public string? label()\n    {\n        return null;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Shelf.sharp", sharp), &[]), Vec::<String>::new());
}

/// Writing `x[i] = v` to a `List` could leave a gap in its keys, so spec section 12 writes a `List` with `set` and
/// `add`. Every step of the target that is a `List` is reported.
#[test]
fn an_index_write_to_a_list_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Sizes\n{\n    public void change(List<int> sizes, Map<string, List<int>> groups, List<Map<string, int>> rows)\n    {\n        sizes[0] = 1;\n        sizes[0] += 1;\n        groups[\"a\"][0] = 1;\n        rows[0][\"a\"] = 1;\n        groups[\"b\"] = [1];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Sizes.sharp", sharp), &[]),
        [
            "7:9 invalid-array-access",
            "8:9 invalid-array-access",
            "9:9 invalid-array-access",
            "10:9 invalid-array-access"
        ]
    );
}

/// `for (const [k, v] of x)` reads the keys of a `Map`. A `List`'s indexes come from `entries()`, as spec section 12
/// writes. PHP stores an all-digit string key as an `int`, so the key of a `Map<string, V>` reads back as
/// `int|string`.
#[test]
fn a_key_and_value_loop_reads_a_map_and_its_keys_as_php_stores_them() {
    let sharp = "namespace Demo;\n\nclass Loops\n{\n    public int run(List<int> sizes, Map<string, int> counts)\n    {\n        let total = 0;\n        for (const [index, size] of sizes) {\n            total += index + size;\n        }\n        for (const [name, count] of counts) {\n            total += strlen(name) + count;\n        }\n        return total;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Loops.sharp", sharp), &[]),
        ["8:37 invalid-iterator", "12:29 possibly-invalid-argument"]
    );
}

/// A `List` has `add`, `set`, `get` and `entries()`, and a `Map` has `delete` and `get`, each typed by the elements of
/// the collection it is called on, as spec section 12 decides.
#[test]
fn collection_methods_take_and_give_the_types_of_their_elements() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public List<Line> lines { get; private set; } = [];\n    private Map<string, int> counts = [:];\n\n    public int total(Line line, List<Line> extra)\n    {\n        this.lines.add(line);\n        extra.add(line);\n        extra.set(0, line);\n        this.counts.delete(\"a\");\n        let sum = (this.counts.get(\"b\") ?? 0) + (extra.get(0)?.cents ?? 0);\n        for (const [index, item] of extra.entries()) {\n            sum += index + item.cents;\n        }\n        return sum;\n    }\n\n    public void wrong(List<Line> extra, Map<string, int> counts)\n    {\n        extra.add(1);\n        extra.set(\"a\", new Line(1));\n        counts.add(1);\n        extra.delete(0);\n    }\n}\n\nclass Line\n{\n    public Line(public int cents { get; })\n    {\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Order.sharp", sharp), &[]),
        ["23:19 invalid-argument", "24:19 invalid-argument", "25:16 non-existent-method", "26:15 non-existent-method"]
    );
}

/// The analyzer checks a collection method against `Sharp\ListMethods` or `Sharp\MapMethods`, and every message names
/// the PHP# type the code wrote instead, as in "`add` doesn't exist on `Map<string, int>`".
#[test]
fn a_collection_method_message_names_the_sharp_type() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public void wrong(List<Line> lines, List<int> sizes, Map<string, int> counts)\n    {\n        lines.add(1);\n        sizes.set(\"a\", 1);\n        counts.add(1);\n        sizes.delete(0);\n        counts.get(1.5);\n        sizes.get();\n        sizes.get(0, 1);\n    }\n}\n\nclass Line\n{\n}\n";
    let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Order.sharp", sharp), &[]);

    let messages: Vec<&str> = issues.iter().map(|issue| issue.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "Invalid argument type for argument #1 of `List<Demo\\Line>.add`: expected `Demo\\Line`, but found `int(1)`.",
            "Invalid argument type for argument #1 of `List<int>.set`: expected `int`, but found `string('a')`.",
            "Method `add` does not exist on `Map<string, int>`.",
            "Method `delete` does not exist on `List<int>`.",
            "Invalid argument type for argument #1 of `Map<string, int>.get`: expected `string`, but found `float(1.5)`.",
            "Too few arguments provided for method `List<int>.get`.",
            "Too many arguments provided for method `List<int>.get`.",
        ]
    );
    for issue in &issues {
        let text = format!("{issue:?}");
        assert!(!text.contains("ListMethods") && !text.contains("MapMethods"), "{text}");
    }
}

/// A PHP# collection holds any value of its element type, as a `List<int>` holds any int, so a method takes one
/// even where the analyzer knows the elements are literals, as TypeScript's `let a = [5]` is a `number[]`.
#[test]
fn collection_methods_take_any_value_of_a_type_the_elements_are_literals_of() {
    let sharp = "namespace Demo;\n\nclass Tray\n{\n    public List<int> items { get; private set; } = [];\n\n    public void fill()\n    {\n        let sizes = [5];\n        sizes.add(6);\n        this.items = [5];\n        this.items.add(6);\n        this.items.set(0, 7);\n        let names = [\"tea\": true];\n        names.delete(\"pie\");\n        let found = names.get(\"pie\") ?? false;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tray.sharp", sharp), &[]), Vec::<String>::new());
}

/// A method that changes a collection writes it back where it lives, so spec section 12 allows it only on a place
/// the caller can write: a local, a parameter, or a property whose `set` the caller reaches. The runtime never meets
/// a write the checker accepts and it refuses.
#[test]
fn a_changing_collection_method_needs_a_place_the_caller_can_write() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public List<int> lines { get; private set; } = [];\n    public List<int> fixed { get; } = [];\n    public List<int> computed => [1];\n}\n\nclass Shop\n{\n    public int change(Order order, Map<string, List<int>> groups)\n    {\n        order.lines.add(1);\n        order.fixed.add(1);\n        order.computed.add(1);\n        this.make().add(1);\n        return count(order.lines.entries()) + (order.fixed.get(0) ?? 0) + count(groups);\n    }\n\n    public List<int> make()\n    {\n        return [];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Shop.sharp", sharp), &[]),
        [
            "14:15 invalid-property-write",
            "15:15 invalid-property-write",
            "16:15 invalid-property-write",
            "17:9 invalid-pass-by-reference"
        ]
    );
}

/// A property with hooks changes through `get` and then `set`, as Swift and decision 014 do, so a changing method
/// may run on it where its `set` is reachable.
#[test]
fn a_changing_collection_method_may_run_on_a_property_with_a_set_hook() {
    let sharp = "namespace Demo;\n\nimport Lib.Box;\n\nclass Shop\n{\n    public void fill(Box box)\n    {\n        box.items.add(1);\n    }\n}\n";
    let box_class = "<?php\n\nnamespace Lib;\n\nfinal class Box\n{\n    /** @var list<int> */\n    public array $items = [] {\n        get => $this->items;\n        set(array $value) {\n            $this->items = $value;\n        }\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Shop.sharp", sharp), &[("src/Lib/Box.php", box_class)]), Vec::<String>::new());
}

/// `+=`, `++` and `--` read an index, then write it. On a `Map` the read is bare, and on a `List` the write is, so
/// both are refused, as Kotlin refuses `map[k] += 1`, and the help writes the form that compiles.
#[test]
fn a_compound_assignment_or_increment_on_an_index_names_the_form_that_compiles() {
    let sharp = "namespace Demo;\n\nclass Counts\n{\n    public void bump(Map<string, int> counts, List<int> sizes)\n    {\n        counts[\"a\"] += 1;\n        counts[\"b\"]++;\n        sizes[0] += 1;\n        sizes[1]--;\n    }\n}\n";

    let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counts.sharp", sharp), &[]);
    let helps: Vec<_> = issues.iter().map(|issue| issue.help.as_deref().unwrap_or("")).collect();

    assert_eq!(issues.len(), 4, "{issues:?}");
    assert!(helps[..2].iter().all(|help| help.contains("m[k] = (m[k] ?? 0) + 1")), "{helps:?}");
    assert!(helps[2..].iter().all(|help| help.contains("list.set(i, list[i] + 1)")), "{helps:?}");
}
