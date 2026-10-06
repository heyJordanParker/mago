//! Each test lowers a PHP# snippet and compares the node tree with the `zend_ast` that php-src's
//! `zend_language_parser.y` builds for the equivalent PHP, quoted above each test.
//!
//! A tree prints one node per line: its kind without `SHARP_AST_`, its attr in brackets when it is not 0, a ZVAL's
//! value, and a declaration's name and lines. `null` is a null child.

#![allow(clippy::panic, clippy::expect_used, clippy::use_debug)]

use std::ffi::c_char;
use std::fmt::Write;
use std::slice;

use indoc::indoc;

use mago_sharp_bridge::sharp_kind;
use mago_sharp_bridge::sharp_lower;
use mago_sharp_bridge::sharp_node;
use mago_sharp_bridge::sharp_severity;
use mago_sharp_bridge::sharp_str;
use mago_sharp_bridge::sharp_unit;
use mago_sharp_bridge::sharp_unit_free;
use mago_sharp_bridge::sharp_value;

/// A unit `sharp_lower` returned, freed on drop.
struct Lowered(*mut sharp_unit);

impl Lowered {
    fn new(code: &str) -> Self {
        Self::named("src/Report.sharp", code)
    }

    fn named(path: &str, code: &str) -> Self {
        // SAFETY: both pointers point to as many bytes as their lengths say.
        Self(unsafe {
            sharp_lower(path.as_ptr().cast::<c_char>(), path.len(), code.as_ptr().cast::<c_char>(), code.len())
        })
    }

    fn unit(&self) -> &sharp_unit {
        // SAFETY: `sharp_lower` returns a valid unit, freed only on drop.
        unsafe { &*self.0 }
    }

    fn nodes(&self) -> &[sharp_node] {
        // SAFETY: the unit owns `node_count` nodes.
        unsafe { array(self.unit().nodes, self.unit().node_count) }
    }

    fn children(&self) -> &[u32] {
        // SAFETY: the unit owns `children_count` children.
        unsafe { array(self.unit().children, self.unit().children_count) }
    }

    /// Every diagnostic, as `line:column severity message`.
    fn diagnostics(&self) -> Vec<String> {
        // SAFETY: the unit owns `diagnostic_count` diagnostics.
        let diagnostics = unsafe { array(self.unit().diagnostics, self.unit().diagnostic_count) };

        diagnostics
            .iter()
            .map(|diagnostic| {
                let severity = match diagnostic.severity {
                    sharp_severity::SHARP_PARSE_ERROR => "parse error",
                    sharp_severity::SHARP_COMPILE_ERROR => "compile error",
                };

                format!("{}:{} {severity}: {}", diagnostic.line, diagnostic.column, text(diagnostic.message))
            })
            .collect()
    }

    fn tree(&self) -> String {
        assert_eq!(self.diagnostics(), Vec::<String>::new(), "the source lowers");

        self.render(self.unit().root)
    }

    /// The statements of the method `run`.
    fn body(&self) -> String {
        let method = self
            .nodes()
            .iter()
            .position(|node| node.kind == sharp_kind::SHARP_AST_METHOD && text(node.text) == "run")
            .expect("the source declares `run`");

        self.render(self.child(method as u32, 2))
    }

    fn child(&self, node: u32, index: u32) -> u32 {
        self.children()[(self.nodes()[node as usize].first_child + index) as usize]
    }

    fn render(&self, node: u32) -> String {
        let mut tree = String::new();
        self.render_into(node, 0, &mut tree);

        tree
    }

    fn render_into(&self, index: u32, depth: usize, tree: &mut String) {
        tree.push_str(&"  ".repeat(depth));
        if index == u32::MAX {
            tree.push_str("null\n");

            return;
        }

        let node = &self.nodes()[index as usize];
        tree.push_str(format!("{:?}", node.kind).trim_start_matches("SHARP_AST_"));
        if node.attr != 0 {
            let _ = write!(tree, " [{}]", node.attr);
        }

        match node.kind {
            sharp_kind::SHARP_AST_ZVAL => {
                let _ = match node.value {
                    sharp_value::SHARP_NULL => write!(tree, " null"),
                    sharp_value::SHARP_FALSE => write!(tree, " false"),
                    sharp_value::SHARP_TRUE => write!(tree, " true"),
                    sharp_value::SHARP_LONG => write!(tree, " {}", node.long_value),
                    sharp_value::SHARP_DOUBLE => write!(tree, " {:?}", node.double_value),
                    sharp_value::SHARP_STRING => write!(tree, " {:?}", text(node.text)),
                };
            }
            sharp_kind::SHARP_AST_CLASS | sharp_kind::SHARP_AST_METHOD | sharp_kind::SHARP_AST_PROPERTY_HOOK => {
                let _ = write!(tree, " {:?} @{}-{}", text(node.text), node.line, node.end_line);
            }
            _ => {}
        }

        tree.push('\n');
        for child in 0..node.child_count {
            self.render_into(self.child(index, child), depth + 1, tree);
        }
    }
}

impl Drop for Lowered {
    fn drop(&mut self) {
        // SAFETY: `sharp_lower` returned the unit, and only this drop frees it.
        unsafe { sharp_unit_free(self.0) };
    }
}

/// # Safety
///
/// When `len` is not 0, `pointer` points to `len` values.
unsafe fn array<'unit, T>(pointer: *const T, len: usize) -> &'unit [T] {
    if len == 0 {
        return &[];
    }

    // SAFETY: the caller passes `len` values at `pointer`.
    unsafe { slice::from_raw_parts(pointer, len) }
}

fn text(text: sharp_str) -> String {
    // SAFETY: the unit owns `len` bytes at `ptr`.
    String::from_utf8_lossy(unsafe { array(text.ptr.cast::<u8>(), text.len) }).into_owned()
}

/// A file declaring the method `run` with `body`. Its body starts on line 9.
fn method(body: &str) -> String {
    format!(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{{\n    public int run(int extra)\n    {{\n{body}    }}\n}}\n"
    )
}

fn body(statements: &str) -> String {
    Lowered::new(&method(statements)).body()
}

/// ```php
/// $base = 2; $base = 3;
/// ```
#[test]
fn a_const_local_reassigned_returns_one_compile_error_and_no_nodes() {
    let lowered = Lowered::new(&method("        const base = 2;\n        base = 3;\n        return base;\n"));

    assert_eq!(lowered.diagnostics(), ["10:9 compile error: Cannot assign to `base`: it is declared with `const`."]);
    assert_eq!(lowered.unit().node_count, 0);
    assert_eq!(lowered.unit().children_count, 0);
}

#[test]
fn php_syntax_returns_a_parse_error_and_no_nodes() {
    let lowered = Lowered::new(&method("        return this->run(1);\n"));

    assert_eq!(lowered.diagnostics(), ["9:20 parse error: `->` is PHP syntax: PHP# writes member access with `.`"]);
    assert_eq!(lowered.unit().node_count, 0);
}

#[test]
fn a_construct_outside_the_slice_returns_its_not_supported_error() {
    let lowered = Lowered::new(&method("        echo extra;\n        return 1;\n"));

    assert_eq!(lowered.diagnostics(), ["9:9 compile error: This statement is not supported yet in PHP#."]);
}

#[test]
fn a_method_without_a_body_returns_the_checker_error() {
    let lowered = Lowered::new("class Report\n{\n    public int run();\n}\n");

    assert_eq!(
        lowered.diagnostics(),
        ["3:21 compile error: Non-Abstract method `Report::run` must have a concrete body."]
    );
}

/// Each body nests one chain 100,000 levels deep: a sum, a call chain, a null-safe call chain, a `??` chain and a
/// `??=` chain. With the namespace, the class and the `return`, that is far past the 512 levels the parser allows.
/// The parser builds a sum or a call chain in a loop and finds it too deep at its innermost term, the first one
/// written. It builds `??` and `??=` by recursing to the right and stops at the term 510 levels into the chain.
#[test]
fn a_file_nested_100_000_levels_deep_returns_the_depth_error_and_no_nodes() {
    let bodies = [
        (format!("        return {};\n", vec!["extra"; 100_000].join(" + ")), 16),
        (format!("        return this{};\n", ".total()".repeat(100_000)), 16),
        (format!("        return this{};\n", "?.total()".repeat(100_000)), 16),
        (format!("        return {};\n", vec!["extra"; 100_000].join(" ?? ")), 16 + 509 * "extra ?? ".len()),
        (format!("        return {};\n", vec!["extra"; 100_000].join(" ??= ")), 16 + 509 * "extra ??= ".len()),
    ];

    for (body, column) in bodies {
        let lowered = Lowered::new(&method(&body));

        assert_eq!(
            lowered.diagnostics(),
            [format!("9:{column} parse error: PHP# nests statements, expressions and types at most 512 levels deep.")]
        );
        assert_eq!(lowered.unit().node_count, 0);
    }
}

/// A union nests its types to the right, and each type is a level, so a 100,000-member return type is too deep at
/// its member 509: the namespace, the class and 509 unions are around it.
#[test]
fn a_union_of_100_000_types_returns_the_depth_error_and_no_nodes() {
    let union = (0..100_000).map(|index| format!("A{index}")).collect::<Vec<_>>().join("|");
    let lowered = Lowered::new(&format!(
        "namespace App.Tenant;\n\nclass Report\n{{\n    public {union} run()\n    {{\n        return null;\n    }}\n}}\n"
    ));
    let column = "    public ".len() + union.find("A509|").expect("the union has member 509") + 1;

    assert_eq!(
        lowered.diagnostics(),
        [format!("5:{column} parse error: PHP# nests statements, expressions and types at most 512 levels deep.")]
    );
    assert_eq!(lowered.unit().node_count, 0);
}

/// A 509-term sum or `??` chain in a method nests its innermost term 512 levels deep, the most the checker allows, and
/// so do a null-safe call chain of 509 calls and a 510-term sum as a field's initial value. The bridge lowers each on a
/// thread whose stack is far smaller than the recursion needs, as a PHP thread or fiber may be.
#[test]
fn the_deepest_file_the_checker_accepts_lowers_on_a_small_stack() {
    let codes = [
        method(&format!("        return {};\n", vec!["extra"; 509].join(" + "))),
        method(&format!("        return {};\n", vec!["extra"; 509].join(" ?? "))),
        method(&format!("        this{};\n        return extra;\n", "?.total(extra)".repeat(508))),
        format!(
            "namespace App.Tenant;\n\nclass Report\n{{\n    private int total = {};\n}}\n",
            vec!["1"; 510].join(" + ")
        ),
    ];

    for code in codes {
        let node_count = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let lowered = Lowered::new(&code);
                assert_eq!(lowered.diagnostics(), Vec::<String>::new());

                lowered.unit().node_count
            })
            .expect("the thread starts")
            .join()
            .expect("the lowering returns");

        assert!(node_count > 509 * 2, "{node_count} nodes");
    }
}

/// `ext/sharp` decides the dialect from the file name, so the bridge parses and checks PHP# whatever name it gets.
#[test]
fn a_file_of_any_name_is_parsed_and_checked_as_php_sharp() {
    let lowered = Lowered::named("src/Upper.SHARP", "namespace App.Tenant;\n\nclass Report\n{\n}\n");
    assert_eq!(lowered.diagnostics(), Vec::<String>::new());

    let refused = Lowered::named("src/Upper.SHARP", &method("        echo extra;\n        return 1;\n"));
    assert_eq!(refused.diagnostics(), ["9:9 compile error: This statement is not supported yet in PHP#."]);
}

/// The Zend scanner ends a line at `\n`, `\r\n` and a lone `\r`, and stops on the line after the last line ending.
#[test]
fn the_root_end_line_is_the_last_line_the_zend_scanner_counts() {
    let end_lines: Vec<u32> = [
        "class Report {}",
        "class Report {}\n",
        "class Report {}\r\n",
        "class Report {}\r",
        "class Report\r\n{\r}\n\n",
    ]
    .iter()
    .map(|code| {
        let lowered = Lowered::new(code);
        assert_eq!(lowered.diagnostics(), Vec::<String>::new());

        lowered.nodes()[lowered.unit().root as usize].end_line
    })
    .collect();

    assert_eq!(end_lines, [1, 2, 2, 2, 5]);
}

/// Node lines, declaration lines and diagnostic lines count a lone `\r` as a line ending too.
#[test]
fn every_line_is_the_line_the_zend_scanner_counts() {
    let class = Lowered::new("class Report\r\n{\r}\n\n");
    let root = &class.nodes()[class.unit().root as usize];
    let declaration = &class.nodes()[class.child(class.unit().root, 1) as usize];
    assert_eq!((declaration.line, declaration.end_line, root.end_line), (1, 3, 5));

    let method = Lowered::new("class Report\r{\r    public int run()\r    {\r        return 1;\r    }\r}\r");
    let lines: Vec<(String, u32, u32)> = method
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD || node.kind == sharp_kind::SHARP_AST_RETURN)
        .map(|node| (format!("{:?}", node.kind), node.line, node.end_line))
        .collect();
    assert_eq!(lines, [("SHARP_AST_RETURN".to_owned(), 5, 0), ("SHARP_AST_METHOD".to_owned(), 3, 6)]);

    let refused = Lowered::new("class Report\r{\r    public void run() { echo 1; }\r}\r");
    assert_eq!(refused.diagnostics(), ["3:25 compile error: This statement is not supported yet in PHP#."]);
}

/// ```php
/// <?php
/// declare(strict_types=1);
/// namespace App\Tenant;
/// use Lib\Calc;
/// class Report
/// {
/// }
/// ```
///
/// The `use` is not lowered: every class name in the tree is fully qualified.
#[test]
fn a_file_declares_strict_types_then_its_namespace_and_classes() {
    let lowered = Lowered::new("namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n}\n");

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              NAMESPACE
                ZVAL "App\\Tenant"
                null
              CLASS "Report" @5-7
                null
                null
                STMT_LIST
                null
                null
        "#}
    );
}

/// ```php
/// <?php
/// declare(strict_types=1);
/// class Report {}
/// ```
#[test]
fn a_file_without_a_namespace_declares_its_classes_globally() {
    let lowered = Lowered::new("class Report {}\n");

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              CLASS "Report" @1-1
                null
                null
                STMT_LIST
                null
                null
        "#}
    );
}

/// ```php
/// public function run(int $extra): int
/// {
///     return $extra;
/// }
/// ```
///
/// `[1]` on the method is `ZEND_ACC_PUBLIC`, and `[1]` on `int` is `ZEND_NAME_NOT_FQ`.
#[test]
fn a_method_is_a_public_function_with_its_return_type_after_its_parameters() {
    let lowered = Lowered::new(&method("        return extra;\n"));
    let class = lowered.child(lowered.unit().root, 2);

    assert_eq!(
        lowered.render(class),
        indoc! {r#"
            CLASS "Report" @5-11
              null
              null
              STMT_LIST
                METHOD [1] "run" @7-10
                  PARAM_LIST
                    PARAM
                      ZVAL [1] "int"
                      ZVAL "extra"
                      null
                      null
                      null
                      null
                  null
                  STMT_LIST
                    RETURN
                      VAR
                        ZVAL "extra"
                  ZVAL [1] "int"
                  null
              null
              null
        "#}
    );
}

/// ```php
/// private int $count = 0;
/// protected \Lib\Calc $calc;
/// ```
///
/// `[4]` and `[2]` are `ZEND_ACC_PRIVATE` and `ZEND_ACC_PROTECTED`.
#[test]
fn a_field_is_a_property_group_of_one_property() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int count = 0;\n    protected Calc calc;\n}\n",
    );
    let class = lowered.child(lowered.unit().root, 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [4]
                ZVAL [1] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "count"
                    ZVAL 0
                    null
                    null
                null
              PROP_GROUP [2]
                ZVAL "Lib\\Calc"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "calc"
                    null
                    null
                    null
                null
        "#}
    );
}

/// ```php
/// public private(set) int $views = 0;
/// public string $name;
/// public readonly int $id;
/// protected protected(set) int $total;   // written `public int total { get; protected set; }`
/// private readonly int $hidden;
/// ```
///
/// An auto-property is a property with asymmetric visibility. `[4097]` is `ZEND_ACC_PUBLIC | ZEND_ACC_PRIVATE_SET`,
/// and `[2049]` is `ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED_SET`. A get-only property is `readonly`, so `[129]` is
/// `ZEND_ACC_PUBLIC | ZEND_ACC_READONLY` and `[132]` is `ZEND_ACC_PRIVATE | ZEND_ACC_READONLY`.
#[test]
fn an_auto_property_is_a_property_with_its_set_visibility() {
    let lowered = Lowered::new(
        "class Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; set; }\n    public int id { get; }\n    public int total { get; protected set; }\n    private int hidden { get; }\n}\n",
    );
    let groups: Vec<(u32, String)> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PROP_GROUP)
        .map(|(index, node)| (node.attr, lowered.render(lowered.child(index as u32, 1))))
        .collect();

    assert_eq!(
        groups,
        [
            (4097, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"views\"\n    ZVAL 0\n    null\n    null\n".to_owned()),
            (1, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"name\"\n    null\n    null\n    null\n".to_owned()),
            (129, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"id\"\n    null\n    null\n    null\n".to_owned()),
            (2049, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"total\"\n    null\n    null\n    null\n".to_owned()),
            (132, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"hidden\"\n    null\n    null\n    null\n".to_owned()),
        ]
    );
}

/// ```php
/// public function __construct(int $start)
/// {
///     $this->count = $start;
/// }
/// ```
///
/// The constructor has no return type, and its first line is its name's, where PHP writes `function`.
#[test]
fn the_constructor_is_a_public_function_named_construct() {
    let lowered = Lowered::new(
        "class Report\n{\n    private int count;\n\n    public\n    Report(int start)\n    {\n        this.count = start;\n    }\n}\n",
    );
    let constructor =
        lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a constructor");

    assert_eq!(
        lowered.render(constructor as u32),
        indoc! {r#"
            METHOD [1] "__construct" @6-9
              PARAM_LIST
                PARAM
                  ZVAL [1] "int"
                  ZVAL "start"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                ASSIGN
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "count"
                  VAR
                    ZVAL "start"
              null
              null
        "#}
    );
}

/// ```php
/// public function total(): int { return $this->count + 1; }
/// public function touch(): void { $this->save(); }
/// public function __construct(int $count) { $this->count = $count; }
/// ```
///
/// An expression body returns its expression, and runs it as a statement in a `void` method and the constructor.
#[test]
fn an_expression_body_is_the_body_that_returns_its_expression() {
    let lowered = Lowered::new(
        "class Report\n{\n    private int count = 0;\n\n    public int total() => this.count + 1;\n\n    public void touch() => this.save();\n\n    public Report(int count) => this.count = count;\n}\n",
    );
    let bodies: Vec<String> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_METHOD)
        .map(|(index, _)| lowered.render(lowered.child(index as u32, 2)))
        .collect();

    assert_eq!(
        bodies,
        [
            indoc! {r#"
                STMT_LIST
                  RETURN
                    BINARY_OP [1]
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "count"
                      ZVAL 1
            "#},
            indoc! {r#"
                STMT_LIST
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "save"
                    ARG_LIST
            "#},
            indoc! {r#"
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "count"
                    VAR
                      ZVAL "count"
            "#},
        ]
    );
}

/// ```php
/// public string $slug { get => strtolower($this->name); }
/// ```
///
/// A computed property is a property with one `get` hook whose body is the short body php-src's grammar builds for
/// `get => expr;`. The hook's lines are the arrow's.
#[test]
fn a_computed_property_is_a_property_with_a_short_get_hook() {
    let lowered = Lowered::new(
        "class Report\n{\n    private string name = \"\";\n\n    public string slug => strtolower(this.name);\n}\n",
    );
    let class = lowered.child(lowered.unit().root, 1);

    assert_eq!(
        lowered.render(lowered.child(lowered.child(class, 2), 1)),
        indoc! {r#"
            PROP_GROUP [1]
              ZVAL [1] "string"
              PROP_DECL
                PROP_ELEM
                  ZVAL "slug"
                  null
                  null
                  STMT_LIST
                    PROPERTY_HOOK "get" @5-5
                      null
                      null
                      PROPERTY_HOOK_SHORT_BODY
                        CALL
                          ZVAL "strtolower"
                          ARG_LIST
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "name"
                      null
                      null
              null
        "#}
    );
}

/// ```php
/// public function __construct(private int $count, public readonly int $id, protected string $name, int $extra) {}
/// ```
///
/// A parameter that declares a member carries the member's flags: `[4]` is `ZEND_ACC_PRIVATE`, `[129]` is
/// `ZEND_ACC_PUBLIC | ZEND_ACC_READONLY`, and `[2]` is `ZEND_ACC_PROTECTED`.
#[test]
fn a_constructor_parameter_with_an_access_modifier_is_a_promoted_parameter() {
    let lowered = Lowered::new(
        "class Report\n{\n    public Report(private int count, public int id { get; }, protected string name { get; set; }, int extra) {}\n}\n",
    );
    let parameters: Vec<(String, u32)> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PARAM)
        .map(|(index, node)| {
            assert_eq!(lowered.child(index as u32, 5), u32::MAX, "a promoted parameter has no hooks");

            (text(lowered.nodes()[lowered.child(index as u32, 1) as usize].text), node.attr)
        })
        .collect();

    assert_eq!(
        parameters,
        [("count".to_owned(), 4), ("id".to_owned(), 129), ("name".to_owned(), 2), ("extra".to_owned(), 0)]
    );
}

/// ```php
/// private \Lib\Calc $calc;
/// public int $total = 2;
/// public function __construct(int $start)
/// {
///     $this->calc = new \Lib\Calc(1);
///     $this->count = $start;
/// }
/// ```
///
/// A constant initial value is the property's default. Any other runs at the start of the constructor.
#[test]
fn a_non_constant_initial_value_runs_at_the_start_of_the_constructor() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private Calc calc = new Calc(1);\n    public int total { get; set; } = 1 + 1;\n\n    public Report(int start)\n    {\n        this.count = start;\n    }\n}\n",
    );
    let class = lowered.child(lowered.unit().root, 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [4]
                ZVAL "Lib\\Calc"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "calc"
                    null
                    null
                    null
                null
              PROP_GROUP [1]
                ZVAL [1] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "total"
                    BINARY_OP [1]
                      ZVAL 1
                      ZVAL 1
                    null
                    null
                null
              METHOD [1] "__construct" @10-13
                PARAM_LIST
                  PARAM
                    ZVAL [1] "int"
                    ZVAL "start"
                    null
                    null
                    null
                    null
                null
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "calc"
                    NEW
                      ZVAL "Lib\\Calc"
                      ARG_LIST
                        ZVAL 1
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "count"
                    VAR
                      ZVAL "start"
                null
                null
        "#}
    );
}

/// ```php
/// private \Lib\Calc $calc;
/// public function __construct()
/// {
///     $this->calc = new \Lib\Calc(1);
/// }
/// ```
///
/// A class with a non-constant initial value and no constructor gets a public one, which spans the class.
#[test]
fn a_class_with_a_non_constant_initial_value_and_no_constructor_gets_one() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private Calc calc = new Calc(1);\n}\n",
    );
    let class = lowered.child(lowered.unit().root, 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [4]
                ZVAL "Lib\\Calc"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "calc"
                    null
                    null
                    null
                null
              METHOD [1] "__construct" @5-8
                PARAM_LIST
                null
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "calc"
                    NEW
                      ZVAL "Lib\\Calc"
                      ARG_LIST
                        ZVAL 1
                null
                null
        "#}
    );
}

/// ```php
/// public readonly string $code;
/// public function __construct()
/// {
///     $this->code = 'none';
/// }
/// ```
///
/// PHP takes no default on a `readonly` property, so a get-only property's initial value runs in the constructor even
/// when it is constant.
#[test]
fn a_get_only_property_gets_its_initial_value_in_the_constructor() {
    let lowered = Lowered::new("class Report\n{\n    public string code { get; } = \"none\";\n}\n");
    let class = lowered.child(lowered.unit().root, 1);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [129]
                ZVAL [1] "string"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "code"
                    null
                    null
                    null
                null
              METHOD [1] "__construct" @1-4
                PARAM_LIST
                null
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "code"
                    ZVAL "none"
                null
                null
        "#}
    );
}

/// ```php
/// public static function make(): void {}
/// private function hide() {}
/// protected function share() {}
/// ```
///
/// The flags are `ZEND_ACC_PUBLIC | ZEND_ACC_STATIC`, `ZEND_ACC_PRIVATE` and `ZEND_ACC_PROTECTED`.
#[test]
fn method_modifiers_become_the_method_flags() {
    let lowered = Lowered::new(
        "class Report\n{\n    public static void make() {}\n    private void hide() {}\n    protected void share() {}\n}\n",
    );
    let methods: Vec<(String, u32)> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD)
        .map(|node| (text(node.text), node.attr))
        .collect();

    assert_eq!(methods, [("make".to_owned(), 17), ("hide".to_owned(), 4), ("share".to_owned(), 2)]);
}

/// ```php
/// public function make(\Lib\Calc $other, float $rate = 1.5, bool $loud = PHP_DEBUG): \Lib\Calc {}
/// ```
///
/// A class type is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn parameters_carry_their_type_name_and_default() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public Calc make(Calc other, float rate = 1.5, bool loud = PHP_DEBUG) { return other; }\n}\n",
    );
    let method = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a method");

    assert_eq!(
        lowered.render(lowered.child(method as u32, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL "Lib\\Calc"
                ZVAL "other"
                null
                null
                null
                null
              PARAM
                ZVAL [1] "float"
                ZVAL "rate"
                ZVAL 1.5
                null
                null
                null
              PARAM
                ZVAL [1] "bool"
                ZVAL "loud"
                CONST
                  ZVAL [1] "PHP_DEBUG"
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {r#"
            ZVAL "Lib\\Calc"
        "#}
    );
}

/// ```php
/// public function find(?int $id, ?\Lib\Calc $other = null): ?\Lib\Calc { return null; }
/// ```
///
/// `[256]` is `ZEND_TYPE_NULLABLE`, which php-src's grammar adds to the type's attr, and `[257]` adds it to
/// `ZEND_NAME_NOT_FQ`.
#[test]
fn a_nullable_type_is_its_type_with_the_nullable_flag() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public Calc? find(int? id, Calc? other = null) { return null; }\n}\n",
    );
    let method = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a method");

    assert_eq!(
        lowered.render(lowered.child(method as u32, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL [257] "int"
                ZVAL "id"
                null
                null
                null
                null
              PARAM
                ZVAL [256] "Lib\\Calc"
                ZVAL "other"
                ZVAL null
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {r#"
            ZVAL [256] "Lib\\Calc"
        "#}
    );
}

/// ```php
/// public function run(): void
/// {
///     return;
/// }
/// ```
#[test]
fn a_void_method_returns_nothing() {
    let lowered = Lowered::new("class Report\n{\n    public void run()\n    {\n        return;\n    }\n}\n");

    assert_eq!(
        lowered.body(),
        indoc! {"
            STMT_LIST
              RETURN
                null
        "}
    );
}

/// ```php
/// $total = $extra * 2;
/// $label = 'one';
/// $total = 3;
/// ```
///
/// `[3]` is `ZEND_MUL`.
#[test]
fn let_and_const_locals_assign_their_variables() {
    assert_eq!(
        body(
            "        let total = extra * 2;\n        const label = 'one';\n        total = 3;\n        return total;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "total"
                BINARY_OP [3]
                  VAR
                    ZVAL "extra"
                  ZVAL 2
              ASSIGN
                VAR
                  ZVAL "label"
                ZVAL "one"
              ASSIGN
                VAR
                  ZVAL "total"
                ZVAL 3
              RETURN
                VAR
                  ZVAL "total"
        "#}
    );
}

/// A written type only tells the checker the local's type, so a typed local lowers as `let` and `const` do.
///
/// ```php
/// $found = null;
/// $base = 2;
/// ```
#[test]
fn typed_locals_assign_their_variables_and_drop_the_type() {
    assert_eq!(
        body("        Calc? found = null;\n        const int base = 2;\n        return base;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "found"
                ZVAL null
              ASSIGN
                VAR
                  ZVAL "base"
                ZVAL 2
              RETURN
                VAR
                  ZVAL "base"
        "#}
    );
}

/// ```php
/// $this->count += 1;
/// ```
///
/// `[1]` is `ZEND_ADD`.
#[test]
fn this_count_plus_equals_one_is_a_compound_assignment_to_a_property_of_this() {
    assert_eq!(
        body("        this.count += 1;\n        return 1;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN_OP [1]
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "count"
                ZVAL 1
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// return \Lib\Calc::make(2);
/// ```
///
/// The class is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn calc_make_is_a_static_call_on_the_imported_class() {
    assert_eq!(
        body("        return Calc.make(2);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "make"
                  ARG_LIST
                    ZVAL 2
        "#}
    );
}

/// ```php
/// return new \Lib\Calc($extra, rate: 2);
/// ```
///
/// The class is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn new_creates_the_imported_class_by_its_full_name() {
    assert_eq!(
        body("        return new Calc(extra, rate: 2);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                NEW
                  ZVAL "Lib\\Calc"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
                    NAMED_ARG
                      ZVAL "rate"
                      ZVAL 2
        "#}
    );
}

/// ```php
/// return \App\Tenant\Report::make();
/// ```
#[test]
fn a_class_of_the_same_namespace_is_called_by_its_full_name() {
    assert_eq!(
        body("        return Report.make();\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "App\\Tenant\\Report"
                  ZVAL "make"
                  ARG_LIST
        "#}
    );
}

/// ```php
/// return \Sharp\Int::parse($extra) + \Sharp\Float::tryParse($extra);
/// ```
#[test]
fn int_and_float_are_the_classes_of_the_sharp_namespace() {
    assert_eq!(
        body("        return Int.parse(extra) + Float.tryParse(extra);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [1]
                  STATIC_CALL
                    ZVAL "Sharp\\Int"
                    ZVAL "parse"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
                  STATIC_CALL
                    ZVAL "Sharp\\Float"
                    ZVAL "tryParse"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
        "#}
    );
}

/// ```php
/// return $this->total($extra, rate: 2)->value;
/// ```
#[test]
fn member_calls_and_reads_on_values_are_instance_access() {
    assert_eq!(
        body("        return this.total(extra, rate: 2).value;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                PROP
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "total"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
                      NAMED_ARG
                        ZVAL "rate"
                        ZVAL 2
                  ZVAL "value"
        "#}
    );
}

/// ```php
/// return \Lib\Calc::make()->add($extra);
/// ```
#[test]
fn a_call_on_a_static_call_result_is_an_instance_call() {
    assert_eq!(
        body("        return Calc.make().add(extra);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                METHOD_CALL
                  STATIC_CALL
                    ZVAL "Lib\\Calc"
                    ZVAL "make"
                    ARG_LIST
                  ZVAL "add"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
        "#}
    );
}

/// ```php
/// $a = "line\n"; $b = 'raw\n'; $c = 1.5; $d = 0x10; $e = 9223372036854775808; $f = true; $g = false; $h = null;
/// ```
///
/// `9223372036854775808` overflows `int`, so PHP reads it as a float.
#[test]
fn literals_are_zvals_of_their_php_value() {
    assert_eq!(
        body(
            "        let a = \"line\\n\";\n        let b = 'raw\\n';\n        let c = 1.5;\n        let d = 0x10;\n        let e = 9223372036854775808;\n        let f = true;\n        let g = false;\n        let h = null;\n        return 1;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                ZVAL "line\n"
              ASSIGN
                VAR
                  ZVAL "b"
                ZVAL "raw\\n"
              ASSIGN
                VAR
                  ZVAL "c"
                ZVAL 1.5
              ASSIGN
                VAR
                  ZVAL "d"
                ZVAL 16
              ASSIGN
                VAR
                  ZVAL "e"
                ZVAL 9.223372036854776e18
              ASSIGN
                VAR
                  ZVAL "f"
                ZVAL true
              ASSIGN
                VAR
                  ZVAL "g"
                ZVAL false
              ASSIGN
                VAR
                  ZVAL "h"
                ZVAL null
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// $a + $a - $a * $a / $a % $a;
/// ```
///
/// `[1]` to `[5]` are `ZEND_ADD`, `ZEND_SUB`, `ZEND_MUL`, `ZEND_DIV` and `ZEND_MOD`.
#[test]
fn arithmetic_operators_are_binary_ops() {
    assert_eq!(
        body("        let a = 1;\n        a + a - a * a / a % a;\n        return a;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                ZVAL 1
              BINARY_OP [2]
                BINARY_OP [1]
                  VAR
                    ZVAL "a"
                  VAR
                    ZVAL "a"
                BINARY_OP [5]
                  BINARY_OP [4]
                    BINARY_OP [3]
                      VAR
                        ZVAL "a"
                      VAR
                        ZVAL "a"
                    VAR
                      ZVAL "a"
                  VAR
                    ZVAL "a"
              RETURN
                VAR
                  ZVAL "a"
        "#}
    );
}

/// ```php
/// $a == $a; $a != $a; $a === $a; $a !== $a; $a < $a; $a <= $a; $a > $a; $a >= $a;
/// ```
///
/// `[18]`, `[19]`, `[16]`, `[17]`, `[20]` and `[21]` are `ZEND_IS_EQUAL`, `ZEND_IS_NOT_EQUAL`, `ZEND_IS_IDENTICAL`,
/// `ZEND_IS_NOT_IDENTICAL`, `ZEND_IS_SMALLER` and `ZEND_IS_SMALLER_OR_EQUAL`. `>` and `>=` have kinds of their own.
#[test]
fn comparison_operators_are_the_kinds_php_gives_them() {
    let tree = body(
        "        let a = 1;\n        a == a;\n        a != a;\n        a === a;\n        a !== a;\n        a < a;\n        a <= a;\n        a > a;\n        a >= a;\n        return a;\n",
    );
    let operators: Vec<&str> = tree.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")).collect();

    assert_eq!(
        operators,
        [
            "  ASSIGN",
            "  BINARY_OP [18]",
            "  BINARY_OP [19]",
            "  BINARY_OP [16]",
            "  BINARY_OP [17]",
            "  BINARY_OP [20]",
            "  BINARY_OP [21]",
            "  GREATER",
            "  GREATER_EQUAL",
            "  RETURN",
        ]
    );
}

/// ```php
/// $a && $a || !$a;
/// ```
///
/// `[14]` is `ZEND_BOOL_NOT`.
#[test]
fn logical_operators_are_and_or_and_bool_not() {
    assert_eq!(
        body("        let a = true;\n        a && a || !a;\n        return 1;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                ZVAL true
              OR
                AND
                  VAR
                    ZVAL "a"
                  VAR
                    ZVAL "a"
                UNARY_OP [14]
                  VAR
                    ZVAL "a"
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// -$a; +$a; ++$a; --$a; $a++; $a--;
/// ```
#[test]
fn unary_operators_are_the_kinds_php_gives_them() {
    let tree = body(
        "        let a = 1;\n        -a;\n        +a;\n        ++a;\n        --a;\n        a++;\n        a--;\n        return a;\n",
    );
    let operators: Vec<&str> = tree.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")).collect();

    assert_eq!(
        operators,
        ["  ASSIGN", "  UNARY_MINUS", "  UNARY_PLUS", "  PRE_INC", "  PRE_DEC", "  POST_INC", "  POST_DEC", "  RETURN"]
    );
}

/// ```php
/// $a -= 1; $a *= 2; $a /= 3; $a **= 4;
/// ```
///
/// `[2]`, `[3]`, `[4]` and `[12]` are `ZEND_SUB`, `ZEND_MUL`, `ZEND_DIV` and `ZEND_POW`.
#[test]
fn compound_assignments_are_assign_ops() {
    let tree = body(
        "        let a = 1;\n        a -= 1;\n        a *= 2;\n        a /= 3;\n        a **= 4;\n        return a;\n",
    );
    let operators: Vec<&str> = tree.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")).collect();

    assert_eq!(
        operators,
        ["  ASSIGN", "  ASSIGN_OP [2]", "  ASSIGN_OP [3]", "  ASSIGN_OP [4]", "  ASSIGN_OP [12]", "  RETURN"]
    );
}

/// ```php
/// return -$a ** $a ** 2;
/// ```
///
/// `[12]` is `ZEND_POW`. `**` groups to the right and binds tighter than unary `-`, as php-src's grammar has it.
#[test]
fn exponentiation_is_a_right_grouped_pow_under_unary_minus() {
    assert_eq!(
        body("        return -extra ** extra ** 2;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                UNARY_MINUS
                  BINARY_OP [12]
                    VAR
                      ZVAL "extra"
                    BINARY_OP [12]
                      VAR
                        ZVAL "extra"
                      ZVAL 2
        "#}
    );
}

/// ```php
/// return $extra ?? $this->total() ?? 0;
/// ```
///
/// `??` is right-associative, as php-src's grammar declares it.
#[test]
fn null_coalescing_is_coalesce() {
    assert_eq!(
        body("        return extra ?? this.total() ?? 0;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                COALESCE
                  VAR
                    ZVAL "extra"
                  COALESCE
                    METHOD_CALL
                      VAR
                        ZVAL "this"
                      ZVAL "total"
                      ARG_LIST
                    ZVAL 0
        "#}
    );
}

/// ```php
/// return ($extra > 1 ? true : false) ? $extra : ($extra < 0 ? 0 : 1);
/// ```
///
/// A ternary in parentheses carries `ZEND_PARENTHESIZED_CONDITIONAL`, as php-src's grammar marks `'(' expr ')'`, so
/// the engine accepts it as the condition of another ternary.
#[test]
fn the_ternary_is_a_conditional_marked_when_parenthesized() {
    assert_eq!(
        body("        return (extra > 1 ? true : false) ? extra : (extra < 0 ? 0 : 1);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CONDITIONAL
                  CONDITIONAL [1]
                    GREATER
                      VAR
                        ZVAL "extra"
                      ZVAL 1
                    ZVAL true
                    ZVAL false
                  VAR
                    ZVAL "extra"
                  CONDITIONAL [1]
                    BINARY_OP [20]
                      VAR
                        ZVAL "extra"
                      ZVAL 0
                    ZVAL 0
                    ZVAL 1
        "#}
    );
}

/// ```php
/// $cents = (int)($extra * 1.5); return (string)(float)$cents;
/// ```
///
/// A cast's attr is the type it converts to, `IS_LONG`, `IS_DOUBLE` or `IS_STRING`, as php-src's grammar builds it.
#[test]
fn casts_between_numbers_are_casts_to_their_types() {
    assert_eq!(
        body("        const cents = (int)(extra * 1.5);\n        return (string)(float)cents;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "cents"
                CAST [4]
                  BINARY_OP [3]
                    VAR
                      ZVAL "extra"
                    ZVAL 1.5
              RETURN
                CAST [6]
                  CAST [5]
                    VAR
                      ZVAL "cents"
        "#}
    );
}

/// ```php
/// $extra ??= 1; $this->count ??= $extra;
/// ```
#[test]
fn null_coalescing_assignment_is_assign_coalesce() {
    assert_eq!(
        body("        extra ??= 1;\n        this.count ??= extra;\n        return extra;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN_COALESCE
                VAR
                  ZVAL "extra"
                ZVAL 1
              ASSIGN_COALESCE
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "count"
                VAR
                  ZVAL "extra"
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// return $this?->total($extra)?->value->cents;
/// ```
#[test]
fn null_safe_calls_and_reads_are_nullsafe_kinds() {
    assert_eq!(
        body("        return this?.total(extra)?.value.cents;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                PROP
                  NULLSAFE_PROP
                    NULLSAFE_METHOD_CALL
                      VAR
                        ZVAL "this"
                      ZVAL "total"
                      ARG_LIST
                        VAR
                          ZVAL "extra"
                    ZVAL "value"
                  ZVAL "cents"
        "#}
    );
}

/// ```php
/// return ($extra + 1) * 2;
/// ```
#[test]
fn parentheses_only_group() {
    assert_eq!(
        body("        return (extra + 1) * 2;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [3]
                  BINARY_OP [1]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
                  ZVAL 2
        "#}
    );
}

/// ```php
/// {
///     $inner = PHP_INT_MAX;
/// }
/// ```
///
/// `[1]` on the constant's name is `ZEND_NAME_NOT_FQ`: like PHP, the engine looks the constant up in the namespace,
/// then globally.
#[test]
fn a_block_is_a_statement_list_and_a_constant_is_looked_up_by_its_short_name() {
    assert_eq!(
        body("        {\n            let inner = PHP_INT_MAX;\n        }\n        return 1;\n"),
        indoc! {r#"
            STMT_LIST
              STMT_LIST
                ASSIGN
                  VAR
                    ZVAL "inner"
                  CONST
                    ZVAL [1] "PHP_INT_MAX"
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// if ($extra > 1) {
///     return 1;
/// } else if ($extra < 0) {
///     return 2;
/// } else {
///     return 3;
/// }
/// ```
///
/// php-src reads `else if` as an `else` whose statement is the next `if`. An `else` has a null condition.
#[test]
fn if_else_if_and_else_are_if_lists_of_if_elems() {
    assert_eq!(
        body(
            "        if (extra > 1) {\n            return 1;\n        } else if (extra < 0) {\n            return 2;\n        } else {\n            return 3;\n        }\n"
        ),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  GREATER
                    VAR
                      ZVAL "extra"
                    ZVAL 1
                  STMT_LIST
                    RETURN
                      ZVAL 1
                IF_ELEM
                  null
                  IF
                    IF_ELEM
                      BINARY_OP [20]
                        VAR
                          ZVAL "extra"
                        ZVAL 0
                      STMT_LIST
                        RETURN
                          ZVAL 2
                    IF_ELEM
                      null
                      STMT_LIST
                        RETURN
                          ZVAL 3
        "#}
    );
}

/// ```php
/// while ($extra > 0) {
///     $extra -= 1;
/// }
/// do {
///     $extra += 1;
/// } while ($extra < 3);
/// ```
///
/// `WHILE` takes its condition first, and `DO_WHILE` its body first.
#[test]
fn while_and_do_while_are_their_php_kinds() {
    assert_eq!(
        body(
            "        while (extra > 0) {\n            extra -= 1;\n        }\n        do {\n            extra += 1;\n        } while (extra < 3);\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              WHILE
                GREATER
                  VAR
                    ZVAL "extra"
                  ZVAL 0
                STMT_LIST
                  ASSIGN_OP [2]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
              DO_WHILE
                STMT_LIST
                  ASSIGN_OP [1]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
                BINARY_OP [20]
                  VAR
                    ZVAL "extra"
                  ZVAL 3
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// while (true) {
///     continue;
///     break;
/// }
/// ```
///
/// A `break` or `continue` without a level has a null depth.
#[test]
fn break_and_continue_have_a_null_depth() {
    assert_eq!(
        body("        while (true) {\n            continue;\n            break;\n        }\n        return 1;\n"),
        indoc! {"
            STMT_LIST
              WHILE
                ZVAL true
                STMT_LIST
                  CONTINUE
                    null
                  BREAK
                    null
              RETURN
                ZVAL 1
        "}
    );
}

/// ```php
/// for ($step = 0; $step < $extra; $step++, $extra--) {
/// }
/// for (;;) {
///     break;
/// }
/// ```
///
/// Each part of the header is an `EXPR_LIST`, or null when it is empty. A `let` counter is the assignment of its
/// value.
#[test]
fn for_loops_are_for_nodes_with_an_expression_list_per_part() {
    assert_eq!(
        body(
            "        for (let step = 0; step < extra; step++, extra--) {\n        }\n        for (;;) {\n            break;\n        }\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              FOR
                EXPR_LIST
                  ASSIGN
                    VAR
                      ZVAL "step"
                    ZVAL 0
                EXPR_LIST
                  BINARY_OP [20]
                    VAR
                      ZVAL "step"
                    VAR
                      ZVAL "extra"
                EXPR_LIST
                  POST_INC
                    VAR
                      ZVAL "step"
                  POST_DEC
                    VAR
                      ZVAL "extra"
                STMT_LIST
              FOR
                null
                null
                null
                STMT_LIST
                  BREAK
                    null
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// A typed counter lowers as a `let` counter does: its type only tells the checker the counter's type.
///
/// ```php
/// for ($step = 0; $step < $extra; ) {
/// }
/// ```
#[test]
fn a_typed_for_counter_is_the_assignment_of_its_value() {
    assert_eq!(
        body("        for (int step = 0; step < extra; ) {\n        }\n        return extra;\n"),
        indoc! {r#"
            STMT_LIST
              FOR
                EXPR_LIST
                  ASSIGN
                    VAR
                      ZVAL "step"
                    ZVAL 0
                EXPR_LIST
                  BINARY_OP [20]
                    VAR
                      ZVAL "step"
                    VAR
                      ZVAL "extra"
                null
                STMT_LIST
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// foreach (\Lib\Calc::make(2) as $value) {
///     $extra += $value;
/// }
/// foreach (\Lib\Calc::make(3) as $key => $value) {
/// }
/// ```
///
/// `FOREACH` takes the collection, the value variable, the key variable or null, and the body.
#[test]
fn for_of_loops_are_foreach_nodes_with_the_value_before_the_key() {
    assert_eq!(
        body(
            "        for (const value of Calc.make(2)) {\n            extra += value;\n        }\n        for (let [key, value] of Calc.make(3)) {\n        }\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              FOREACH
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "make"
                  ARG_LIST
                    ZVAL 2
                VAR
                  ZVAL "value"
                null
                STMT_LIST
                  ASSIGN_OP [1]
                    VAR
                      ZVAL "extra"
                    VAR
                      ZVAL "value"
              FOREACH
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "make"
                  ARG_LIST
                    ZVAL 3
                VAR
                  ZVAL "value"
                VAR
                  ZVAL "key"
                STMT_LIST
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// throw new \App\Tenant\Failure($extra);
/// return $extra ?? throw new \App\Tenant\Failure(0);
/// ```
///
/// `throw` is an expression, and a `throw` statement is that expression alone.
#[test]
fn throw_is_a_throw_expression() {
    assert_eq!(
        body("        throw new Failure(extra);\n        return extra ?? throw new Failure(0);\n"),
        indoc! {r#"
            STMT_LIST
              THROW
                NEW
                  ZVAL "App\\Tenant\\Failure"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
              RETURN
                COALESCE
                  VAR
                    ZVAL "extra"
                  THROW
                    NEW
                      ZVAL "App\\Tenant\\Failure"
                      ARG_LIST
                        ZVAL 0
        "#}
    );
}

/// ```php
/// try {
///     $extra += 1;
/// } catch (\Lib\Calc | \App\Tenant\Missing $failure) {
///     throw $failure;
/// } catch (\App\Tenant\Broken) {
/// } finally {
///     $extra -= 1;
/// }
/// ```
///
/// `TRY` takes the try block, a `CATCH_LIST` and the finally block or null. Each `CATCH` takes a `NAME_LIST` of
/// classes, the variable's name or null, and its block.
#[test]
fn try_is_a_try_node_with_a_catch_list_and_a_finally_block() {
    assert_eq!(
        body(
            "        try {\n            extra += 1;\n        } catch (Calc | Missing failure) {\n            throw failure;\n        } catch (Broken) {\n        } finally {\n            extra -= 1;\n        }\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              TRY
                STMT_LIST
                  ASSIGN_OP [1]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
                CATCH_LIST
                  CATCH
                    NAME_LIST
                      ZVAL "Lib\\Calc"
                      ZVAL "App\\Tenant\\Missing"
                    ZVAL "failure"
                    STMT_LIST
                      THROW
                        VAR
                          ZVAL "failure"
                  CATCH
                    NAME_LIST
                      ZVAL "App\\Tenant\\Broken"
                    null
                    STMT_LIST
                STMT_LIST
                  ASSIGN_OP [2]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// try {
/// } finally {
/// }
/// ```
#[test]
fn try_without_a_catch_has_an_empty_catch_list() {
    assert_eq!(
        body("        try {\n        } finally {\n        }\n        return extra;\n"),
        indoc! {r#"
            STMT_LIST
              TRY
                STMT_LIST
                CATCH_LIST
                STMT_LIST
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// return \strlen(\sprintf("%d", \count($extra, mode: 0)));
/// ```
///
/// PHP# calls only PHP's built-in functions, so a call names the global function with `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_function_call_is_a_call_of_the_global_function() {
    assert_eq!(
        body("        return strlen(sprintf(\"%d\", count(extra, mode: 0)));\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "strlen"
                  ARG_LIST
                    CALL
                      ZVAL "sprintf"
                      ARG_LIST
                        ZVAL "%d"
                        CALL
                          ZVAL "count"
                          ARG_LIST
                            VAR
                              ZVAL "extra"
                            NAMED_ARG
                              ZVAL "mode"
                              ZVAL 0
        "#}
    );
}

/// ```php
/// $label = "Order {$extra}: " . \strlen("x") . "!"; $alone = "{$extra}"; $plain = "plain A"; return $extra;
/// ```
///
/// php-src's grammar builds an interpolated string as an `ENCAPS_LIST` of its text and its expressions, and a
/// string without one as a `ZVAL`. A template's `${…}` takes any expression, which `ENCAPS_LIST` compiles as PHP's
/// `{$…}` does.
#[test]
fn a_template_is_an_encaps_list_of_its_text_and_interpolations() {
    assert_eq!(
        body(
            "        const label = `Order ${extra}: ${strlen(\"x\")}!`;\n        const alone = `${extra}`;\n        const plain = `plain \\x41`;\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "label"
                ENCAPS_LIST
                  ZVAL "Order "
                  VAR
                    ZVAL "extra"
                  ZVAL ": "
                  CALL
                    ZVAL "strlen"
                    ARG_LIST
                      ZVAL "x"
                  ZVAL "!"
              ASSIGN
                VAR
                  ZVAL "alone"
                ENCAPS_LIST
                  VAR
                    ZVAL "extra"
              ASSIGN
                VAR
                  ZVAL "plain"
                ZVAL "plain A"
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// php-src takes a list's line from its first child, and each piece of text's from where it starts.
#[test]
fn a_template_over_several_lines_keeps_the_line_of_each_part() {
    let lowered = Lowered::new(&method("        return `total:\n${extra} more`;\n"));
    let list = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_ENCAPS_LIST).expect("a list");
    let lines: Vec<u32> =
        (0..3).map(|index| lowered.nodes()[lowered.child(list as u32, index) as usize].line).collect();

    assert_eq!(lowered.nodes()[list].line, 9);
    assert_eq!(lines, [9, 10, 10]);
}

/// ```php
/// #[\Lib\Entity(label: "Orders", order: 2 * 3), \App\Tenant\Searchable]
/// #[\Lib\Entity(null)]
/// class Report
/// {
///     #[\Lib\Field] private int $count = 0;
///
///     public function __construct(#[\Lib\Field(-1.5)] public readonly int $total)
///     {
///     }
///
///     #[\Lib\Entity(true, PHP_INT_MAX)]
///     public function run(#[\Lib\Field] int $extra): void
///     {
///     }
/// }
/// ```
///
/// Each `#[...]` is an `ATTRIBUTE_GROUP`, and a declaration's groups are one `ATTRIBUTE_LIST` in the child
/// `zend_ast_with_attributes` gives it: the 4th of a class, the 5th of a method, the 3rd of a property group and the
/// 4th of a parameter.
#[test]
fn attributes_are_attribute_lists_of_attribute_groups_on_their_declarations() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Field;\nimport Lib.Entity;\n\n[Entity(label: \"Orders\", order: 2 * 3), Searchable]\n[Entity(null)]\nclass Report\n{\n    [Field] private int count = 0;\n\n    public Report([Field(-1.5)] public int total { get; })\n    {\n    }\n\n    [Entity(true, PHP_INT_MAX)]\n    public void run([Field] int extra)\n    {\n    }\n}\n",
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.unit().root, 2)),
        indoc! {r#"
            CLASS "Report" @8-20
              null
              null
              STMT_LIST
                PROP_GROUP [4]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "count"
                      ZVAL 0
                      null
                      null
                  ATTRIBUTE_LIST
                    ATTRIBUTE_GROUP
                      ATTRIBUTE
                        ZVAL "Lib\\Field"
                        null
                METHOD [1] "__construct" @12-14
                  PARAM_LIST
                    PARAM [129]
                      ZVAL [1] "int"
                      ZVAL "total"
                      null
                      ATTRIBUTE_LIST
                        ATTRIBUTE_GROUP
                          ATTRIBUTE
                            ZVAL "Lib\\Field"
                            ARG_LIST
                              UNARY_MINUS
                                ZVAL 1.5
                      null
                      null
                  null
                  STMT_LIST
                  null
                  null
                METHOD [1] "run" @17-19
                  PARAM_LIST
                    PARAM
                      ZVAL [1] "int"
                      ZVAL "extra"
                      null
                      ATTRIBUTE_LIST
                        ATTRIBUTE_GROUP
                          ATTRIBUTE
                            ZVAL "Lib\\Field"
                            null
                      null
                      null
                  null
                  STMT_LIST
                  ZVAL [1] "void"
                  ATTRIBUTE_LIST
                    ATTRIBUTE_GROUP
                      ATTRIBUTE
                        ZVAL "Lib\\Entity"
                        ARG_LIST
                          ZVAL true
                          CONST
                            ZVAL [1] "PHP_INT_MAX"
              ATTRIBUTE_LIST
                ATTRIBUTE_GROUP
                  ATTRIBUTE
                    ZVAL "Lib\\Entity"
                    ARG_LIST
                      NAMED_ARG
                        ZVAL "label"
                        ZVAL "Orders"
                      NAMED_ARG
                        ZVAL "order"
                        BINARY_OP [3]
                          ZVAL 2
                          ZVAL 3
                  ATTRIBUTE
                    ZVAL "App\\Tenant\\Searchable"
                    null
                ATTRIBUTE_GROUP
                  ATTRIBUTE
                    ZVAL "Lib\\Entity"
                    ARG_LIST
                      ZVAL null
              null
        "#}
    );
}

/// ```php
/// $a = $extra instanceof \Lib\Calc; $b = \is_int($extra); $c = \is_string($extra);
/// $d = (${'match#1'} = $this->total()) instanceof \Lib\Calc;
/// ```
///
/// A class is tested with `instanceof` on its full name, and a built-in type with the global `is_` function the
/// engine compiles to `TYPE_CHECK`. A value that is not a local or a parameter is tested through a variable PHP#
/// cannot name, so the analyzer never narrows a property.
#[test]
fn is_without_a_name_is_instanceof_or_a_type_check() {
    assert_eq!(
        body(
            "        const a = extra is Calc;\n        const b = extra is int;\n        const c = extra is string;\n        const d = this.total() is Calc;\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                INSTANCEOF
                  VAR
                    ZVAL "extra"
                  ZVAL "Lib\\Calc"
              ASSIGN
                VAR
                  ZVAL "b"
                CALL
                  ZVAL "is_int"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
              ASSIGN
                VAR
                  ZVAL "c"
                CALL
                  ZVAL "is_string"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
              ASSIGN
                VAR
                  ZVAL "d"
                INSTANCEOF
                  ASSIGN
                    VAR
                      ZVAL "match#1"
                    METHOD_CALL
                      VAR
                        ZVAL "this"
                      ZVAL "total"
                      ARG_LIST
                  ZVAL "Lib\\Calc"
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// if (($calc = $extra) instanceof \Lib\Calc) { return 1; }
/// if (!\is_int($count = $extra)) { return 0; }
/// return $count;
/// ```
///
/// A pattern's variable is assigned the tested value before the test. The binder lets code read it only where the
/// test holds, so the assignment is never seen where it fails.
#[test]
fn is_with_a_name_assigns_the_name_before_the_test() {
    assert_eq!(
        body(
            "        if (extra is Calc calc) {\n            return 1;\n        }\n        if (extra is not int count) {\n            return 0;\n        }\n        return count;\n"
        ),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  INSTANCEOF
                    ASSIGN
                      VAR
                        ZVAL "calc"
                      VAR
                        ZVAL "extra"
                    ZVAL "Lib\\Calc"
                  STMT_LIST
                    RETURN
                      ZVAL 1
              IF
                IF_ELEM
                  UNARY_OP [14]
                    CALL
                      ZVAL "is_int"
                      ARG_LIST
                        ASSIGN
                          VAR
                            ZVAL "count"
                          VAR
                            ZVAL "extra"
                  STMT_LIST
                    RETURN
                      ZVAL 0
              RETURN
                VAR
                  ZVAL "count"
        "#}
    );
}

/// ```php
/// $calc = $extra instanceof \Lib\Calc ? $extra : null;
/// $made = (${'as#1'} = \Lib\Calc::make()) instanceof \Lib\Calc ? ${'as#1'} : null;
/// ```
///
/// `as` reads a local or a parameter twice. Any other value is assigned to a variable PHP# cannot name, so it runs
/// once.
#[test]
fn as_is_a_conditional_that_gives_the_value_or_null() {
    assert_eq!(
        body("        const calc = extra as Calc;\n        const made = Calc.make() as Calc;\n        return extra;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "calc"
                CONDITIONAL
                  INSTANCEOF
                    VAR
                      ZVAL "extra"
                    ZVAL "Lib\\Calc"
                  VAR
                    ZVAL "extra"
                  ZVAL null
              ASSIGN
                VAR
                  ZVAL "made"
                CONDITIONAL
                  INSTANCEOF
                    ASSIGN
                      VAR
                        ZVAL "as#1"
                      STATIC_CALL
                        ZVAL "Lib\\Calc"
                        ZVAL "make"
                        ARG_LIST
                    ZVAL "Lib\\Calc"
                  VAR
                    ZVAL "as#1"
                  ZVAL null
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// $limit = 10;
/// $a = $extra === 200;
/// $b = $extra >= 1 && $extra < $limit || !($extra === -1);
/// $c = $extra === $limit;
/// $d = \is_object($extra) && (${'match#1'} = $extra->count) > 0 && \is_string($label = $extra->name);
/// ```
///
/// A value is compared with `===`, a comparison keeps its operator, and `and`, `or` and `not` are `&&`, `||` and
/// `!`. A bare name is the local's value when a local of that name is in scope. A properties pattern tests that the
/// value is an object, then reads each property once.
#[test]
fn values_comparisons_and_properties_are_the_php_comparisons_they_name() {
    assert_eq!(
        body(
            "        let limit = 10;\n        const a = extra is 200;\n        const b = extra is >= 1 and < limit or not -1;\n        const c = extra is limit;\n        const d = extra is { count: > 0, name: string label };\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "limit"
                ZVAL 10
              ASSIGN
                VAR
                  ZVAL "a"
                BINARY_OP [16]
                  VAR
                    ZVAL "extra"
                  ZVAL 200
              ASSIGN
                VAR
                  ZVAL "b"
                OR
                  AND
                    GREATER_EQUAL
                      VAR
                        ZVAL "extra"
                      ZVAL 1
                    BINARY_OP [20]
                      VAR
                        ZVAL "extra"
                      VAR
                        ZVAL "limit"
                  UNARY_OP [14]
                    BINARY_OP [16]
                      VAR
                        ZVAL "extra"
                      UNARY_MINUS
                        ZVAL 1
              ASSIGN
                VAR
                  ZVAL "c"
                BINARY_OP [16]
                  VAR
                    ZVAL "extra"
                  VAR
                    ZVAL "limit"
              ASSIGN
                VAR
                  ZVAL "d"
                AND
                  AND
                    CALL
                      ZVAL "is_object"
                      ARG_LIST
                        VAR
                          ZVAL "extra"
                    GREATER
                      ASSIGN
                        VAR
                          ZVAL "match#1"
                        PROP
                          VAR
                            ZVAL "extra"
                          ZVAL "count"
                      ZVAL 0
                  CALL
                    ZVAL "is_string"
                    ARG_LIST
                      ASSIGN
                        VAR
                          ZVAL "label"
                        PROP
                          VAR
                            ZVAL "extra"
                          ZVAL "name"
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// return match (true) {
///     (${'match#1'} = $this->total()) === 0 => 1,
///     \is_int($n = ${'match#1'}) && $n > 9 => $n,
///     default => 2,
/// };
/// ```
///
/// A `match` that gives a value is PHP's `match (true)` with one condition per arm: its pattern's test, `&&` its
/// `when` condition. The first test assigns the hidden variable.
#[test]
fn a_match_that_gives_a_value_is_a_match_of_true() {
    assert_eq!(
        body(
            "        return match (this.total()) {\n            0 => 1,\n            int n when n > 9 => n,\n            default => 2,\n        };\n"
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                MATCH
                  ZVAL true
                  MATCH_ARM_LIST
                    MATCH_ARM
                      EXPR_LIST
                        BINARY_OP [16]
                          ASSIGN
                            VAR
                              ZVAL "match#1"
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "total"
                              ARG_LIST
                          ZVAL 0
                      ZVAL 1
                    MATCH_ARM
                      EXPR_LIST
                        AND
                          CALL
                            ZVAL "is_int"
                            ARG_LIST
                              ASSIGN
                                VAR
                                  ZVAL "n"
                                VAR
                                  ZVAL "match#1"
                          GREATER
                            VAR
                              ZVAL "n"
                            ZVAL 9
                      VAR
                        ZVAL "n"
                    MATCH_ARM
                      null
                      ZVAL 2
        "#}
    );
}

/// ```php
/// if ($extra === 0) {
///     return 1;
/// } else if (\is_int($n = $extra) && $n > 9) $this->run($n); else {
/// }
/// ```
///
/// A `match` that starts a statement is `if`, `else if` per arm, and `else` for `default`. An arm that is an
/// expression is its expression statement.
#[test]
fn a_match_that_starts_a_statement_is_an_if_list() {
    assert_eq!(
        body(
            "        match (extra) {\n            0 => {\n                return 1;\n            },\n            int n when n > 9 => this.run(n),\n            default => {},\n        }\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  BINARY_OP [16]
                    VAR
                      ZVAL "extra"
                    ZVAL 0
                  STMT_LIST
                    RETURN
                      ZVAL 1
                IF_ELEM
                  null
                  IF
                    IF_ELEM
                      AND
                        CALL
                          ZVAL "is_int"
                          ARG_LIST
                            ASSIGN
                              VAR
                                ZVAL "n"
                              VAR
                                ZVAL "extra"
                        GREATER
                          VAR
                            ZVAL "n"
                          ZVAL 9
                      METHOD_CALL
                        VAR
                          ZVAL "this"
                        ZVAL "run"
                        ARG_LIST
                          VAR
                            ZVAL "n"
                    IF_ELEM
                      null
                      STMT_LIST
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// The child count `zend_ast_get_num_children` gives a fixed-size kind, or 5 for a declaration. `None` for a list.
fn fixed_child_count(kind: sharp_kind) -> Option<u32> {
    match kind {
        sharp_kind::SHARP_AST_ARG_LIST
        | sharp_kind::SHARP_AST_STMT_LIST
        | sharp_kind::SHARP_AST_PARAM_LIST
        | sharp_kind::SHARP_AST_CONST_DECL
        | sharp_kind::SHARP_AST_IF
        | sharp_kind::SHARP_AST_EXPR_LIST
        | sharp_kind::SHARP_AST_PROP_DECL
        | sharp_kind::SHARP_AST_ATTRIBUTE_LIST
        | sharp_kind::SHARP_AST_ATTRIBUTE_GROUP
        | sharp_kind::SHARP_AST_CATCH_LIST
        | sharp_kind::SHARP_AST_NAME_LIST
        | sharp_kind::SHARP_AST_ENCAPS_LIST
        | sharp_kind::SHARP_AST_MATCH_ARM_LIST => None,
        sharp_kind::SHARP_AST_ZVAL => Some(0),
        sharp_kind::SHARP_AST_VAR
        | sharp_kind::SHARP_AST_CONST
        | sharp_kind::SHARP_AST_UNARY_PLUS
        | sharp_kind::SHARP_AST_UNARY_MINUS
        | sharp_kind::SHARP_AST_UNARY_OP
        | sharp_kind::SHARP_AST_PRE_INC
        | sharp_kind::SHARP_AST_PRE_DEC
        | sharp_kind::SHARP_AST_POST_INC
        | sharp_kind::SHARP_AST_POST_DEC
        | sharp_kind::SHARP_AST_RETURN
        | sharp_kind::SHARP_AST_BREAK
        | sharp_kind::SHARP_AST_CONTINUE
        | sharp_kind::SHARP_AST_THROW
        | sharp_kind::SHARP_AST_CAST
        | sharp_kind::SHARP_AST_PROPERTY_HOOK_SHORT_BODY => Some(1),
        sharp_kind::SHARP_AST_PROP
        | sharp_kind::SHARP_AST_ASSIGN
        | sharp_kind::SHARP_AST_ASSIGN_OP
        | sharp_kind::SHARP_AST_BINARY_OP
        | sharp_kind::SHARP_AST_GREATER
        | sharp_kind::SHARP_AST_GREATER_EQUAL
        | sharp_kind::SHARP_AST_AND
        | sharp_kind::SHARP_AST_OR
        | sharp_kind::SHARP_AST_DECLARE
        | sharp_kind::SHARP_AST_NAMESPACE
        | sharp_kind::SHARP_AST_NAMED_ARG
        | sharp_kind::SHARP_AST_COALESCE
        | sharp_kind::SHARP_AST_ASSIGN_COALESCE
        | sharp_kind::SHARP_AST_NULLSAFE_PROP
        | sharp_kind::SHARP_AST_IF_ELEM
        | sharp_kind::SHARP_AST_WHILE
        | sharp_kind::SHARP_AST_DO_WHILE
        | sharp_kind::SHARP_AST_NEW
        | sharp_kind::SHARP_AST_ATTRIBUTE
        | sharp_kind::SHARP_AST_CALL
        | sharp_kind::SHARP_AST_INSTANCEOF
        | sharp_kind::SHARP_AST_MATCH
        | sharp_kind::SHARP_AST_MATCH_ARM => Some(2),
        sharp_kind::SHARP_AST_METHOD_CALL
        | sharp_kind::SHARP_AST_STATIC_CALL
        | sharp_kind::SHARP_AST_CONST_ELEM
        | sharp_kind::SHARP_AST_NULLSAFE_METHOD_CALL
        | sharp_kind::SHARP_AST_PROP_GROUP
        | sharp_kind::SHARP_AST_TRY
        | sharp_kind::SHARP_AST_CATCH
        | sharp_kind::SHARP_AST_CONDITIONAL => Some(3),
        sharp_kind::SHARP_AST_FOR | sharp_kind::SHARP_AST_FOREACH | sharp_kind::SHARP_AST_PROP_ELEM => Some(4),
        sharp_kind::SHARP_AST_METHOD | sharp_kind::SHARP_AST_CLASS | sharp_kind::SHARP_AST_PROPERTY_HOOK => Some(5),
        sharp_kind::SHARP_AST_PARAM => Some(6),
    }
}

/// The fixture holds every construct the checker accepts, so the bridge lowers all of them, and each fixed-size node
/// has the child count of its kind.
#[test]
fn every_construct_of_the_slice_lowers_into_nodes_of_their_kinds_child_count() {
    let lowered = Lowered::new(include_str!("../../semantics/tests/fixtures/slice.sharp"));
    assert_eq!(lowered.diagnostics(), Vec::<String>::new());

    let wrong: Vec<String> = lowered
        .nodes()
        .iter()
        .filter(|node| fixed_child_count(node.kind).is_some_and(|count| count != node.child_count))
        .map(|node| format!("{:?} has {} children", node.kind, node.child_count))
        .collect();

    assert_eq!(wrong, Vec::<String>::new());
}

#[test]
fn each_node_carries_the_line_of_its_first_token() {
    let lowered = Lowered::new(&method("        let total =\n            extra;\n        return total;\n"));
    let lines: Vec<(String, u32)> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_ZVAL && node.value == sharp_value::SHARP_STRING)
        .map(|node| (text(node.text), node.line))
        .filter(|(name, _)| name == "total" || name == "extra")
        .collect();

    assert_eq!(
        lines,
        [("extra".to_owned(), 7), ("total".to_owned(), 9), ("extra".to_owned(), 10), ("total".to_owned(), 11)]
    );
}
