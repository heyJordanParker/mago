#![allow(clippy::panic, clippy::expect_used, clippy::single_char_lifetime_names)]

use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_span::HasSpan;
use mago_syntax::cst::*;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file;

fn parse<'arena>(arena: &'arena LocalArena, name: &'static str, code: &'static str) -> &'arena Program<'arena> {
    let file = File::ephemeral(Cow::Borrowed(name.as_bytes()), Cow::Borrowed(code.as_bytes()));

    parse_file(arena, &file)
}

#[test]
fn sharp_file_starts_in_code() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Demo/Report.sharp", "namespace Demo;\n");

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    assert_eq!(program.dialect, Dialect::Sharp);
    assert!(matches!(program.statements.first(), Some(Statement::Namespace(_))), "{:#?}", program.statements);
}

fn source<'a>(code: &'a str, node: &impl HasSpan) -> &'a str {
    let span = node.span();

    &code[span.start.offset as usize..span.end.offset as usize]
}

#[test]
fn namespace_and_import_take_dotted_names() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.Money;\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Store.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(Statement::Namespace(namespace)) = program.statements.first() else {
        panic!("expected a namespace, got {:#?}", program.statements);
    };
    let name = namespace.name.expect("a namespace name");
    assert!(matches!(name, Identifier::Dotted(_)), "{name:?}");
    assert_eq!(name.value(), b"App.Tenant.Store");
    assert_eq!(source(CODE, &name), "App.Tenant.Store");

    let Some(Statement::Use(import)) = namespace.statements().first() else {
        panic!("expected an import, got {:#?}", namespace.statements());
    };
    assert_eq!(import.r#use.value, b"import");
    let UseItems::Sequence(items) = &import.items else {
        panic!("expected a plain import, got {:#?}", import.items);
    };
    let imported = items.items.first().expect("one imported class").name;
    assert!(matches!(imported, Identifier::Dotted(_)), "{imported:?}");
    assert_eq!(imported.value(), b"App.Shared.Money");
    assert_eq!(source(CODE, &imported), "App.Shared.Money");
}

#[test]
fn dotted_name_with_a_space_is_a_parse_error() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Store.sharp", "namespace App. Tenant;\n");

    assert!(!program.errors.is_empty());
}

fn class_members<'arena>(program: &'arena Program<'arena>) -> &'arena Sequence<'arena, ClassLikeMember<'arena>> {
    let Some(Statement::Class(class)) = program.statements.first() else {
        panic!("expected a class, got {:#?}", program.statements);
    };

    &class.members
}

fn method_body<'arena>(program: &'arena Program<'arena>) -> &'arena [Statement<'arena>] {
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let MethodBody::Concrete(body) = &method.body else {
        panic!("expected a method body, got {:#?}", method.body);
    };

    body.statements.as_slice()
}

fn expression<'arena>(statement: &'arena Statement<'arena>) -> &'arena Expression<'arena> {
    match statement {
        Statement::Expression(statement) => statement.expression,
        Statement::Return(Return { value: Some(value), .. }) => value,
        _ => panic!("expected an expression, got {statement:#?}"),
    }
}

fn bare_name<'arena>(expression: &'arena Expression<'arena>) -> &'arena [u8] {
    let Expression::ConstantAccess(access) = expression else {
        panic!("expected a bare name, got {expression:#?}");
    };

    access.name.value()
}

#[test]
fn method_puts_its_return_type_first_without_a_function_keyword() {
    const CODE: &str =
        "class Report\n{\n    public static int total(int extra)\n    {\n        return extra;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(method.function, None);
    assert_eq!(method.name.value, b"total");
    assert!(method.is_static());
    let return_type = method.return_type_hint.as_ref().expect("a return type");
    assert_eq!(return_type.colon, None);
    assert_eq!(source(CODE, return_type), "int");
    assert_eq!(source(CODE, method), &CODE[19..CODE.len() - 3]);

    let parameter = method.parameter_list.parameters.first().expect("one parameter");
    assert_eq!(parameter.variable.name, b"extra");
    assert_eq!(source(CODE, &parameter.variable), "extra");
    assert_eq!(source(CODE, parameter), "int extra");
}

#[test]
fn void_method_without_modifiers_starts_at_its_return_type() {
    const CODE: &str = "class Report\n{\n    void run() {}\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, method), "void run() {}");
}

#[test]
fn a_method_with_a_long_return_type_parses() {
    const CODE: &str = "class Report\n{\n    public static int|string|null total() { return 1; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, method.return_type_hint.as_ref().expect("a return type")), "int|string|null");
}

#[test]
fn a_by_reference_parameter_is_a_parse_error() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", "class Report\n{\n    public void fill(int &count) {}\n}\n");

    assert!(!program.errors.is_empty());
}

#[test]
fn a_parameter_with_a_dnf_type_parses() {
    const CODE: &str = "class Report\n{\n    public void fill((A&B)|null both) {}\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let [both] = method.parameter_list.parameters.as_slice() else {
        panic!("expected one parameter, got {:#?}", method.parameter_list);
    };
    assert_eq!(source(CODE, both.hint.as_ref().expect("a type")), "(A&B)|null");
}

#[test]
fn property_parses_as_in_php() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", "class Report\n{\n    private int $count = 0;\n}\n");

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    assert!(matches!(class_members(program).first(), Some(ClassLikeMember::Property(_))));
}

#[test]
fn let_and_const_declare_locals_with_an_initializer() {
    const CODE: &str =
        "class Report\n{\n    void run()\n    {\n        let label = \"one\";\n        const base = 2;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::LocalDeclaration(label), Statement::LocalDeclaration(base)] = method_body(program) else {
        panic!("expected two local declarations, got {:#?}", method_body(program));
    };
    assert_eq!(label.keyword.value, b"let");
    assert!(!label.is_const());
    assert_eq!(label.name.value, b"label");
    assert!(matches!(label.value, Expression::Literal(Literal::String(_))));
    assert_eq!(source(CODE, label), "let label = \"one\";");
    assert_eq!(base.keyword.value, b"const");
    assert!(base.is_const());
    assert_eq!(base.name.value, b"base");
}

#[test]
fn dot_reads_as_member_access_whatever_the_name_before_it() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        calc.add(label, base);\n        Calc.make();\n        this.total;\n        total = total - 1;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [add, make, total, assignment] = method_body(program) else {
        panic!("expected four statements, got {:#?}", method_body(program));
    };

    let Expression::Call(Call::Method(add)) = expression(add) else {
        panic!("expected `calc.add(...)` to be a method call, got {add:#?}");
    };
    assert_eq!(bare_name(add.object), b"calc");
    assert_eq!(source(CODE, &add.arrow), ".");
    let arguments: Vec<_> = add.argument_list.arguments.iter().map(|argument| bare_name(argument.value())).collect();
    assert_eq!(arguments, [&b"label"[..], &b"base"[..]]);

    let Expression::Call(Call::Method(make)) = expression(make) else {
        panic!("expected `Calc.make()` to be a method call, got {make:#?}");
    };
    assert_eq!(bare_name(make.object), b"Calc");

    let Expression::Access(Access::Property(total)) = expression(total) else {
        panic!("expected `this.total` to be a property access, got {total:#?}");
    };
    assert_eq!(bare_name(total.object), b"this");

    let Expression::Assignment(assignment) = expression(assignment) else {
        panic!("expected an assignment, got {assignment:#?}");
    };
    assert_eq!(bare_name(assignment.lhs), b"total");
    assert!(matches!(assignment.rhs, Expression::Binary(Binary { operator: BinaryOperator::Subtraction(_), .. })));
}

#[test]
fn php_member_access_and_concatenating_assignment_are_parse_errors_that_name_the_dot() {
    for (code, message) in [
        ("calc->add()", "`->` is PHP syntax: PHP# writes member access with `.`"),
        ("calc?->add()", "`?->` is PHP syntax: PHP# writes member access with `.`"),
        ("Calc::make()", "`::` is PHP syntax: PHP# writes static access with `.`"),
        ("label .= \"x\"", "`.=` is PHP syntax: in PHP# `.` is member access"),
    ] {
        let source = format!("class Report\n{{\n    void run()\n    {{\n        {code};\n    }}\n}}\n");
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", Box::leak(source.into_boxed_str()));

        let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
        assert_eq!(messages, [message], "{code}");
    }
}

#[test]
fn a_method_written_with_function_is_a_parse_error() {
    let arena = LocalArena::new();
    let program =
        parse(&arena, "src/Report.sharp", "class Report\n{\n    public function run(): int { return 1; }\n}\n");

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages.first().map(String::as_str),
        Some("`function` is PHP syntax: a PHP# method starts with its return type")
    );
}

#[test]
fn dot_keeps_concatenating_in_php() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php $a . $b;");

    let Some(Statement::Expression(statement)) = program.statements.get(1) else {
        panic!("expected an expression statement, got {:#?}", program.statements);
    };
    assert!(matches!(statement.expression, Expression::Binary(binary) if binary.operator.is_concatenation()));
}

#[test]
fn php_file_starts_in_inline_text() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Demo/Report.php", "namespace Demo;\n");

    assert_eq!(program.dialect, Dialect::Php);
    assert!(matches!(program.statements.first(), Some(Statement::Inline(_))), "{:#?}", program.statements);
}
