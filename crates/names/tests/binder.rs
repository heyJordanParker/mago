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
use mago_names::resolver::NameResolver;
use mago_span::Position;
use mago_span::Span;
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

fn span(code: &str, needle: &str, nth: usize) -> Span {
    let start = Position::new(offset(code, needle, nth));
    let end = Position::new(start.offset + u32::try_from(needle.len()).expect("length fits in u32"));

    Span::new(FileId::new(FILE_NAME), start, end)
}

fn declared(code: &str, needle: &str, nth: usize, kind: LocalKind) -> Local {
    Local { declaration: span(code, needle, nth), kind }
}

#[test]
fn a_local_used_after_its_block_closes_is_a_constant_and_a_binding_error() {
    const CODE: &str = "class Report\n{\n    public int run()\n    {\n        {\n            let inner = 1;\n            inner;\n        }\n        return inner;\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "inner", 1), Some(local(CODE, "inner", 0, LocalKind::Let)));
    assert_eq!(binding(&names, CODE, "inner", 2), Some(Binding::Constant));
    assert_eq!(
        names.binding_errors(),
        [BindingError::OutOfScope { name: span(CODE, "inner", 2), local: declared(CODE, "inner", 0, LocalKind::Let) }]
    );
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
fn a_member_of_the_enclosing_class_without_this_is_recorded() {
    const CODE: &str = "class Report\n{\n    private int count()\n    {\n        return 1;\n    }\n\n    public int total()\n    {\n        return count() + total();\n    }\n}\n";
    let arena = LocalArena::new();
    let names = bind(&arena, CODE);

    assert_eq!(binding(&names, CODE, "count()", 1), Some(Binding::Member));
    assert_eq!(binding(&names, CODE, "total()", 1), Some(Binding::Member));
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
