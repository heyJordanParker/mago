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
        let path = "src/Report.sharp";

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
            sharp_kind::SHARP_AST_CLASS | sharp_kind::SHARP_AST_METHOD => {
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

/// PHP's compiler refuses the same string with this message, so the engine never runs it.
#[test]
fn an_invalid_unicode_escape_returns_the_engine_compile_error() {
    let lowered = Lowered::new(&method("        let text = \"\\u{110000}\";\n        return 1;\n"));

    assert_eq!(lowered.diagnostics(), ["9:20 compile error: Invalid UTF-8 codepoint escape sequence"]);
    assert_eq!(lowered.unit().node_count, 0);
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
/// namespace App\Tenant {
///     class Report {}
/// }
/// ```
#[test]
fn a_braced_namespace_holds_its_classes() {
    let lowered = Lowered::new("namespace App.Tenant {\n    class Report {}\n}\n");

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
                STMT_LIST
                  CLASS "Report" @2-2
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
/// public static function make(): void {}
/// private function hide() {}
/// protected function share() {}
/// function open() {}
/// ```
///
/// The flags are `ZEND_ACC_PUBLIC | ZEND_ACC_STATIC`, `ZEND_ACC_PRIVATE`, `ZEND_ACC_PROTECTED`, and
/// `ZEND_ACC_PUBLIC` when no access modifier is written.
#[test]
fn method_modifiers_become_the_method_flags() {
    let lowered = Lowered::new(
        "class Report\n{\n    public static void make() {}\n    private void hide() {}\n    protected void share() {}\n    void open() {}\n}\n",
    );
    let methods: Vec<(String, u32)> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD)
        .map(|node| (text(node.text), node.attr))
        .collect();

    assert_eq!(
        methods,
        [("make".to_owned(), 17), ("hide".to_owned(), 4), ("share".to_owned(), 2), ("open".to_owned(), 1)]
    );
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
/// $a -= 1; $a *= 2; $a /= 3;
/// ```
///
/// `[2]`, `[3]` and `[4]` are `ZEND_SUB`, `ZEND_MUL` and `ZEND_DIV`.
#[test]
fn compound_assignments_are_assign_ops() {
    let tree = body("        let a = 1;\n        a -= 1;\n        a *= 2;\n        a /= 3;\n        return a;\n");
    let operators: Vec<&str> = tree.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")).collect();

    assert_eq!(operators, ["  ASSIGN", "  ASSIGN_OP [2]", "  ASSIGN_OP [3]", "  ASSIGN_OP [4]", "  RETURN"]);
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

/// The child count `zend_ast_get_num_children` gives a fixed-size kind, or 5 for a declaration. `None` for a list.
fn fixed_child_count(kind: sharp_kind) -> Option<u32> {
    match kind {
        sharp_kind::SHARP_AST_ARG_LIST
        | sharp_kind::SHARP_AST_STMT_LIST
        | sharp_kind::SHARP_AST_PARAM_LIST
        | sharp_kind::SHARP_AST_CONST_DECL => None,
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
        | sharp_kind::SHARP_AST_RETURN => Some(1),
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
        | sharp_kind::SHARP_AST_NAMED_ARG => Some(2),
        sharp_kind::SHARP_AST_METHOD_CALL | sharp_kind::SHARP_AST_STATIC_CALL | sharp_kind::SHARP_AST_CONST_ELEM => {
            Some(3)
        }
        sharp_kind::SHARP_AST_METHOD | sharp_kind::SHARP_AST_CLASS => Some(5),
        sharp_kind::SHARP_AST_PARAM => Some(6),
    }
}

#[test]
fn every_fixed_size_node_has_the_child_count_of_its_kind() {
    let lowered = Lowered::new(&method(
        "        let a = -extra + +1;\n        a = !true && false || a > 1 && a >= 2;\n        ++a;\n        --a;\n        a++;\n        a--;\n        this.count *= PHP_INT_MAX;\n        {\n            const b = this.total(a, rate: 2).value;\n        }\n        return Calc.make();\n",
    ));
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
