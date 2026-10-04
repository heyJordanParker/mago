use mago_bytes::BytesDisplay;
use mago_names::binding::Binding;
use mago_names::binding::BindingError;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_names::scope::php_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Global;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::PositionalArgument;
use mago_syntax::cst::Program;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItem;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::Variable;

use crate::internal::consts::RESERVED_CLASS_NAMES;
use crate::internal::consts::RESERVED_KEYWORDS;
use crate::internal::consts::SOFT_RESERVED_KEYWORDS_MINUS_SYMBOL_ALLOWED;
use crate::internal::context::Context;

/// The PHP superglobals. A PHP# local or parameter of one of these names would read or replace it.
const SUPERGLOBALS: [&[u8]; 9] =
    [b"GLOBALS", b"_SERVER", b"_GET", b"_POST", b"_FILES", b"_COOKIE", b"_SESSION", b"_REQUEST", b"_ENV"];

/// Checks a PHP# file against the slice allow-list: the only constructs a `.sharp` file may use, and the contract
/// the engine's lowering implements.
///
/// - At file level: `namespace`, `import` and `class`, with no attributes, modifiers, `extends` or `implements`.
/// - In a class: methods, with `public`, `protected`, `private` and `static` as their only modifiers.
/// - Method parameters: a type, a name and an optional default.
/// - In a method body: blocks, expression statements, `return`, and `let` and `const` declarations.
/// - In expressions: literals, parentheses, bare names, assignment, binary operators, prefix operators, postfix `++`
///   and `--`, and method calls and property access written with `.`, with positional and named arguments.
///
/// Every other construct is not supported yet, reported once at its outermost node. The constructs PHP# never has,
/// such as `$` variables, keep their own errors.
#[inline]
pub fn check_slice(program: &Program, context: &mut Context<'_, '_, '_>) {
    for statement in &program.statements {
        check_file_statement(statement, context);
    }
}

fn check_file_statement(statement: &Statement, context: &mut Context<'_, '_, '_>) {
    match statement {
        Statement::Namespace(namespace) => {
            for statement in namespace.statements() {
                check_file_statement(statement, context);
            }
        }
        Statement::Use(_) => {}
        Statement::Class(class) => check_class(class, context),
        // PHP# never has top-level functions: `check_function` reports them.
        Statement::Function(_) => {}
        _ => report_not_supported(
            statement.span(),
            "statement",
            "At file level, PHP# supports `namespace`, `import` and `class`.",
            context,
        ),
    }
}

fn check_class(class: &Class, context: &mut Context<'_, '_, '_>) {
    report_attributes(&class.attribute_lists, context);
    for modifier in &class.modifiers {
        report_not_supported(modifier.span(), "modifier", "A PHP# class takes no modifiers.", context);
    }

    if let Some(extends) = &class.extends {
        report_not_supported(extends.span(), "`extends` clause", "A PHP# class has no parent class.", context);
    }

    if let Some(implements) = &class.implements {
        report_not_supported(
            implements.span(),
            "`implements` clause",
            "A PHP# class implements no interfaces.",
            context,
        );
    }

    for member in &class.members {
        match member {
            ClassLikeMember::Method(method) => check_method(method, context),
            ClassLikeMember::Property(field) => context.report(
                Issue::error("PHP# fields are not supported yet.")
                    .with_annotation(Annotation::primary(field.span()).with_message("Not supported yet."))
                    .with_note("In a class, PHP# supports methods."),
            ),
            _ => report_not_supported(member.span(), "class member", "In a class, PHP# supports methods.", context),
        }
    }
}

fn check_method(method: &Method, context: &mut Context<'_, '_, '_>) {
    report_attributes(&method.attribute_lists, context);
    for modifier in &method.modifiers {
        if !matches!(
            modifier,
            Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Static(_)
        ) {
            report_not_supported(
                modifier.span(),
                "modifier",
                "On a method, PHP# supports `public`, `protected`, `private` and `static`.",
                context,
            );
        }
    }

    for parameter in &method.parameter_list.parameters {
        check_method_parameter(parameter, context);
    }

    if let MethodBody::Concrete(block) = &method.body {
        for statement in &block.statements {
            check_body_statement(statement, context);
        }
    }
}

fn check_method_parameter(parameter: &FunctionLikeParameter, context: &mut Context<'_, '_, '_>) {
    const SUPPORTED: &str = "PHP# supports parameters with a type, a name and an optional default.";

    report_attributes(&parameter.attribute_lists, context);
    if let Some(ellipsis) = parameter.ellipsis {
        report_not_supported(ellipsis, "variadic parameter", SUPPORTED, context);
    }

    if let Some(default_value) = &parameter.default_value {
        check_expression(default_value.value, context);
    }
}

fn report_attributes(attribute_lists: &Sequence<'_, AttributeList<'_>>, context: &mut Context<'_, '_, '_>) {
    for attribute_list in attribute_lists {
        report_not_supported(attribute_list.span(), "attribute", "PHP# writes attributes as `[...]`.", context);
    }
}

fn check_body_statement(statement: &Statement, context: &mut Context<'_, '_, '_>) {
    match statement {
        Statement::Block(block) => {
            for statement in &block.statements {
                check_body_statement(statement, context);
            }
        }
        Statement::Expression(statement) => check_expression(statement.expression, context),
        Statement::Return(r#return) => {
            if let Some(value) = r#return.value {
                check_expression(value, context);
            }
        }
        Statement::LocalDeclaration(local_declaration) => check_expression(local_declaration.value, context),
        // PHP# never has `global`: `check_global` reports it.
        Statement::Global(_) => {}
        _ => report_not_supported(
            statement.span(),
            "statement",
            "In a method, PHP# supports blocks, expression statements, `return`, `let` and `const`.",
            context,
        ),
    }
}

fn check_expression(expression: &Expression, context: &mut Context<'_, '_, '_>) {
    match expression {
        Expression::Literal(_) | Expression::ConstantAccess(_) => {}
        Expression::Parenthesized(parenthesized) => check_expression(parenthesized.expression, context),
        Expression::Assignment(assignment) => {
            check_expression(assignment.lhs, context);
            check_expression(assignment.rhs, context);
        }
        Expression::Binary(binary) => {
            check_expression(binary.lhs, context);
            check_expression(binary.rhs, context);
        }
        Expression::UnaryPrefix(unary_prefix) => check_expression(unary_prefix.operand, context),
        Expression::UnaryPostfix(unary_postfix) => check_expression(unary_postfix.operand, context),
        Expression::Call(Call::Method(method_call)) => {
            check_expression(method_call.object, context);
            for argument in &method_call.argument_list.arguments {
                match argument {
                    Argument::Positional(PositionalArgument { ellipsis: None, value }) => {
                        check_expression(value, context)
                    }
                    Argument::Named(named) => check_expression(named.value, context),
                    Argument::Positional(_) => report_not_supported(
                        argument.span(),
                        "spread argument",
                        "PHP# supports positional and named arguments.",
                        context,
                    ),
                }
            }
        }
        Expression::Access(Access::Property(property_access)) => check_expression(property_access.object, context),
        // PHP# never has `$` variables, `compact()`, `extract()` or a member called without `this.`:
        // `check_variable` and `check_function_call` report them.
        Expression::Variable(_) => {}
        Expression::Call(Call::Function(function_call)) if is_checked_function_call(function_call, context) => {}
        _ => report_unsupported_expression(expression.span(), context),
    }
}

fn report_unsupported_expression(span: Span, context: &mut Context<'_, '_, '_>) {
    report_not_supported(
        span,
        "expression",
        "PHP# supports literals, parentheses, bare names, assignment, binary operators, prefix operators, postfix `++` and `--`, and method calls and property access with `.`.",
        context,
    );
}

fn report_not_supported(span: Span, construct: &str, supported: &str, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!("This {construct} is not supported yet in PHP#."))
            .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
            .with_note(supported),
    );
}

#[inline]
pub fn check_assignment(assignment: &Assignment, context: &mut Context<'_, '_, '_>) {
    check_const_write(assignment.lhs, "assign to", context);
}

#[inline]
pub fn check_unary_prefix(unary_prefix: &UnaryPrefix, context: &mut Context<'_, '_, '_>) {
    match unary_prefix.operator {
        UnaryPrefixOperator::PreIncrement(_) => check_const_write(unary_prefix.operand, "increment", context),
        UnaryPrefixOperator::PreDecrement(_) => check_const_write(unary_prefix.operand, "decrement", context),
        _ => {}
    }
}

#[inline]
pub fn check_unary_postfix(unary_postfix: &UnaryPostfix, context: &mut Context<'_, '_, '_>) {
    match unary_postfix.operator {
        UnaryPostfixOperator::PostIncrement(_) => check_const_write(unary_postfix.operand, "increment", context),
        UnaryPostfixOperator::PostDecrement(_) => check_const_write(unary_postfix.operand, "decrement", context),
    }
}

/// Reports a write, such as `assign to` or `increment`, to a local declared with `const`.
fn check_const_write(target: &Expression, write: &str, context: &mut Context<'_, '_, '_>) {
    let Expression::ConstantAccess(target) = target else {
        return;
    };

    if let Some(Binding::Local(local @ Local { kind: LocalKind::Const, .. })) = context.names.binding(&target.name) {
        let name = BytesDisplay(target.name.value());

        context.report(
            Issue::error(format!("Cannot {write} `{name}`: it is declared with `const`."))
                .with_annotation(Annotation::primary(target.span()).with_message("Changed here."))
                .with_annotation(Annotation::secondary(local.declaration).with_message("Declared with `const` here."))
                .with_help(format!("Declare `{name}` with `let` to change it.")),
        );
    }
}

#[inline]
pub fn check_local_declaration(local_declaration: &LocalDeclaration, context: &mut Context<'_, '_, '_>) {
    check_local_name(local_declaration.name.value, local_declaration.name.span, "local", context);
}

#[inline]
pub fn check_parameter(parameter: &FunctionLikeParameter, context: &mut Context<'_, '_, '_>) {
    if parameter.variable.name.starts_with(b"$") {
        report_dollar_variable(parameter.variable.name, parameter.variable.span, context);
    } else {
        check_local_name(parameter.variable.name, parameter.variable.span, "parameter", context);
    }
}

/// Reports the PHP# scope rules the binder found broken.
#[inline]
pub fn check_binding_errors(context: &mut Context<'_, '_, '_>) {
    for error in context.names.binding_errors() {
        let issue = match *error {
            BindingError::OutOfScope { name, local } => {
                let name_text = BytesDisplay(context.get_code_snippet(name));

                Issue::error(format!("`{name_text}` is used after the block that declares it closes."))
                    .with_annotation(Annotation::primary(name).with_message("Used here."))
                    .with_annotation(Annotation::secondary(local.declaration).with_message("Declared here."))
                    .with_help(format!(
                        "A local lives until the `}}` that closes its block. Declare `{name_text}` before the block to use it after."
                    ))
            }
            BindingError::Redeclared { name, earlier } => {
                let name_text = BytesDisplay(context.get_code_snippet(name));

                Issue::error(format!("`{name_text}` is already declared in an enclosing block of this method."))
                    .with_annotation(Annotation::primary(name).with_message("Declared again here."))
                    .with_annotation(Annotation::secondary(earlier.declaration).with_message("First declared here."))
                    .with_help("Rename one of the two locals. A block cannot redeclare a name its enclosing blocks declare, as in C#'s rule CS0136.")
            }
        };

        context.report(issue);
    }
}

/// Checks a PHP# class name against the names the engine reserves, beyond the keywords the PHP checks reject.
#[inline]
pub fn check_class_name(class: &Class, context: &mut Context<'_, '_, '_>) {
    let is_keyword = RESERVED_KEYWORDS
        .iter()
        .chain(&SOFT_RESERVED_KEYWORDS_MINUS_SYMBOL_ALLOWED)
        .any(|keyword| keyword.eq_ignore_ascii_case(class.name.value));

    if is_reserved_class_name(class.name.value) && !is_keyword {
        let name = BytesDisplay(class.name.value);

        context.report(
            Issue::error(format!("Cannot use `{name}` as a class name: it is reserved."))
                .with_annotation(Annotation::primary(class.name.span).with_message("Class declared here."))
                .with_note("PHP reserves this name for a type."),
        );
    }
}

/// Checks the classes and imports of a PHP# file against each other, as the engine does when it compiles the file.
#[inline]
pub fn check_declarations(program: &Program, context: &mut Context<'_, '_, '_>) {
    let (classes, imports) = declarations(program);
    let namespace = program.statements.iter().find_map(|statement| match statement {
        Statement::Namespace(namespace) => namespace.name.as_ref().map(php_name),
        _ => None,
    });

    for (index, import) in imports.iter().enumerate() {
        let short_name = import.name.last_segment();
        let full_name = BytesDisplay(import.name.value());

        if is_reserved_class_name(short_name) {
            let short_name = BytesDisplay(short_name);

            context.report(
                Issue::error(format!(
                    "Cannot import `{full_name}` as `{short_name}`: PHP reserves `{short_name}` for a type."
                ))
                .with_annotation(Annotation::primary(import.name.span()).with_message("Imported here."))
                .with_help("Import a class with another name."),
            );
        }

        if let Some(earlier) =
            imports[..index].iter().find(|earlier| earlier.name.last_segment().eq_ignore_ascii_case(short_name))
        {
            let earlier_full_name = BytesDisplay(earlier.name.value());

            context.report(
                Issue::error(format!(
                    "Cannot import `{full_name}` as `{}`: `{earlier_full_name}` is already imported as `{}`.",
                    BytesDisplay(short_name),
                    BytesDisplay(earlier.name.last_segment()),
                ))
                .with_annotation(Annotation::primary(import.name.span()).with_message("Imported again here."))
                .with_annotation(Annotation::secondary(earlier.name.span()).with_message("First imported here."))
                .with_note("Class names are case-insensitive."),
            );
        }

        // Importing the class the file declares names that class, which the engine accepts.
        if let Some(class) = classes.iter().find(|class| class.name.value.eq_ignore_ascii_case(short_name))
            && !names_class(&php_name(&import.name), namespace.as_deref(), class.name.value)
        {
            let short_name = BytesDisplay(short_name);
            let class_name = BytesDisplay(class.name.value);

            context.report(
                Issue::error(format!(
                    "Cannot import `{full_name}` as `{short_name}`: this file declares a class named `{class_name}`."
                ))
                .with_annotation(Annotation::primary(import.name.span()).with_message("Imported here."))
                .with_annotation(Annotation::secondary(class.name.span).with_message("Class declared here.")),
            );
        }
    }
}

#[inline]
pub fn check_constant_access(constant_access: &ConstantAccess, context: &mut Context<'_, '_, '_>) {
    if context.names.binding(&constant_access.name) == Some(Binding::Member) {
        report_bare_member(constant_access.span(), BytesDisplay(constant_access.name.value()), context);
    }
}

#[inline]
pub fn check_function_call(function_call: &FunctionCall, context: &mut Context<'_, '_, '_>) {
    let Expression::Identifier(identifier) = function_call.function else {
        return;
    };

    if context.names.binding(identifier) == Some(Binding::Member) {
        report_bare_member(identifier.span(), BytesDisplay(identifier.value()), context);

        return;
    }

    let function = identifier.last_segment();
    if is_compact_or_extract(function) {
        let function = BytesDisplay(function);

        context.report(
            Issue::error(format!("`{function}()` is not part of PHP#."))
                .with_annotation(Annotation::primary(function_call.span()).with_message("Called here."))
                .with_help("PHP# variables are never created or read by name at runtime."),
        );
    }
}

/// Returns true when `check_function_call` reports the call: a bare member, `compact()` or `extract()`.
fn is_checked_function_call(function_call: &FunctionCall, context: &Context<'_, '_, '_>) -> bool {
    let Expression::Identifier(identifier) = function_call.function else {
        return false;
    };

    context.names.binding(identifier) == Some(Binding::Member) || is_compact_or_extract(identifier.last_segment())
}

fn is_compact_or_extract(function: &[u8]) -> bool {
    function.eq_ignore_ascii_case(b"compact") || function.eq_ignore_ascii_case(b"extract")
}

#[inline]
pub fn check_function(function: &Function, context: &mut Context<'_, '_, '_>) {
    let name = BytesDisplay(function.name.value);

    context.report(
        Issue::error(format!("PHP# has no top-level functions: move `{name}` into a class as a static method."))
            .with_annotation(Annotation::primary(function.name.span).with_message("Function declared here.")),
    );
}

#[inline]
pub fn check_global(global: &Global, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error("`global` is not part of PHP#.")
            .with_annotation(Annotation::primary(global.global.span).with_message("Used here."))
            .with_help("Pass the value in as a parameter instead."),
    );
}

#[inline]
pub fn check_variable(variable: &Variable, context: &mut Context<'_, '_, '_>) {
    match variable {
        Variable::Direct(direct) => report_dollar_variable(direct.name, direct.span, context),
        Variable::Indirect(_) | Variable::Nested(_) => context.report(
            Issue::error("Variable variables are not part of PHP#.")
                .with_annotation(Annotation::primary(variable.span()).with_message("Used here."))
                .with_help("PHP# variables are never created or read by name at runtime."),
        ),
    }
}

/// Checks `object.member` and `object.member()` once per chain of property reads, from its outermost access.
///
/// A chain rooted at a class reads static members. Reading one without a call is not supported yet, and a chain of
/// capitalized names is a full name, which belongs in an `import` line.
#[inline]
pub fn check_member_access(
    access: Span,
    object: &Expression,
    member: &ClassLikeMemberSelector,
    is_call: bool,
    context: &mut Context<'_, '_, '_>,
) {
    let is_outermost = !context.property_chain_objects.contains(&access);
    if let Expression::Access(Access::Property(_)) = object {
        context.property_chain_objects.insert(object.span());
    }

    if !is_outermost {
        return;
    }

    let mut properties = Vec::new();
    let mut root = object;
    while let Expression::Access(Access::Property(property)) = root
        && let ClassLikeMemberSelector::Identifier(name) = &property.property
    {
        properties.push(name);
        root = property.object;
    }

    let Expression::ConstantAccess(root) = root else {
        return;
    };

    if context.names.binding(&root.name) != Some(Binding::Class) {
        return;
    }

    properties.reverse();
    let Some(first_property) = properties.first() else {
        if !is_call && let ClassLikeMemberSelector::Identifier(member) = member {
            report_static_read(root, member, access, context);
        }

        return;
    };

    let is_full_name = !context.names.is_imported(&root.name)
        && !declares_class(context.program, root.name.value())
        && starts_uppercase(root.name.value())
        && properties.iter().all(|property| starts_uppercase(property.value));

    if !is_full_name {
        let read = Span::between(root.span(), first_property.span);
        report_static_read(root, first_property, read, context);

        return;
    }

    let mut full_name = root.name.value().to_vec();
    for property in &properties {
        full_name.push(b'.');
        full_name.extend_from_slice(property.value);
    }

    let last = properties.last().map_or(first_property.value, |property| property.value);
    let full_name = BytesDisplay(&full_name);
    let class = BytesDisplay(last);
    let span = Span::between(root.span(), properties.last().map_or(first_property.span, |property| property.span));

    context.report(
        Issue::error(format!(
            "Full names appear only in `import` lines: add `import {full_name};` and write `{class}`."
        ))
        .with_annotation(Annotation::primary(span).with_message("Full name used here."))
        .with_help(
            "In code, `.` is always member access, so a class is written by the short name its import brings in.",
        ),
    );
}

/// The classes and imports a PHP# file declares, in source order.
fn declarations<'ast, 'arena>(
    program: &'ast Program<'arena>,
) -> (Vec<&'ast Class<'arena>>, Vec<&'ast UseItem<'arena>>) {
    let mut classes = Vec::new();
    let mut imports = Vec::new();
    for statement in &program.statements {
        collect_declarations(statement, &mut classes, &mut imports);
        if let Statement::Namespace(namespace) = statement {
            for statement in namespace.statements() {
                collect_declarations(statement, &mut classes, &mut imports);
            }
        }
    }

    (classes, imports)
}

fn declares_class(program: &Program, name: &[u8]) -> bool {
    declarations(program).0.iter().any(|class| class.name.value.eq_ignore_ascii_case(name))
}

fn collect_declarations<'ast, 'arena>(
    statement: &'ast Statement<'arena>,
    classes: &mut Vec<&'ast Class<'arena>>,
    imports: &mut Vec<&'ast UseItem<'arena>>,
) {
    match statement {
        Statement::Class(class) => classes.push(class),
        Statement::Use(Use { items: UseItems::Sequence(sequence), .. }) => imports.extend(sequence.items.iter()),
        _ => {}
    }
}

/// Checks the name of a PHP# local or parameter, which runs as the PHP variable of the same name.
fn check_local_name(name: &[u8], span: Span, kind: &str, context: &mut Context<'_, '_, '_>) {
    if name == b"this" {
        context.report(
            Issue::error(format!("Cannot name a {kind} `this`: `this` is the object the method runs on."))
                .with_annotation(Annotation::primary(span).with_message("Declared here."))
                .with_note(format!("A PHP# {kind} runs as a PHP variable of the same name, and PHP forbids `$this`.")),
        );
    } else if SUPERGLOBALS.contains(&name) {
        let name = BytesDisplay(name);

        context.report(
            Issue::error(format!("`{name}` is the name of a PHP superglobal: rename this {kind}."))
                .with_annotation(Annotation::primary(span).with_message("Declared here."))
                .with_note(format!("A PHP# {kind} runs as a PHP variable of the same name, which would be `${name}`.")),
        );
    }
}

fn is_reserved_class_name(name: &[u8]) -> bool {
    RESERVED_CLASS_NAMES.iter().any(|reserved| reserved.eq_ignore_ascii_case(name))
}

/// Returns true when the PHP name `full_name` is the class `class_name` declared in `namespace`.
fn names_class(full_name: &[u8], namespace: Option<&[u8]>, class_name: &[u8]) -> bool {
    let (import_namespace, import_class) = match full_name.iter().rposition(|&byte| byte == b'\\') {
        Some(separator) => (&full_name[..separator], &full_name[separator + 1..]),
        None => (&full_name[..0], full_name),
    };

    import_class.eq_ignore_ascii_case(class_name)
        && import_namespace.eq_ignore_ascii_case(namespace.unwrap_or_default())
}

fn report_dollar_variable(name: &[u8], span: Span, context: &mut Context<'_, '_, '_>) {
    let bare = BytesDisplay(name.strip_prefix(b"$").unwrap_or(name));

    context.report(
        Issue::error(format!("PHP# variables have no `$`: write `{bare}`."))
            .with_annotation(Annotation::primary(span).with_message("Written with `$` here.")),
    );
}

fn report_static_read(root: &ConstantAccess, member: &LocalIdentifier, span: Span, context: &mut Context<'_, '_, '_>) {
    let class = BytesDisplay(root.name.value());
    let member = BytesDisplay(member.value);

    context.report(
        Issue::error(format!("Reading `{class}.{member}` without a call is not supported yet."))
            .with_annotation(Annotation::primary(span).with_message("Read here."))
            .with_note("The engine does not run a static member read without a call yet."),
    );
}

fn report_bare_member(span: Span, name: BytesDisplay<'_>, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!("Write `this.{name}`: members of the same object are always written with `this.`."))
            .with_annotation(Annotation::primary(span).with_message("Used here.")),
    );
}

fn starts_uppercase(name: &[u8]) -> bool {
    name.first().is_some_and(u8::is_ascii_uppercase)
}
