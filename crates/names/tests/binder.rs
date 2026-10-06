#![allow(clippy::panic, clippy::expect_used)]

use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::BindingError;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_names::binding::php_variable_name;
use mago_names::resolver::NameResolver;
use mago_span::Position;
use mago_span::Span;
use mago_syntax::cst::Node;
use mago_syntax::parser::parse_file;

const FILE_NAME: &[u8] = b"src/Store.sharp";

fn bind<'arena>(arena: &'arena LocalArena, code: &'static str) -> ResolvedNames<'arena> {
    let file = File::ephemeral(Cow::Borrowed(FILE_NAME), Cow::Borrowed(code.as_bytes()));
    let program = parse_file(arena, &file);
    assert!(program.errors.is_empty(), "{:#?}", program.errors);

    NameResolver::new(arena).resolve(program)
}

/// The offset of the `nth` occurrence of `needle` in `code`, counting from zero.
fn offset(code: &str, needle: &str, nth: usize) -> u32 {
    let start = code.match_indices(needle).nth(nth).unwrap_or_else(|| panic!("no `{needle}` #{nth} in code")).0;

    u32::try_from(start).expect("offset fits in u32")
}

fn binding(names: &ResolvedNames<'_>, code: &str, needle: &str, nth: usize) -> Option<Binding> {
    names.binding(&Position::new(offset(code, needle, nth)))
}

fn local(code: &str, needle: &str, nth: usize, kind: LocalKind) -> Binding {
    Binding::Local(declared(code, needle, nth, kind))
}

fn resolved<'arena>(names: &ResolvedNames<'arena>, code: &str, needle: &str, nth: usize) -> &'arena [u8] {
    names.get(&Position::new(offset(code, needle, nth)))
}

#[test]
fn dotted_namespace_and_import_resolve_to_php_names() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.Money;\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(resolved(&names, CODE, "App.Tenant.Store", 0), b"App\\Tenant\\Store");
    assert_eq!(resolved(&names, CODE, "App.Shared.Money", 0), b"App\\Shared\\Money");
}

#[test]
fn the_names_in_a_class_or_interface_header_resolve_as_class_names() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.Entity;\n\nclass Page : Entity, Linkable\n{\n}\n\ninterface Linkable : Named\n{\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(resolved(&names, CODE, "Entity,", 0), b"App\\Shared\\Entity");
    assert_eq!(resolved(&names, CODE, "Linkable\n", 0), b"App\\Tenant\\Store\\Linkable");
    assert_eq!(resolved(&names, CODE, "Named", 0), b"App\\Tenant\\Store\\Named");
}

#[test]
fn the_interfaces_in_an_enum_header_resolve_as_class_names() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.HasLabel;\n\nenum Status : string, HasLabel, Sorted\n{\n}\n\nenum Suit : HasLabel\n{\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(resolved(&names, CODE, "HasLabel,", 0), b"App\\Shared\\HasLabel");
    assert_eq!(resolved(&names, CODE, "Sorted", 0), b"App\\Tenant\\Store\\Sorted");
    assert_eq!(resolved(&names, CODE, "HasLabel\n", 0), b"App\\Shared\\HasLabel");
}

#[test]
fn bare_names_bind_to_locals_this_classes_and_constants() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.Money;\n\nclass Report\n{\n    public int total(int extra)\n    {\n        let label = extra;\n        const base = Money.of(label);\n        Calc.make(this, base, PHP_EOL);\n        return extra;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "extra", 0), Some(local(CODE, "extra", 0, LocalKind::Parameter)));
    assert_eq!(binding(&names, CODE, "extra", 1), Some(local(CODE, "extra", 0, LocalKind::Parameter)));
    assert_eq!(binding(&names, CODE, "extra", 2), Some(local(CODE, "extra", 0, LocalKind::Parameter)));
    assert_eq!(binding(&names, CODE, "label", 0), Some(local(CODE, "label", 0, LocalKind::Let)));
    assert_eq!(binding(&names, CODE, "label", 1), Some(local(CODE, "label", 0, LocalKind::Let)));
    assert_eq!(binding(&names, CODE, "base", 1), Some(local(CODE, "base", 0, LocalKind::Const)));
    assert_eq!(binding(&names, CODE, "this", 0), Some(Binding::This));

    assert_eq!(binding(&names, CODE, "Money.of", 0), Some(Binding::Class));
    assert_eq!(resolved(&names, CODE, "Money.of", 0), b"App\\Shared\\Money");
    assert_eq!(binding(&names, CODE, "Calc", 0), Some(Binding::Class));
    assert_eq!(resolved(&names, CODE, "Calc", 0), b"App\\Tenant\\Store\\Calc");
    assert_eq!(binding(&names, CODE, "PHP_EOL", 0), Some(Binding::Constant));
}

#[test]
fn the_class_in_typeof_resolves_like_any_class_name() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.Money;\n\nclass Report\n{\n    public void total()\n    {\n        Store.keep(typeof(Money), typeof(Order));\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(resolved(&names, CODE, "Money)", 0), b"App\\Shared\\Money");
    assert_eq!(resolved(&names, CODE, "Order", 0), b"App\\Tenant\\Store\\Order");
    assert_eq!(binding(&names, CODE, "Order", 0), None);
}

#[test]
fn a_typed_local_binds_like_let_or_const_and_its_type_resolves() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.Money;\n\nclass Report\n{\n    public int total()\n    {\n        Money? found = null;\n        const int base = 2;\n        found = null;\n        return base;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "found", 1), Some(local(CODE, "found", 0, LocalKind::Let)));
    assert_eq!(binding(&names, CODE, "base", 1), Some(local(CODE, "base", 0, LocalKind::Const)));
    assert_eq!(resolved(&names, CODE, "Money?", 0), b"App\\Shared\\Money");
}

fn span(code: &str, needle: &str, nth: usize) -> Span {
    let start = Position::new(offset(code, needle, nth));
    let end = Position::new(start.offset + u32::try_from(needle.len()).expect("length fits in u32"));

    Span::new(FileId::new(FILE_NAME), start, end)
}

fn declared(code: &str, needle: &str, nth: usize, kind: LocalKind) -> Local {
    Local { declaration: span(code, needle, nth), kind }
}

#[test]
fn a_local_used_after_its_block_closes_binds_as_that_local_and_is_a_binding_error() {
    const CODE: &str = "class Report\n{\n    public int run()\n    {\n        {\n            let inner = 1;\n            inner;\n        }\n        return inner;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "inner", 1), Some(local(CODE, "inner", 0, LocalKind::Let)));
    assert_eq!(binding(&names, CODE, "inner", 2), Some(local(CODE, "inner", 0, LocalKind::Let)));
    assert_eq!(
        names.binding_errors(),
        [BindingError::OutOfScope { name: span(CODE, "inner", 2), local: declared(CODE, "inner", 0, LocalKind::Let) }]
    );
}

#[test]
fn a_for_counter_lives_until_its_loop_ends() {
    const CODE: &str = "class Report\n{\n    public int run()\n    {\n        for (let step = 0; step < 3; step++) {\n            step;\n        }\n        return step;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    for nth in 1..=3 {
        assert_eq!(binding(&names, CODE, "step", nth), Some(local(CODE, "step", 0, LocalKind::Let)), "`step` #{nth}");
    }
    assert_eq!(
        names.binding_errors(),
        [BindingError::OutOfScope { name: span(CODE, "step", 4), local: declared(CODE, "step", 0, LocalKind::Let) }]
    );
}

#[test]
fn a_typed_for_counter_binds_like_let_or_const() {
    const CODE: &str = "class Report\n{\n    public int run()\n    {\n        for (int step = 0; step < 3; step++) {\n        }\n        for (const int? once = null; once === null; ) {\n        }\n        return 0;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "step", 1), Some(local(CODE, "step", 0, LocalKind::Let)));
    assert_eq!(binding(&names, CODE, "once", 1), Some(local(CODE, "once", 0, LocalKind::Const)));
    assert_eq!(names.binding_errors(), []);
}

#[test]
fn for_of_loop_variables_live_until_their_loop_ends() {
    const CODE: &str = "class Report\n{\n    public int run(array values)\n    {\n        for (const [key, entry] of values) {\n            key;\n            entry;\n        }\n        for (let item of values) {\n            item;\n        }\n        return entry;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "key", 1), Some(local(CODE, "key", 0, LocalKind::Const)));
    assert_eq!(binding(&names, CODE, "entry", 1), Some(local(CODE, "entry", 0, LocalKind::Const)));
    assert_eq!(binding(&names, CODE, "item", 1), Some(local(CODE, "item", 0, LocalKind::Let)));
    assert!(binding(&names, CODE, "values", 1).is_some());
    assert_eq!(binding(&names, CODE, "values", 1), binding(&names, CODE, "values", 2));
    assert_eq!(
        names.binding_errors(),
        [BindingError::OutOfScope {
            name: span(CODE, "entry", 2),
            local: declared(CODE, "entry", 0, LocalKind::Const)
        }]
    );
}

#[test]
fn a_catch_variable_lives_until_its_catch_block_ends() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nclass Report\n{\n    public int run()\n    {\n        try {\n        } catch (Missing | Broken failure) {\n            failure;\n        } finally {\n            failure;\n        }\n        return 0;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "failure", 1), Some(local(CODE, "failure", 0, LocalKind::Let)));
    assert_eq!(resolved(&names, CODE, "Missing", 0), b"App\\Tenant\\Store\\Missing");
    assert_eq!(resolved(&names, CODE, "Broken", 0), b"App\\Tenant\\Store\\Broken");
    assert_eq!(
        names.binding_errors(),
        [BindingError::OutOfScope {
            name: span(CODE, "failure", 2),
            local: declared(CODE, "failure", 0, LocalKind::Let)
        }]
    );
}

#[test]
fn a_called_name_binds_as_a_local_when_one_is_declared_and_is_unbound_otherwise() {
    const CODE: &str = "class Report\n{\n    public int run(int count)\n    {\n        count(count);\n        return strlen(\"total\");\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "count", 1), Some(local(CODE, "count", 0, LocalKind::Parameter)));
    assert_eq!(binding(&names, CODE, "strlen", 0), None);
}

#[test]
fn redeclaring_a_name_an_enclosing_block_declares_is_a_binding_error() {
    const CODE: &str = "class Report\n{\n    public int run(int count)\n    {\n        let total = 1;\n        {\n            let total = 2;\n            let count = 3;\n        }\n        return total;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "total", 1), Some(local(CODE, "total", 1, LocalKind::Let)));
    assert_eq!(binding(&names, CODE, "total", 2), Some(local(CODE, "total", 0, LocalKind::Let)));
    assert_eq!(
        names.binding_errors(),
        [
            BindingError::Redeclared {
                name: span(CODE, "total", 1),
                earlier: declared(CODE, "total", 0, LocalKind::Let)
            },
            BindingError::Redeclared {
                name: span(CODE, "count", 1),
                earlier: declared(CODE, "count", 0, LocalKind::Parameter)
            },
        ]
    );
}

#[test]
fn locals_and_this_have_no_resolved_name() {
    const CODE: &str = "class Report\n{\n    public int run(int extra)\n    {\n        let label = extra;\n        return this.total(label);\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    for (needle, nth) in [("extra", 1), ("label", 1), ("this", 0)] {
        assert!(!names.contains(&Position::new(offset(CODE, needle, nth))), "`{needle}` #{nth} has a resolved name");
    }
}

#[test]
fn a_name_before_a_partial_method_application_is_a_class() {
    const CODE: &str = "class Report\n{\n    public void run()\n    {\n        Calc.make(...);\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "Calc", 0), Some(Binding::Class));
    assert_eq!(resolved(&names, CODE, "Calc", 0), b"Calc");
}

#[test]
fn a_name_before_null_safe_access_is_a_class_unless_it_is_a_local() {
    const CODE: &str = "class Report\n{\n    public void run(Calc calc)\n    {\n        Calc?.make();\n        Money?.rate;\n        calc?.add(1);\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "Calc?", 0), Some(Binding::Class));
    assert_eq!(binding(&names, CODE, "Money?", 0), Some(Binding::Class));
    assert_eq!(binding(&names, CODE, "calc?", 0), Some(local(CODE, "calc", 0, LocalKind::Parameter)));
}

#[test]
fn a_static_call_is_a_method_call_whose_object_binds_as_a_class() {
    const CODE: &str = "class Report\n{\n    public int run(Calc calc)\n    {\n        return Calc.make().add(1) + calc.add(2) + this.run(calc);\n    }\n}\n";
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Borrowed(FILE_NAME), Cow::Borrowed(CODE.as_bytes()));
    let program = parse_file(&arena, &file);
    let names = NameResolver::new(&arena).resolve(program);

    let classes = Node::Program(program).filter_map(|node| match node {
        Node::MethodCall(call) => Some(names.static_call_class(call).map(|class| class.name.value())),
        _ => None,
    });

    assert_eq!(classes, [Some(&b"Calc"[..]), None, None, None]);
}

#[test]
fn a_bare_int_or_float_before_a_dot_is_the_class_in_the_sharp_namespace() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nclass Report\n{\n    public float run(string text)\n    {\n        return Int.parse(text) + Float.tryParse(text) ?? 0.0;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "Int", 0), Some(Binding::Class));
    assert_eq!(resolved(&names, CODE, "Int", 0), b"Sharp\\Int");
    assert_eq!(binding(&names, CODE, "Float.", 0), Some(Binding::Class));
    assert_eq!(resolved(&names, CODE, "Float.", 0), b"Sharp\\Float");
}

#[test]
fn an_imported_int_is_the_imported_class() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.Int;\n\nclass Report\n{\n    public int run(string text)\n    {\n        return Int.parse(text);\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "Int.parse", 0), Some(Binding::Class));
    assert_eq!(resolved(&names, CODE, "Int.parse", 0), b"App\\Shared\\Int");
}

#[test]
fn locals_of_one_method_are_not_visible_in_another() {
    const CODE: &str = "class Report\n{\n    public int first()\n    {\n        let value = 1;\n        return value;\n    }\n\n    public int second()\n    {\n        return value;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "value", 2), Some(Binding::Constant));
}

#[test]
fn an_initializer_does_not_see_the_local_it_declares() {
    const CODE: &str =
        "class Report\n{\n    public int run()\n    {\n        let value = value;\n        return value;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "value", 1), Some(Binding::Constant));
    assert_eq!(binding(&names, CODE, "value", 2), Some(local(CODE, "value", 0, LocalKind::Let)));
}

#[test]
fn a_parameter_default_does_not_see_the_parameter_it_belongs_to() {
    const CODE: &str =
        "class Report\n{\n    public int run(int limit = limit)\n    {\n        return limit;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "limit", 1), Some(Binding::Constant));
    assert_eq!(binding(&names, CODE, "limit", 2), Some(local(CODE, "limit", 0, LocalKind::Parameter)));
}

#[test]
fn a_member_of_the_enclosing_class_without_this_is_recorded() {
    const CODE: &str = "class Report\n{\n    private int count()\n    {\n        return 1;\n    }\n\n    public int total()\n    {\n        return count() + total();\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "count()", 1), Some(Binding::Member));
    assert_eq!(binding(&names, CODE, "total()", 1), Some(Binding::Member));
}

#[test]
fn a_case_or_method_of_the_enclosing_enum_without_this_is_recorded() {
    const CODE: &str = "enum Status : string\n{\n    case Active = \"a\";\n\n    public string label()\n    {\n        return Active + label() + this.value;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "Active", 1), Some(Binding::Member));
    assert_eq!(binding(&names, CODE, "label()", 1), Some(Binding::Member));
    assert_eq!(binding(&names, CODE, "this", 0), Some(Binding::This));
}

#[test]
fn a_member_name_matches_as_php_matches_it() {
    const CODE: &str = "class Report\n{\n    const int RATE = 2;\n\n    public int total()\n    {\n        return Total() + Rate;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "Total()", 0), Some(Binding::Member));
    assert_eq!(binding(&names, CODE, "Rate", 0), Some(Binding::Constant));
}

#[test]
fn php_variable_name_adds_a_dollar_only_to_a_bare_name() {
    assert_eq!(php_variable_name(b"total").as_bytes(), b"$total");
    assert_eq!(php_variable_name(b"$total").as_bytes(), b"$total");
}

#[test]
fn php_names_are_not_bound() {
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Borrowed(b"src/Store.php"), Cow::Borrowed(b"<?php echo PHP_EOL;"));
    let program = parse_file(&arena, &file);
    let names = NameResolver::new(&arena).resolve(program);

    assert_eq!(names.binding(&Position::new(11)), None);
    assert_eq!(names.get(&Position::new(11)), b"PHP_EOL");
}
