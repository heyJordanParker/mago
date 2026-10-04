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
use mago_syntax::cst::Assignment;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Global;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Program;
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
    if function.eq_ignore_ascii_case(b"compact") || function.eq_ignore_ascii_case(b"extract") {
        let function = BytesDisplay(function);

        context.report(
            Issue::error(format!("`{function}()` is not part of PHP#."))
                .with_annotation(Annotation::primary(function_call.span()).with_message("Called here."))
                .with_help("PHP# variables are never created or read by name at runtime."),
        );
    }
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
            .with_note("What reading a static member without a call means at runtime is still undecided."),
    );
}

fn report_bare_member(span: Span, name: BytesDisplay<'_>, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!("Using the member `{name}` without `this.` is not supported yet."))
            .with_annotation(Annotation::primary(span).with_message("Used here."))
            .with_help(format!("Write `this.{name}`."))
            .with_note("What a bare member name means at runtime is still undecided."),
    );
}

fn starts_uppercase(name: &[u8]) -> bool {
    name.first().is_some_and(u8::is_ascii_uppercase)
}
