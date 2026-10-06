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

#[test]
fn a_method_used_as_a_value_is_not_supported_yet() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public int total()\n    {\n        const calc = Calc.make();\n        const add = calc.add;\n        return this.total;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);

    assert!(sharp_issues.contains(&"10:26 not-supported-yet".to_string()), "{sharp_issues:?}");
    assert!(sharp_issues.contains(&"11:21 not-supported-yet".to_string()), "{sharp_issues:?}");
    assert!(!sharp_issues.iter().any(|issue| issue.ends_with("non-existent-property")), "{sharp_issues:?}");

    let helps: Vec<_> =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)])
            .into_iter()
            .filter(|issue| issue.code.as_deref() == Some("not-supported-yet"))
            .filter_map(|issue| issue.help)
            .collect();
    assert_eq!(helps, ["Call the method: `calc.add()`.", "Call the method: `this.total()`."]);
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
fn a_method_used_as_a_value_through_null_safe_access_is_not_supported_yet() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static void total(Calc? calc)\n    {\n        const add = calc?.add;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);

    assert!(sharp_issues.contains(&"9:27 not-supported-yet".to_string()), "{sharp_issues:?}");
    assert!(!sharp_issues.iter().any(|issue| issue.ends_with("non-existent-property")), "{sharp_issues:?}");
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

const PRICES: &str = "<?php\n\nnamespace Lib;\n\nfinal class Prices\n{\n    /** @return array<string, int> */\n    public static function named(): array\n    {\n        return ['a' => 1];\n    }\n\n    /** @return iterable<int, int> */\n    public static function stream(): iterable\n    {\n        yield 1;\n    }\n\n    /** @return list<int> */\n    public static function listed(): array\n    {\n        return [1, 2];\n    }\n}\n";

#[test]
fn a_spread_of_a_value_that_is_not_a_list_is_an_invalid_argument() {
    let sharp = "namespace Demo;\n\nimport Lib.Prices;\n\nclass Report\n{\n    public Report(int first, int second)\n    {\n    }\n\n    public int size() => 1;\n\n    public static int sum(int ...values) => count(values);\n\n    public static int spread()\n    {\n        const named = Report.sum(...Prices.named());\n        const stream = Report.sum(...Prices.stream());\n        const made = new Report(...Prices.named());\n        return named + stream + made.size() + Report.sum(...Prices.listed());\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Prices;\n\nclass Report\n{\n    public function __construct(int $first, int $second)\n    {\n    }\n\n    public function size(): int { return 1; }\n\n    public static function sum(int ...$values): int { return count($values); }\n\n    public static function spread(): int\n    {\n        $named = Report::sum(...Prices::named());\n        $stream = Report::sum(...Prices::stream());\n        $made = new Report(...Prices::named());\n        return $named + $stream + $made->size() + Report::sum(...Prices::listed());\n    }\n}\n";

    // An `array<string, int>` may be empty, so the constructor may get too few arguments, in PHP too.
    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Prices.php", PRICES)]),
        ["17:37 invalid-argument", "18:38 invalid-argument", "19:36 invalid-argument", "19:32 too-few-arguments"]
    );
    assert_eq!(issues(("src/Demo/Report.php", php), &[("src/Lib/Prices.php", PRICES)]), ["21:27 too-few-arguments"]);
}

#[test]
fn a_spread_of_a_value_that_is_not_iterable_is_one_invalid_argument() {
    let into_defaults = "namespace Demo;\n\nclass Report\n{\n    public static int part(int first = 1, int second = 2) => first + second;\n\n    public static int run(int number) => Report.part(...number);\n}\n";
    let into_variadic = "namespace Demo;\n\nclass Report\n{\n    public static int sum(int ...values) => count(values);\n\n    public static int run(int number) => Report.sum(...number);\n}\n";

    for sharp in [into_defaults, into_variadic] {
        let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]);
        let messages: Vec<&str> = issues.iter().map(|issue| issue.message.as_str()).collect();

        assert_eq!(messages, ["Cannot spread a value of type `int`: PHP# spreads only a list."], "{sharp}");
    }
}

#[test]
fn a_named_argument_never_fills_a_variadic_parameter_of_a_method_or_a_php_function() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int sum(int ...values) => max(0, ...values);\n\n    public static int run()\n    {\n        const named = Report.sum(first: 1, second: 2);\n        const own = Report.sum(values: 1);\n        const after = Report.sum(1, values: 2);\n        return named + own + after + strlen(sprintf(\"%d\", 1, other: 2));\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function sum(int ...$values): int { return max(0, ...$values); }\n\n    public static function run(): int\n    {\n        $named = Report::sum(first: 1, second: 2);\n        $own = Report::sum(values: 1);\n        $after = Report::sum(1, values: 2);\n        return $named + $own + $after + strlen(sprintf(\"%d\", 1, other: 2));\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "9:34 invalid-named-argument",
            "10:32 invalid-named-argument",
            "11:37 invalid-named-argument",
            "12:62 invalid-named-argument",
        ]
    );
    let annotations: Vec<String> = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[])
        .iter()
        .filter_map(|issue| issue.primary_annotation()?.message.clone())
        .collect();
    assert_eq!(annotations, ["A variadic parameter takes no named argument in PHP#"; 4]);
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["13:33 named-argument-after-positional"]);
}

#[test]
fn a_default_that_does_not_fit_its_type_is_an_error_unless_the_type_is_nullable() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    protected int key = null;\n    protected int|string other = false;\n    protected int? kept = null;\n\n    public Report(protected int|string id = null)\n    {\n    }\n\n    public static int size(int|string id = null, int x = null, int? y = null) => 1;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    protected int $key = null;\n    protected int|string $other = false;\n    protected ?int $kept = null;\n\n    public function __construct(protected int|string $id = null)\n    {\n    }\n\n    public static function size(int|string $id = null, int $x = null, ?int $y = null): int { return 1; }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "5:25 invalid-property-default-value",
            "6:34 invalid-property-default-value",
            "9:45 invalid-parameter-default-value",
            "13:44 invalid-parameter-default-value",
            "13:58 invalid-parameter-default-value",
        ]
    );
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
}

#[test]
fn a_variadic_parameter_is_a_list_that_spreads_into_methods_and_php_functions_with_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int sum(int ...values)\n    {\n        const most = max(0, ...values);\n        return Report.total(...values) + new Report(...values).size() + most;\n    }\n\n    public Report(int ...values)\n    {\n    }\n\n    public int size() => 1;\n\n    public static int total(int ...values) => count(values);\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_union_type_is_checked_as_php_checks_its_twin() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int|string key = 1;\n\n    public int|string find(int|Calc id)\n    {\n        int|string found = this.key;\n        if (found === 1) {\n            return 1.5;\n        }\n        return id;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    private int|string $key = 1;\n\n    public function find(int|Calc $id): int|string\n    {\n        $found = $this->key;\n        if ($found === 1) {\n            return 1.5;\n        }\n        return $id;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(sharp_issues, ["13:20 invalid-return-statement", "15:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_nullable_union_holds_its_types_and_null_as_its_php_twin_does() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private (int|string)? key = null;\n\n    public (int|string)? find((int|string)? id)\n    {\n        (int|string)? found = id ?? this.key;\n        return found;\n    }\n\n    public int run()\n    {\n        this.find(null);\n        this.find(\"one\");\n        this.find(1.5);\n        return 1;\n    }\n}\n";
    let php = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Demo;\n\nclass Report\n{\n    private int|string|null $key = null;\n\n    public function find(int|string|null $id): int|string|null\n    {\n        $found = $id ?? $this->key;\n        return $found;\n    }\n\n    public function run(): int\n    {\n        $this->find(null);\n        $this->find(\"one\");\n        $this->find(1.5);\n        return 1;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["17:19 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_nullable_field_without_an_initial_value_starts_as_null_as_its_php_twin_does() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int? total;\n    private (int|string)? key;\n    public Report? next { get; set; }\n\n    public int? current()\n    {\n        return this.total;\n    }\n\n    public int sum()\n    {\n        return (this.total ?? 0) + (this.next?.sum() ?? 0);\n    }\n\n    public (int|string)? find() => this.key;\n}\n";
    let php = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Demo;\n\nclass Report\n{\n    private ?int $total = null;\n    private int|string|null $key = null;\n    public ?Report $next = null;\n\n    public function current(): ?int\n    {\n        return $this->total;\n    }\n\n    public function sum(): int\n    {\n        return ($this->total ?? 0) + ($this->next?->sum() ?? 0);\n    }\n\n    public function find(): int|string|null\n    {\n        return $this->key;\n    }\n}\n";
    let php_without_default = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    private ?int $total;\n\n    public function current(): ?int\n    {\n        return $this->total;\n    }\n}\n";
    let settings = || Settings { check_property_initialization: true, ..settings() };

    let sharp_issues = issues_with(settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(settings(), ("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
    assert_eq!(issues_with(settings(), ("src/Demo/Report.php", php_without_default), &[]), ["5:7 missing-constructor"]);
}

#[test]
fn a_nullable_field_without_an_initial_value_reads_as_the_same_field_written_with_null() {
    let implicit = "namespace Demo;\n\nclass Report\n{\n    private int? total;\n\n    public int current()\n    {\n        return this.total;\n    }\n}\n";
    let explicit = "namespace Demo;\n\nclass Report\n{\n    private int? total = null;\n\n    public int current()\n    {\n        return this.total;\n    }\n}\n";
    let messages = |code| -> Vec<String> {
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", code), &[])
            .into_iter()
            .map(|issue| issue.message)
            .collect()
    };

    assert_eq!(messages(implicit), messages(explicit));
    assert_eq!(
        messages(implicit),
        [
            "Function `Demo\\Report::current` is declared to return `int` but possibly returns a nullable value (inferred as `int|null`).",
            "Invalid return type for function `Demo\\Report::current`: expected `int`, but found `int|null`.",
        ]
    );
}
