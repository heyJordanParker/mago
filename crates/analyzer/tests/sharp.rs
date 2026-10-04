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
use mago_span::HasSpan;
use mago_span::Span;
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
        assert!(!program.has_errors(), "{name} did not parse: {:?}", program.errors);

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

    let code = analyzed.1;
    result
        .issues
        .iter()
        .map(|issue| {
            let offset = issue.primary_span().expect("a primary span").start.offset as usize;
            let line = code[..offset].matches('\n').count() + 1;
            let column = offset - code[..offset].rfind('\n').map_or(0, |newline| newline + 1) + 1;

            format!("{line}:{column} {}", issue.code.as_deref().unwrap_or("none"))
        })
        .collect()
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

    assert!(
        result.issues.iter().any(|issue| issue.code.as_deref() == Some("not-supported-yet")),
        "{:#?}",
        result.issues
    );
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
fn adding_a_string_is_not_supported_yet() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        let label = \"one\";\n        return extra + label;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);

    assert!(sharp_issues.contains(&"8:16 not-supported-yet".to_string()), "{sharp_issues:?}");
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
