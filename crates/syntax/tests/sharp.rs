#![allow(clippy::panic, clippy::expect_used, clippy::single_char_lifetime_names)]

use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::*;
use mago_syntax::dialect::Dialect;
use mago_syntax::error::ParseError;
use mago_syntax::parser::parse_file;
use mago_syntax::parser::parse_file_with_dialect;
use mago_syntax::settings::ParserSettings;

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

#[test]
fn a_caller_parses_a_file_in_the_dialect_it_names_whatever_the_file_name() {
    let arena = LocalArena::new();
    let sharp = File::ephemeral(Cow::Borrowed(b"Report.php"), Cow::Borrowed(b"namespace App.Tenant;\n"));
    let php = File::ephemeral(Cow::Borrowed(b"Report.sharp"), Cow::Borrowed(b"<?php namespace App\\Tenant;\n"));

    let sharp_program = parse_file_with_dialect(&arena, &sharp, Dialect::Sharp, ParserSettings::default());
    let php_program = parse_file_with_dialect(&arena, &php, Dialect::Php, ParserSettings::default());

    assert!(sharp_program.errors.is_empty(), "{:#?}", sharp_program.errors);
    assert_eq!(sharp_program.dialect, Dialect::Sharp);
    assert!(php_program.errors.is_empty(), "{:#?}", php_program.errors);
    assert_eq!(php_program.dialect, Dialect::Php);
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
fn a_constructor_is_a_method_named_after_its_class_without_a_return_type() {
    const CODE: &str = "class Report\n{\n    public Report(private int count, int extra) {}\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(constructor)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(constructor.function, None);
    assert_eq!(constructor.return_type_hint, None);
    assert_eq!(source(CODE, &constructor.name), "Report");
    assert_eq!(source(CODE, constructor), "public Report(private int count, int extra) {}");
    let [count, extra] = constructor.parameter_list.parameters.as_slice() else {
        panic!("expected two parameters, got {:#?}", constructor.parameter_list);
    };
    assert_eq!(source(CODE, count), "private int count");
    assert_eq!(source(CODE, extra), "int extra");
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

/// `required`, `via`, a named constructor and a computed property are spec syntax outside the slice. Each one is a
/// single error where it starts, and the class around it still parses.
#[test]
fn spec_syntax_outside_the_slice_is_one_not_supported_error_where_it_starts() {
    for (member, message, start) in [
        ("public required int count { get; set; }", "`required` is not supported yet in PHP#.", "required"),
        ("public string name { get; set; } via Trimmed, Tracked;", "`via` is not supported yet in PHP#.", "via"),
        (
            "public Report(public int id { get; } via Tracked, int other) {}",
            "`via` is not supported yet in PHP#.",
            "via",
        ),
        (
            "public Report.fromJson(string json) : this(json.length) {}",
            "A named constructor is not supported yet in PHP#.",
            "Report.fromJson",
        ),
        (
            "public Report make() { return new Report.fromJson(\"{}\"); }",
            "A named constructor is not supported yet in PHP#.",
            "Report.fromJson",
        ),
        ("public string slug => this.name;", "A computed property is not supported yet in PHP#.", "=>"),
    ] {
        let arena = LocalArena::new();
        let code: &'static str = Box::leak(
            format!("class Report\n{{\n    {member}\n\n    public int run() {{ return 1; }}\n}}\n").into_boxed_str(),
        );
        let program = parse(&arena, "src/Report.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{member}`, got {:#?}", program.errors);
        };
        assert_eq!(error.to_string(), message, "{member}");
        assert_eq!(source(code, error), start, "{member}");
        let Some(Statement::Class(class)) = program.statements.first() else {
            panic!("expected a class for `{member}`, got {:#?}", program.statements);
        };
        assert!(
            class
                .members
                .iter()
                .any(|member| matches!(member, ClassLikeMember::Method(run) if run.name.value == b"run")),
            "the class keeps parsing after `{member}`"
        );
    }
}

/// A PHP#-only parse error is its own message, so `mago analyze` shows the rule as the issue's title.
#[test]
fn a_sharp_parse_error_shows_its_message_as_the_issue_title() {
    for (code, message) in [
        (
            "class Report\n{\n    public int run() { return this->total; }\n}\n",
            "`->` is PHP syntax: PHP# writes member access with `.`",
        ),
        (
            "class Report\n{\n    public int run() { return \\Lib\\Calc.make(); }\n}\n",
            "A `\\` name is PHP syntax: add `import Lib.Calc;` and write `Calc`",
        ),
        (
            "class Report\n{\n    public int run(extra) { return 1; }\n}\n",
            "A PHP# parameter needs a type, as in `int extra`.",
        ),
        (
            "class Report\n{\n    public required int count { get; set; }\n}\n",
            "`required` is not supported yet in PHP#.",
        ),
        (
            "class Report\n{\n    public void run() { for (const line in lines) {} }\n}\n",
            "PHP# loops over a collection with `of`, as in `for (const line of lines)`.",
        ),
    ] {
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{code}`, got {:#?}", program.errors);
        };
        assert_eq!(Issue::from(error).message, message, "{code}");
    }
}

#[test]
fn a_parameter_without_a_type_is_a_parse_error_that_names_the_rule() {
    for parameters in ["extra", "extra, int other", "extra = 1"] {
        let arena = LocalArena::new();
        let code: &'static str = Box::leak(
            format!("class Report\n{{\n    public int run({parameters}) {{ return 1; }}\n}}\n").into_boxed_str(),
        );
        let program = parse(&arena, "src/Report.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{parameters}`, got {:#?}", program.errors);
        };
        assert_eq!(error.to_string(), "A PHP# parameter needs a type, as in `int extra`.", "{parameters}");
        assert_eq!(source(code, error), "extra", "{parameters}");
    }
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
fn a_question_mark_after_a_type_makes_it_nullable() {
    const CODE: &str = "class Report\n{\n    public Calc? find(int? id, string? label = null) { return null; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let hints = std::iter::once(&method.return_type_hint.as_ref().expect("a return type").hint)
        .chain(method.parameter_list.parameters.iter().map(|parameter| parameter.hint.as_ref().expect("a type")));
    let nullable: Vec<(&str, &str, &str)> = hints
        .map(|hint| {
            let Hint::Nullable(nullable) = hint else {
                panic!("expected a nullable type, got {hint:#?}");
            };

            (source(CODE, hint), source(CODE, nullable.hint), source(CODE, &nullable.question_mark))
        })
        .collect();

    assert_eq!(nullable, [("Calc?", "Calc", "?"), ("int?", "int", "?"), ("string?", "string", "?")]);
}

#[test]
fn a_question_mark_before_a_type_is_a_php_syntax_error_that_names_the_suffix() {
    const CODE: &str = "class Report\n{\n    public ?int find(?Calc calc) { return null; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "`?` before a type is PHP syntax: PHP# writes it after the type, as in `int?`",
            "`?` before a type is PHP syntax: PHP# writes it after the type, as in `int?`",
        ]
    );
    let spans: Vec<&str> = program.errors.iter().map(|error| source(CODE, error)).collect();
    assert_eq!(spans, ["?", "?"]);
}

#[test]
fn a_dollar_property_is_a_php_syntax_error() {
    const CODE: &str = "class Report\n{\n    private int $count = 0;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert_eq!(program.errors.len(), 1, "{:#?}", program.errors);
    let Some(ParseError::PhpSyntaxInSharp(_, span)) = program.errors.first() else {
        panic!("expected a PHP-syntax error, got {:#?}", program.errors);
    };
    assert_eq!(source(CODE, span), "$count");
}

#[test]
fn var_is_a_php_syntax_error() {
    const CODE: &str = "class Report\n{\n    var int count;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert_eq!(program.errors.len(), 1, "{:#?}", program.errors);
    let Some(ParseError::PhpSyntaxInSharp(_, span)) = program.errors.first() else {
        panic!("expected a PHP-syntax error, got {:#?}", program.errors);
    };
    assert_eq!(source(CODE, span), "var");
}

#[test]
fn a_field_keeps_its_type_and_bare_name() {
    const CODE: &str = "class Report\n{\n    private int count = 0;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Property(Property::Plain(field))) = class_members(program).first() else {
        panic!("expected a field, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, field.hint.as_ref().expect("a type")), "int");
    let item = field.items.first().expect("one field");
    assert_eq!(source(CODE, item.variable()), "count");
    assert_eq!(source(CODE, field), "private int count = 0;");
}

#[test]
fn an_auto_property_takes_its_initial_value_after_its_accessors() {
    const CODE: &str =
        "class Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [ClassLikeMember::Property(Property::Hooked(views)), ClassLikeMember::Property(Property::Hooked(name))] =
        class_members(program).as_slice()
    else {
        panic!("expected two properties, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, views), "public int views { get; private set; } = 0;");
    assert_eq!(source(CODE, &views.hook_list), "{ get; private set; }");
    let initial_value = views.initial_value.as_ref().expect("an initial value");
    assert_eq!(source(CODE, initial_value.value), "0");
    assert_eq!(source(CODE, name), "public string name { get; }");
    assert_eq!(name.initial_value, None);
}

#[test]
fn a_php_hooked_property_has_no_trailing_initial_value() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php class Report { public int $views { get; } = 0; }");

    assert!(!program.errors.is_empty());
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
    assert_eq!(label.keyword.expect("a keyword").value, b"let");
    assert!(label.hint.is_none());
    assert!(!label.is_const());
    assert_eq!(label.name.value, b"label");
    assert!(matches!(label.value, Expression::Literal(Literal::String(_))));
    assert_eq!(source(CODE, label), "let label = \"one\";");
    assert_eq!(base.keyword.expect("a keyword").value, b"const");
    assert!(base.is_const());
    assert_eq!(base.name.value, b"base");
}

#[test]
fn a_local_can_be_declared_with_its_type_written() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        Calc? found = null;\n        int total = 1;\n        const int base = 2;\n        const Calc? none = null;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let locals: Vec<(&str, &str, bool, &str)> = method_body(program)
        .iter()
        .map(|statement| {
            let Statement::LocalDeclaration(local) = statement else {
                panic!("expected a local declaration, got {statement:#?}");
            };

            (
                source(CODE, local),
                source(CODE, local.hint.as_ref().expect("a type")),
                local.is_const(),
                source(CODE, &local.name),
            )
        })
        .collect();

    assert_eq!(
        locals,
        [
            ("Calc? found = null;", "Calc?", false, "found"),
            ("int total = 1;", "int", false, "total"),
            ("const int base = 2;", "int", true, "base"),
            ("const Calc? none = null;", "Calc?", true, "none"),
        ]
    );
}

#[test]
fn a_spaced_question_mark_after_a_name_is_not_a_typed_local() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        found ? total = 1 : 2;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [statement] = method_body(program) else {
        panic!("expected one statement, got {:#?}", method_body(program));
    };
    assert!(matches!(expression(statement), Expression::Conditional(_)), "{statement:#?}");
}

#[test]
fn a_for_loop_declares_its_counter_with_let_or_const() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        for (let i = 0; i < 3; i++) {\n        }\n        for (const j = 0; ; ) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::For(counted), Statement::For(endless)] = method_body(program) else {
        panic!("expected two for loops, got {:#?}", method_body(program));
    };

    let declaration = counted.declaration.as_ref().expect("a declaration");
    assert_eq!(declaration.name.value, b"i");
    assert!(!declaration.is_const());
    assert_eq!(source(CODE, declaration), "let i = 0;");
    assert_eq!(source(CODE, &counted.initializations_semicolon), ";");
    assert!(counted.initializations.is_empty());
    assert_eq!(counted.conditions.len(), 1);
    assert_eq!(counted.increments.len(), 1);
    assert_eq!(source(CODE, counted), "for (let i = 0; i < 3; i++) {\n        }");

    assert!(endless.declaration.as_ref().is_some_and(LocalDeclaration::is_const));
    assert!(endless.conditions.is_empty());
}

#[test]
fn a_for_loop_declares_its_counter_with_its_type_written() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        for (int i = 0; i < 3; i++) {\n        }\n        for (const int? j = null; ; ) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::For(counted), Statement::For(endless)] = method_body(program) else {
        panic!("expected two for loops, got {:#?}", method_body(program));
    };

    let declaration = counted.declaration.as_ref().expect("a declaration");
    assert_eq!(source(CODE, declaration.hint.expect("a type")), "int");
    assert!(!declaration.is_const());
    assert_eq!(source(CODE, declaration), "int i = 0;");
    assert_eq!(counted.conditions.len(), 1);

    let declaration = endless.declaration.as_ref().expect("a declaration");
    assert_eq!(source(CODE, declaration.hint.expect("a type")), "int?");
    assert!(declaration.is_const());
}

#[test]
fn for_of_declares_its_loop_variable_or_key_and_value() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        for (const line of lines) {\n        }\n        for (let [key, plan] of this.plans()) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::ForOf(values), Statement::ForOf(entries)] = method_body(program) else {
        panic!("expected two for … of loops, got {:#?}", method_body(program));
    };

    assert!(values.is_const());
    let ForOfTarget::Value(line) = &values.target else {
        panic!("expected one loop variable, got {:#?}", values.target);
    };
    assert_eq!(line.value, b"line");
    assert_eq!(bare_name(values.expression), b"lines");
    assert_eq!(source(CODE, &values.of), "of");
    assert_eq!(source(CODE, values), "for (const line of lines) {\n        }");

    assert!(!entries.is_const());
    let ForOfTarget::KeyValue(pair) = &entries.target else {
        panic!("expected a key and a value, got {:#?}", entries.target);
    };
    assert_eq!((pair.key.value, pair.value.value), (&b"key"[..], &b"plan"[..]));
    assert_eq!(source(CODE, &entries.target), "[key, plan]");
    assert!(matches!(entries.expression, Expression::Call(Call::Method(_))));
}

#[test]
fn for_in_is_a_parse_error_that_names_of() {
    let arena = LocalArena::new();
    let program = parse(
        &arena,
        "src/Report.sharp",
        "class Report\n{\n    void run()\n    {\n        for (const line in lines) {\n        }\n    }\n}\n",
    );

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(messages, ["PHP# loops over a collection with `of`, as in `for (const line of lines)`."]);
}

#[test]
fn foreach_is_a_php_syntax_error() {
    let arena = LocalArena::new();
    let program = parse(
        &arena,
        "src/Report.sharp",
        "class Report\n{\n    void run()\n    {\n        foreach (lines as line) {\n        }\n    }\n}\n",
    );

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(messages, ["`foreach` is PHP syntax: PHP# loops over a collection with `for … of`"]);
}

#[test]
fn a_php_for_loop_has_no_declaration() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php for ($i = 0; $i < 3; $i++) {}");

    let Some(Statement::For(r#for)) = program.statements.get(1) else {
        panic!("expected a for loop, got {:#?}", program.statements);
    };
    assert!(r#for.declaration.is_none());
    assert_eq!(r#for.initializations.len(), 1);
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
fn question_mark_dot_reads_as_null_safe_member_access() {
    const CODE: &str =
        "class Report\n{\n    void run()\n    {\n        calc?.add(1);\n        this?.total.next;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [add, next] = method_body(program) else {
        panic!("expected two statements, got {:#?}", method_body(program));
    };

    let Expression::Call(Call::NullSafeMethod(add)) = expression(add) else {
        panic!("expected `calc?.add(1)` to be a null-safe method call, got {add:#?}");
    };
    assert_eq!(bare_name(add.object), b"calc");
    assert_eq!(source(CODE, &add.question_mark_arrow), "?.");
    assert_eq!(source(CODE, add), "calc?.add(1)");

    let Expression::Access(Access::Property(next)) = expression(next) else {
        panic!("expected `this?.total.next` to read `next`, got {next:#?}");
    };
    let Expression::Access(Access::NullSafeProperty(total)) = next.object else {
        panic!("expected `this?.total` to be a null-safe property access, got {:#?}", next.object);
    };
    assert_eq!(bare_name(total.object), b"this");
    assert_eq!(source(CODE, total), "this?.total");
}

#[test]
fn a_question_mark_apart_from_the_dot_is_not_null_safe_access() {
    let arena = LocalArena::new();
    let program =
        parse(&arena, "src/Report.sharp", "class Report\n{\n    void run()\n    {\n        calc? .add(1);\n    }\n}\n");

    assert!(!program.errors.is_empty());
}

#[test]
fn php_member_access_and_concatenating_assignment_are_parse_errors_that_name_the_dot() {
    for (code, message) in [
        ("calc->add()", "`->` is PHP syntax: PHP# writes member access with `.`"),
        ("calc?->add()", "`?->` is PHP syntax: PHP# writes null-safe member access with `?.`"),
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
fn a_php_operator_is_reported_once_where_it_is_consumed() {
    let arena = LocalArena::new();
    let program = parse(
        &arena,
        "src/Report.sharp",
        "class Report\n{\n    void run()\n    {\n        a = b + a .= \"x\";\n    }\n}\n",
    );

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(messages, ["`.=` is PHP syntax: in PHP# `.` is member access"]);
}

#[test]
fn the_use_keyword_is_a_parse_error_in_every_form() {
    for code in [
        "use Calc;\n",
        "use Lib\\Calc as Adder;\n",
        "use Lib\\{Calc, Money};\n",
        "use function Lib\\{make, total};\n",
        "class Report\n{\n    use Shared;\n}\n",
        "class Report\n{\n    use Lib\\Shared { Lib\\Shared::run as start; }\n}\n",
        "class Report\n{\n    void run()\n    {\n        const total = function () use ($count) { return 1; };\n    }\n}\n",
    ] {
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", code);

        let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
        assert_eq!(messages, ["`use` is PHP syntax: PHP# imports a class with `import`"], "{code}");
    }
}

#[test]
fn a_qualified_name_is_a_parse_error_that_names_the_import() {
    for code in [
        "class Report\n{\n    int run()\n    {\n        return \\Lib\\Calc.make();\n    }\n}\n",
        "class Report\n{\n    int run()\n    {\n        return Lib\\Calc.make();\n    }\n}\n",
        "class Report\n{\n    void run(\\Lib\\Calc calc)\n    {\n    }\n}\n",
    ] {
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", code);

        let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
        assert_eq!(messages, ["A `\\` name is PHP syntax: add `import Lib.Calc;` and write `Calc`"], "{code}");
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
