use std::collections::HashSet;

use mago_bytes::BytesDisplay;
use mago_names::binding::Binding;
use mago_names::binding::BindingError;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_names::binding::php_method_name;
use mago_names::scope::php_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::BinaryOperator;
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
use mago_syntax::cst::Hint;
use mago_syntax::cst::HookedProperty;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Method;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::Namespace;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Node;
use mago_syntax::cst::Program;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyItem;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Terminator;
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

/// Checks a PHP# file against the slice: the only constructs a `.sharp` file may use, and the contract the
/// engine's lowering implements.
///
/// - At file level: `namespace`, `import` and `class`. A file has at most one namespace, named and written without
///   braces.
/// - A class: a name, fields and methods, with no attributes, modifiers, `extends` or `implements`.
/// - A field: `private` or `protected`, a type, one name and an optional initial value. The initial value is a
///   literal, a constant, or the operators below on them, as a parameter default is. A `public` field is an error,
///   because spec section 6 has no public fields.
/// - An auto-property: `public`, `protected` or `private`, a type, one name, the accessors `get;` and an optional
///   `set;` that may take an access modifier narrower than the property's, and an optional initial value after the
///   accessors, which is a field's. A property without `get`, an accessor declared twice, or a `set` access modifier
///   as wide as the property's is an error, as in C#. A write to a get-only property of `this` outside the
///   constructor is an error.
/// - A method: `public`, `protected` or `private`, an optional `static`, parameters, a return type and a body. Its name
///   does not start with `__`, which PHP reserves for magic methods, and is not its class's name, compared ignoring
///   case, which PHP# gives to the constructor.
/// - The constructor: a method named exactly after its class, without a return type and not `static`. A method
///   without a return type named otherwise is an error.
/// - A parameter: always a type, a name and an optional default, and neither variadic nor by reference. A default is
///   a literal, a constant, or the operators below on them, without `++` and `--`.
/// - Types: `int`, `float`, `bool`, `string` and a class written by its short name, and `void` as a return type.
///   PHP's own check reports a `void` parameter.
/// - In a method body: blocks, expression statements, `return`, and `let` and `const` declarations.
/// - Writes: `=`, compound assignment, `++` and `--` write only a local, a parameter or a member written
///   `object.name`.
/// - In expressions: literals, parentheses, bare names, assignment, the operators below, and method calls and
///   property reads written with `.` and a member name, and `new Class(...)` on a class written by its short name,
///   with positional and named arguments. A string literal's
///   `\u{...}` escapes are valid codepoints, as PHP requires.
/// - Operators: `+ - * / %`, `== != === !== < > <= >=`, `&& || !`, unary `-` and `+`, `++` and `--`, and
///   `= += -= *= /=`.
///
/// The check visits every node and refuses any node, or any position of a node, that this list does not name. It
/// reports each refusal once, at its outermost node. It does not run on a file with a parse error, which is the one
/// error to fix first. The constructs PHP# never has, such as `$` variables, `global` and top-level functions, keep
/// their own errors.
///
/// Two more refusals need inferred types, so the analyzer makes them as its part of this contract:
/// - `+` with an operand that may be a string, in `analyze_arithmetic_operation`.
/// - an instance method used as a value, such as `order.total` without a call, in `report_non_existent_property`.
#[inline]
pub fn check_slice(program: &Program, context: &mut Context<'_, '_, '_>) {
    check_node(Node::Program(program), Place::File, &mut HashSet::new(), context);
}

/// Where a node of a PHP# file sits, for the parts of the slice that depend on it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The file's statements, its namespace and its imports.
    File,
    /// A class and its members.
    Class,
    /// A field or a property, both of which the CST calls a property: its modifiers, type and name.
    FieldOrProperty,
    /// A method's modifiers, parameters and return type.
    Method,
    /// One parameter.
    Parameter,
    /// A method body.
    Body,
    /// A parameter default.
    Default,
}

/// How code uses a member access, which decides how the slice reports it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberUse {
    Call,
    Read,
    Write,
}

/// Walks `node` at `place`. `checked` holds the spans of the member accesses already checked, so the walk checks
/// each access once.
fn check_node(node: Node<'_, '_>, place: Place, checked: &mut HashSet<Span>, context: &mut Context<'_, '_, '_>) {
    if let Some(place) = enter(node, place, checked, context) {
        for child in node.children() {
            check_node(child, place, checked, context);
        }
    }
}

/// Decides one node at its place. Returns the place of its children when the slice has the node, and `None` when
/// the node is reported.
fn enter(
    node: Node<'_, '_>,
    place: Place,
    checked: &mut HashSet<Span>,
    context: &mut Context<'_, '_, '_>,
) -> Option<Place> {
    use Place::Body;
    use Place::Class;
    use Place::Default;
    use Place::FieldOrProperty;
    use Place::File;
    use Place::Method;
    use Place::Parameter;

    if let Some(target) = write_target(node) {
        // PHP# never has `$` variables, and `check_variable` reports a write to one.
        if !matches!(target, Expression::Variable(_)) && !is_slice_target(target, context) {
            report_not_supported(
                target.span(),
                "write target",
                "PHP# writes to a local, a parameter or a member written `object.name`.",
                context,
            );

            return None;
        }

        if let Expression::Access(Access::Property(property)) = target {
            check_get_only_write(property, context);
            check_member_access(
                property.span(),
                property.object,
                &property.property,
                MemberUse::Write,
                checked,
                context,
            );
        }
    }

    match (node, place) {
        (Node::Statement(Statement::Namespace(namespace)), File) if !is_slice_namespace(namespace, context.program) => {
            report_not_supported(
                namespace.r#namespace.span,
                "namespace",
                "A PHP# file has at most one namespace, written `namespace App.Tenant;` before its imports.",
                context,
            );

            None
        }
        (Node::Keyword(_) | Node::LocalIdentifier(_) | Node::Identifier(Identifier::Local(_)), _) => Some(place),
        (Node::Terminator(Terminator::Semicolon(_)), _) => Some(place),

        (
            Node::Program(_)
            | Node::Statement(Statement::Namespace(_) | Statement::Use(_) | Statement::Class(_))
            | Node::Namespace(_)
            | Node::NamespaceBody(_)
            | Node::NamespaceImplicitBody(_)
            | Node::Block(_)
            | Node::Identifier(Identifier::Dotted(_))
            | Node::DottedIdentifier(_)
            | Node::Use(_)
            | Node::UseItems(UseItems::Sequence(_))
            | Node::UseItemSequence(_)
            | Node::UseItem(_),
            File,
        ) => Some(File),
        (Node::Class(_), File) => Some(Class),

        (Node::ClassLikeMember(ClassLikeMember::Method(_)), Class) => Some(Class),
        (Node::ClassLikeMember(ClassLikeMember::Property(property)), Class) => match is_slice_property(property) {
            Ok(()) => Some(FieldOrProperty),
            Err(issue) => {
                context.report(*issue);

                None
            }
        },
        (Node::HookedProperty(property), FieldOrProperty) => {
            check_accessors(property, context);

            Some(FieldOrProperty)
        }
        // `check_accessors` checked the accessors, and the initial value is a parameter default's expression.
        (Node::PropertyHookList(_), FieldOrProperty) => None,
        (Node::Expression(_), FieldOrProperty) => {
            check_node(node, Default, checked, context);

            None
        }
        (
            Node::Property(_)
            | Node::PlainProperty(_)
            | Node::PropertyItem(_)
            | Node::PropertyAbstractItem(_)
            | Node::PropertyConcreteItem(_)
            | Node::DirectVariable(_)
            | Node::Modifier(Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)),
            FieldOrProperty,
        ) => Some(FieldOrProperty),
        (Node::Hint(hint), FieldOrProperty) if is_slice_type(hint) && !matches!(hint, Hint::Void(_)) => Some(FieldOrProperty),
        (Node::Method(method), Class) => match is_slice_method(method, context.program) {
            Ok(()) => Some(Method),
            Err((message, help)) => {
                context.report(
                    Issue::error(message)
                        .with_annotation(Annotation::primary(method.name.span).with_message("Not supported yet."))
                        .with_note(help),
                );

                None
            }
        },

        (
            Node::Modifier(Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Static(_))
            | Node::FunctionLikeParameterList(_)
            | Node::FunctionLikeReturnTypeHint(_)
            | Node::MethodBody(_)
            // A method without a body is `abstract`, which the slice refuses, or a PHP error.
            | Node::MethodAbstractBody(_),
            Method,
        ) => Some(Method),
        (Node::FunctionLikeParameter(parameter), Method) => match is_slice_parameter(parameter) {
            Ok(()) => Some(Parameter),
            Err((span, message, help)) => {
                context.report(
                    Issue::error(message)
                        .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
                        .with_note(help),
                );

                None
            }
        },
        (Node::Hint(hint), Method | Parameter) if is_slice_type(hint) => Some(place),
        (Node::DirectVariable(_), Parameter) => Some(Parameter),
        (Node::FunctionLikeParameterDefaultValue(_), Parameter) => Some(Default),
        (Node::Block(_), Method | Body) => Some(Body),

        (
            Node::Statement(
                Statement::Block(_) | Statement::Expression(_) | Statement::Return(_) | Statement::LocalDeclaration(_),
            )
            | Node::ExpressionStatement(_)
            | Node::Return(_)
            | Node::LocalDeclaration(_),
            Body,
        ) => Some(Body),

        // PHP refuses the file at compile time, so the engine would too.
        (Node::LiteralString(string), Body | Default) if string.value.is_none() => {
            context.report(
                Issue::error("Invalid UTF-8 codepoint escape sequence.")
                    .with_annotation(Annotation::primary(string.span).with_message("Escape written here."))
                    .with_note("A `\\u{...}` escape holds hex digits for a codepoint up to `10FFFF`."),
            );

            None
        }
        (
            Node::Expression(
                Expression::Literal(_)
                | Expression::ConstantAccess(_)
                | Expression::Parenthesized(_)
                | Expression::Binary(_)
                | Expression::UnaryPrefix(_),
            )
            | Node::Literal(_)
            | Node::LiteralInteger(_)
            | Node::LiteralFloat(_)
            | Node::LiteralString(_)
            | Node::Parenthesized(_)
            | Node::Binary(_)
            | Node::UnaryPrefix(_),
            Body | Default,
        ) => Some(place),
        (Node::ConstantAccess(_), Body) => Some(Body),
        (Node::ConstantAccess(constant), Default) if context.names.binding(&constant.name) == Some(Binding::Constant) => {
            Some(Default)
        }
        (Node::BinaryOperator(operator), Body | Default) if is_slice_binary_operator(operator) => Some(place),
        (Node::UnaryPrefixOperator(operator), Body | Default) if is_slice_prefix_operator(operator, place) => {
            Some(place)
        }

        (
            Node::Expression(
                Expression::Assignment(_)
                | Expression::UnaryPostfix(_)
                | Expression::Call(Call::Method(_))
                | Expression::Access(Access::Property(_)),
            )
            | Node::Assignment(_)
            | Node::UnaryPostfix(_)
            | Node::UnaryPostfixOperator(UnaryPostfixOperator::PostIncrement(_) | UnaryPostfixOperator::PostDecrement(_))
            | Node::Call(Call::Method(_))
            | Node::Access(Access::Property(_))
            | Node::ClassLikeMemberSelector(ClassLikeMemberSelector::Identifier(_))
            | Node::ArgumentList(_)
            | Node::Argument(_)
            | Node::NamedArgument(_),
            Body,
        ) => Some(Body),
        (Node::Expression(Expression::Instantiation(instantiation)), Body)
            if matches!(instantiation.class, Expression::Identifier(Identifier::Local(_))) =>
        {
            if let Some(arguments) = &instantiation.argument_list {
                check_node(Node::ArgumentList(arguments), Body, checked, context);

                return None;
            }

            report_not_supported(
                instantiation.span(),
                "`new` without arguments",
                "PHP# creates an object with `new Class(arguments)`, parentheses included.",
                context,
            );

            None
        }
        (Node::AssignmentOperator(operator), Body) if is_slice_assignment_operator(operator) => Some(Body),
        (Node::PositionalArgument(argument), Body) if argument.ellipsis.is_none() => Some(Body),
        (Node::MethodCall(call), Body) => {
            check_member_access(call.span(), call.object, &call.method, MemberUse::Call, checked, context);

            Some(Body)
        }
        (Node::PropertyAccess(access), Body) => {
            check_member_access(access.span(), access.object, &access.property, MemberUse::Read, checked, context);

            Some(Body)
        }

        // PHP# never has `$` variables. A `$` variable inside a construct the walk refuses adds no second error.
        (Node::Expression(Expression::Variable(variable)), Body | Default) => {
            check_variable(variable, context);

            None
        }
        // PHP# never has top-level functions, `global`, `compact()`, `extract()` or a member called without `this.`:
        // `check_function`, `check_global` and `check_function_call` report them.
        (Node::Statement(Statement::Function(_)), File) | (Node::Statement(Statement::Global(_)), Body) => None,
        (Node::Expression(Expression::Call(Call::Function(function_call))), Body)
            if is_checked_function_call(function_call, context) =>
        {
            None
        }

        _ => {
            report_unsupported(node, place, context);

            None
        }
    }
}

/// Whether the slice has a method, with the refusal's message and note when it does not. Each rule keeps out a method
/// whose meaning a later slice gives it: PHP's magic methods, PHP#'s `private` default of spec section 5, and a static
/// constructor. The constructor of spec section 9 is the one method without a return type, named after its class.
fn is_slice_method(method: &Method, program: &Program) -> Result<(), (&'static str, &'static str)> {
    let class_name = enclosing_class(program, method.span()).map(|class| class.name.value);
    let refusal = if method.return_type_hint.is_none() && class_name != Some(method.name.value) {
        (
            "A PHP# method needs a return type: only the constructor, named after its class, has none.",
            "A method is written `public int total()`, and the constructor `public Report()`.",
        )
    } else if method.return_type_hint.is_none() && method.is_static() {
        ("A static constructor is not supported yet in PHP#.", "The main constructor of a PHP# class is not `static`.")
    } else if method.name.value.starts_with(b"__") {
        (
            "This method name is not supported yet in PHP#.",
            "PHP reserves method names that start with `__` for its magic methods.",
        )
    } else if !method
        .modifiers
        .iter()
        .any(|modifier| matches!(modifier, Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)))
    {
        (
            "A method without `public`, `protected` or `private` is not supported yet in PHP#.",
            "A PHP# member without an access modifier is `private`, while PHP makes it `public`.",
        )
    } else if method.return_type_hint.is_some()
        && class_name.is_some_and(|class_name| class_name.eq_ignore_ascii_case(method.name.value))
    {
        (
            "A method named after its class is not supported yet in PHP#.",
            "In PHP#, a method named after its class is the class's constructor.",
        )
    } else {
        return Ok(());
    };

    Err(refusal)
}

/// Whether the slice has a parameter, with the refusal's span, message and note when it does not.
fn is_slice_parameter(parameter: &FunctionLikeParameter) -> Result<(), (Span, &'static str, &'static str)> {
    if let Some(ellipsis) = parameter.ellipsis {
        Err((ellipsis, "This variadic parameter is not supported yet in PHP#.", supported(Place::Parameter)))
    } else if let Some(ampersand) = parameter.ampersand {
        Err((
            ampersand,
            "A by-reference parameter is not supported yet in PHP#.",
            "The engine passes every PHP# argument by value.",
        ))
    } else if parameter.hint.is_none() {
        Err((
            parameter.variable.span,
            "A parameter without a type is not supported in PHP#.",
            "A PHP# parameter is written with its type, as in `int extra`.",
        ))
    } else {
        Ok(())
    }
}

/// Whether the slice has a field or a property, with the refusal when it does not. Spec section 6 makes a field
/// storage, which is `private` or `protected`, and a property the API, whose accessors `check_accessors` decides.
fn is_slice_property(property: &Property) -> Result<(), Box<Issue>> {
    let not_supported = |span: Span, message: &str, note: &str| {
        Issue::error(message)
            .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
            .with_note(note)
    };
    let no_access_modifier = "A PHP# member without an access modifier is `private`, while PHP makes it `public`.";

    match property {
        Property::Plain(field) => {
            if let Some(public) = field.modifiers.get_public() {
                Err(Box::new(
                    Issue::error("A PHP# field cannot be `public`: a field is `private` or `protected`.")
                        .with_annotation(Annotation::primary(public.span()).with_message("Declared `public` here."))
                        .with_help("Declare a property, as in `public int views { get; set; }`, to make it public."),
                ))
            } else if !field.modifiers.contains_protected() && !field.modifiers.contains_private() {
                Err(Box::new(not_supported(
                    property.first_variable().span,
                    "A field without `private` or `protected` is not supported yet in PHP#.",
                    no_access_modifier,
                )))
            } else if field.items.len() > 1 {
                Err(Box::new(not_supported(
                    field.span(),
                    "A field declaring several names is not supported yet in PHP#.",
                    supported(Place::FieldOrProperty),
                )))
            } else {
                Ok(())
            }
        }
        Property::Hooked(auto_property) => {
            if !auto_property.modifiers.contains_visibility() {
                Err(Box::new(not_supported(
                    property.first_variable().span,
                    "A property without `public`, `protected` or `private` is not supported yet in PHP#.",
                    no_access_modifier,
                )))
            } else if let PropertyItem::Concrete(item) = &auto_property.item {
                Err(Box::new(not_supported(
                    item.span(),
                    "An initial value before the accessors is not supported yet in PHP#.",
                    "A PHP# property writes its initial value after its accessors: `public int views { get; set; } = 0;`.",
                )))
            } else {
                Ok(())
            }
        }
    }
}

/// Checks the accessors of an auto-property: `get;` once, and an optional `set;` once, which may take an access
/// modifier narrower than the property's, as in C#. Accessor bodies, `init` and an access modifier on `get` are not
/// supported yet.
fn check_accessors(property: &HookedProperty, context: &mut Context<'_, '_, '_>) {
    let reach = property.modifiers.get_first_read_visibility().map_or(0, visibility_reach);
    let mut declared: Vec<&[u8]> = Vec::new();

    for accessor in &property.hook_list.hooks {
        let name = accessor.name.value;
        let is_auto = accessor.attribute_lists.is_empty()
            && accessor.ampersand.is_none()
            && accessor.parameter_list.is_none()
            && matches!(accessor.body, PropertyHookBody::Abstract(_))
            && accessor.modifiers.len() <= 1
            && accessor.modifiers.iter().all(|modifier| {
                matches!(modifier, Modifier::Protected(_) | Modifier::Private(_) | Modifier::Public(_))
            });

        if !is_auto || !(name == b"get" || name == b"set") || (name == b"get" && !accessor.modifiers.is_empty()) {
            report_not_supported(
                accessor.span(),
                "accessor",
                "A PHP# property's accessors are `get;` and `set;`, and `set` may take `private` or `protected`.",
                context,
            );
        } else if declared.contains(&name) {
            context.report(
                Issue::error("A PHP# property declares each accessor once.")
                    .with_annotation(Annotation::primary(accessor.name.span).with_message("Declared again here.")),
            );
        } else if accessor.modifiers.first().is_some_and(|modifier| visibility_reach(modifier) >= reach) {
            context.report(
                Issue::error("The `set` accessor of a PHP# property must be narrower than the property.")
                    .with_annotation(Annotation::primary(accessor.span()).with_message("Declared here."))
                    .with_help("Write `private set;` or `protected set;` narrower than the property, or `set;`."),
            );
        }

        declared.push(name);
    }

    if !declared.contains(&b"get".as_slice()) {
        context.report(
            Issue::error("A PHP# property needs a `get` accessor.")
                .with_annotation(Annotation::primary(property.item.variable().span).with_message("Declared here."))
                .with_help("Add `get;`, as in `public int views { get; set; }`."),
        );
    }
}

/// Reports a write to a get-only property of `this` outside the constructor. Spec section 6.1 sets a get-only
/// property only in the constructor, and the engine's `private(set)` alone would allow any method of the class.
fn check_get_only_write(access: &PropertyAccess, context: &mut Context<'_, '_, '_>) {
    let (Expression::ConstantAccess(object), ClassLikeMemberSelector::Identifier(name)) =
        (access.object, &access.property)
    else {
        return;
    };

    let Some(class) = enclosing_class(context.program, access.span()) else {
        return;
    };

    if context.names.binding(&object.name) != Some(Binding::This) {
        return;
    }

    let is_get_only = class.members.iter().any(|member| {
        matches!(member, ClassLikeMember::Property(Property::Hooked(property))
            if property.item.variable().name == name.value
                && !property.hook_list.hooks.iter().any(|accessor| accessor.name.value == b"set"))
    });
    let in_constructor = class.members.iter().any(|member| {
        matches!(member, ClassLikeMember::Method(method)
            if php_method_name(method) == b"__construct" && method.span().contains(&access.span().start))
    });

    if is_get_only && !in_constructor {
        context.report(
            Issue::error(format!(
                "Cannot write `{}` here: a get-only property is set only in the constructor.",
                BytesDisplay(name.value)
            ))
            .with_annotation(Annotation::primary(access.span()).with_message("Written here."))
            .with_help("Write it in the constructor, or add `private set;` to change it later."),
        );
    }
}

/// How far a visibility modifier reaches: `private` least, then `protected`, then `public`.
const fn visibility_reach(modifier: &Modifier) -> u8 {
    match modifier {
        Modifier::Public(_) | Modifier::PublicSet(_) => 2,
        Modifier::Protected(_) | Modifier::ProtectedSet(_) => 1,
        _ => 0,
    }
}

/// Whether the slice has a namespace: the file's first, with a name and no braces.
fn is_slice_namespace(namespace: &Namespace, program: &Program) -> bool {
    namespace.name.is_some()
        && matches!(namespace.body, NamespaceBody::Implicit(_))
        && first_namespace(program).is_some_and(|first| first.span() == namespace.span())
}

/// The first namespace of a PHP# file, the only one the slice has.
fn first_namespace<'ast, 'arena>(program: &'ast Program<'arena>) -> Option<&'ast Namespace<'arena>> {
    program.statements.iter().find_map(|statement| match statement {
        Statement::Namespace(namespace) => Some(namespace),
        _ => None,
    })
}

/// The expression a node writes: the left side of an assignment, or the operand of `++` or `--`.
fn write_target<'ast, 'arena>(node: Node<'ast, 'arena>) -> Option<&'ast Expression<'arena>> {
    match node {
        Node::Assignment(assignment) => Some(assignment.lhs),
        Node::UnaryPrefix(unary_prefix) if unary_prefix.operator.is_increment_or_decrement() => {
            Some(unary_prefix.operand)
        }
        Node::UnaryPostfix(unary_postfix) => Some(unary_postfix.operand),
        _ => None,
    }
}

/// Whether the slice can write an expression: a local, a parameter, or a member written `object.name`.
fn is_slice_target(target: &Expression, context: &Context<'_, '_, '_>) -> bool {
    match target {
        Expression::ConstantAccess(access) => matches!(context.names.binding(&access.name), Some(Binding::Local(_))),
        Expression::Access(Access::Property(_)) => true,
        _ => false,
    }
}

/// Whether the slice has a type: the built-in types of spec section 24, or a class written by its short name.
fn is_slice_type(hint: &Hint) -> bool {
    matches!(
        hint,
        Hint::Integer(_)
            | Hint::Float(_)
            | Hint::Bool(_)
            | Hint::String(_)
            | Hint::Void(_)
            | Hint::Identifier(Identifier::Local(_))
    )
}

/// Whether the slice has a binary operator. Every operator is named, so a new one does not compile until it is
/// decided.
const fn is_slice_binary_operator(operator: &BinaryOperator) -> bool {
    match operator {
        BinaryOperator::Addition(_)
        | BinaryOperator::Subtraction(_)
        | BinaryOperator::Multiplication(_)
        | BinaryOperator::Division(_)
        | BinaryOperator::Modulo(_)
        | BinaryOperator::Equal(_)
        | BinaryOperator::NotEqual(_)
        | BinaryOperator::Identical(_)
        | BinaryOperator::NotIdentical(_)
        | BinaryOperator::LessThan(_)
        | BinaryOperator::LessThanOrEqual(_)
        | BinaryOperator::GreaterThan(_)
        | BinaryOperator::GreaterThanOrEqual(_)
        | BinaryOperator::And(_)
        | BinaryOperator::Or(_) => true,
        BinaryOperator::Exponentiation(_)
        | BinaryOperator::BitwiseAnd(_)
        | BinaryOperator::BitwiseOr(_)
        | BinaryOperator::BitwiseXor(_)
        | BinaryOperator::LeftShift(_)
        | BinaryOperator::RightShift(_)
        | BinaryOperator::NullCoalesce(_)
        | BinaryOperator::AngledNotEqual(_)
        | BinaryOperator::Spaceship(_)
        | BinaryOperator::StringConcat(_)
        | BinaryOperator::Instanceof(_)
        | BinaryOperator::LowAnd(_)
        | BinaryOperator::LowOr(_)
        | BinaryOperator::LowXor(_) => false,
    }
}

/// Whether the slice has a prefix operator at a place. A parameter default has no `++` or `--`. Every operator is
/// named, so a new one does not compile until it is decided.
fn is_slice_prefix_operator(operator: &UnaryPrefixOperator, place: Place) -> bool {
    match operator {
        UnaryPrefixOperator::Negation(_) | UnaryPrefixOperator::Plus(_) | UnaryPrefixOperator::Not(_) => true,
        UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_) => place == Place::Body,
        UnaryPrefixOperator::ErrorControl(_)
        | UnaryPrefixOperator::Reference(_)
        | UnaryPrefixOperator::ArrayCast(..)
        | UnaryPrefixOperator::BoolCast(..)
        | UnaryPrefixOperator::BooleanCast(..)
        | UnaryPrefixOperator::DoubleCast(..)
        | UnaryPrefixOperator::RealCast(..)
        | UnaryPrefixOperator::FloatCast(..)
        | UnaryPrefixOperator::IntCast(..)
        | UnaryPrefixOperator::IntegerCast(..)
        | UnaryPrefixOperator::ObjectCast(..)
        | UnaryPrefixOperator::UnsetCast(..)
        | UnaryPrefixOperator::StringCast(..)
        | UnaryPrefixOperator::BinaryCast(..)
        | UnaryPrefixOperator::VoidCast(..)
        | UnaryPrefixOperator::BitwiseNot(_) => false,
    }
}

/// Whether the slice has an assignment operator. Every operator is named, so a new one does not compile until it
/// is decided.
const fn is_slice_assignment_operator(operator: &AssignmentOperator) -> bool {
    match operator {
        AssignmentOperator::Assign(_)
        | AssignmentOperator::Addition(_)
        | AssignmentOperator::Subtraction(_)
        | AssignmentOperator::Multiplication(_)
        | AssignmentOperator::Division(_) => true,
        AssignmentOperator::Modulo(_)
        | AssignmentOperator::Exponentiation(_)
        | AssignmentOperator::Concat(_)
        | AssignmentOperator::BitwiseAnd(_)
        | AssignmentOperator::BitwiseOr(_)
        | AssignmentOperator::BitwiseXor(_)
        | AssignmentOperator::LeftShift(_)
        | AssignmentOperator::RightShift(_)
        | AssignmentOperator::Coalesce(_) => false,
    }
}

fn report_unsupported(node: Node<'_, '_>, place: Place, context: &mut Context<'_, '_, '_>) {
    let construct = match node {
        Node::Statement(_) => "statement",
        Node::Expression(_) | Node::ConstantAccess(_) => "expression",
        Node::BinaryOperator(_) | Node::UnaryPrefixOperator(_) | Node::AssignmentOperator(_) => "operator",
        Node::Hint(_) => "type",
        Node::Modifier(_) => "modifier",
        Node::AttributeList(_) => {
            return report_not_supported(node.span(), "attribute", "PHP# writes attributes as `[...]`.", context);
        }
        Node::Extends(_) => "`extends` clause",
        Node::Implements(_) => "`implements` clause",
        Node::ClassLikeMember(_) => "class member",
        Node::ClassLikeMemberSelector(_) => "member name",
        Node::PositionalArgument(_) => "spread argument",
        _ => "construct",
    };

    report_not_supported(node.span(), construct, supported(place), context);
}

/// What the slice has at a place, as the note of a refusal there.
const fn supported(place: Place) -> &'static str {
    match place {
        Place::File => "At file level, PHP# supports `namespace`, `import` and `class`.",
        Place::Class => {
            "A PHP# class has a name, fields and methods, with no attributes, modifiers, `extends` or `implements`."
        }
        Place::FieldOrProperty => {
            "A PHP# field is `private` or `protected`, and a property has the accessors `get;` and an optional `set;`. Both have a type of `int`, `float`, `bool`, `string` or a class, a name, and an optional initial value."
        }
        Place::Method => {
            "A PHP# method takes `public`, `protected`, `private` and `static`, parameters, and a return type of `int`, `float`, `bool`, `string`, `void` or a class."
        }
        Place::Parameter => {
            "A PHP# parameter has a type of `int`, `float`, `bool`, `string` or a class, a name, and an optional default."
        }
        Place::Body => {
            "In a method body, PHP# supports blocks, expression statements, `return`, `let` and `const`, with literals, parentheses, bare names, assignment, arithmetic, comparison and logical operators, `++` and `--`, method calls and property reads written with `.`, and `new Class(...)`."
        }
        Place::Default => {
            "A parameter default is a literal, a constant, or arithmetic, comparison and logical operators on them."
        }
    }
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
///
/// A write after the local's block closed reports only its scope error.
fn check_const_write(target: &Expression, write: &str, context: &mut Context<'_, '_, '_>) {
    let Expression::ConstantAccess(target) = target else {
        return;
    };

    let span = target.name.span();
    let out_of_scope = context
        .names
        .binding_errors()
        .iter()
        .any(|error| matches!(error, BindingError::OutOfScope { name, .. } if *name == span));
    if out_of_scope {
        return;
    }

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
    let namespace = first_namespace(program).and_then(|namespace| namespace.name.as_ref()).map(php_name);

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
        report_bare_member(constant_access.span(), constant_access.name.value(), context);
    }
}

#[inline]
pub fn check_function_call(function_call: &FunctionCall, context: &mut Context<'_, '_, '_>) {
    let Expression::Identifier(identifier) = function_call.function else {
        return;
    };

    if context.names.binding(identifier) == Some(Binding::Member) {
        report_bare_member(identifier.span(), identifier.value(), context);

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

fn check_variable(variable: &Variable, context: &mut Context<'_, '_, '_>) {
    match variable {
        Variable::Direct(direct) => report_dollar_variable(direct.name, direct.span, context),
        Variable::Indirect(_) | Variable::Nested(_) => context.report(
            Issue::error("Variable variables are not part of PHP#.")
                .with_annotation(Annotation::primary(variable.span()).with_message("Used here."))
                .with_help("PHP# variables are never created or read by name at runtime."),
        ),
    }
}

/// Checks `object.member` and `object.member()` once per chain of property reads, from its outermost access. The
/// spans of the accesses it checks go into `checked`.
///
/// A chain rooted at a class reaches static members. Reading one without a call or writing one is not supported yet,
/// and a chain of capitalized names is a full name, which belongs in an `import` line.
fn check_member_access(
    access: Span,
    object: &Expression,
    member: &ClassLikeMemberSelector,
    member_use: MemberUse,
    checked: &mut HashSet<Span>,
    context: &mut Context<'_, '_, '_>,
) {
    if !checked.insert(access) {
        return;
    }

    let mut properties = Vec::new();
    let mut root = object;
    while let Expression::Access(Access::Property(property)) = root
        && let ClassLikeMemberSelector::Identifier(name) = &property.property
    {
        checked.insert(property.span());
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
        if member_use != MemberUse::Call
            && let ClassLikeMemberSelector::Identifier(member) = member
        {
            report_static_access(root, member, access, member_use, context);
        }

        return;
    };

    let is_full_name = !context.names.is_imported(&root.name)
        && !declares_class(context.program, root.name.value())
        && starts_uppercase(root.name.value())
        && properties.iter().all(|property| starts_uppercase(property.value));

    if !is_full_name {
        let read = Span::between(root.span(), first_property.span);
        report_static_access(root, first_property, read, MemberUse::Read, context);

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

/// The classes and imports a PHP# file declares at its top level and in its first namespace, in source order.
fn declarations<'ast, 'arena>(
    program: &'ast Program<'arena>,
) -> (Vec<&'ast Class<'arena>>, Vec<&'ast UseItem<'arena>>) {
    let mut classes = Vec::new();
    let mut imports = Vec::new();
    for statement in &program.statements {
        collect_declarations(statement, &mut classes, &mut imports);
    }

    // The slice refuses a second namespace, so its declarations are not the file's.
    for statement in first_namespace(program).into_iter().flat_map(|namespace| namespace.statements().iter()) {
        collect_declarations(statement, &mut classes, &mut imports);
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

fn report_static_access(
    root: &ConstantAccess,
    member: &LocalIdentifier,
    span: Span,
    member_use: MemberUse,
    context: &mut Context<'_, '_, '_>,
) {
    let class = BytesDisplay(root.name.value());
    let member = BytesDisplay(member.value);
    let issue = if member_use == MemberUse::Write {
        Issue::error(format!("Writing `{class}.{member}` is not supported yet."))
            .with_annotation(Annotation::primary(span).with_message("Written here."))
            .with_note("The engine does not run a static member write yet.")
    } else {
        Issue::error(format!("Reading `{class}.{member}` without a call is not supported yet."))
            .with_annotation(Annotation::primary(span).with_message("Read here."))
            .with_note("The engine does not run a static member read without a call yet.")
    };

    context.report(issue);
}

fn report_bare_member(span: Span, name: &[u8], context: &mut Context<'_, '_, '_>) {
    let (name, call) = match enclosing_class_method(context.program, span, name) {
        Some(method) => (BytesDisplay(method.name.value), "()"),
        None => (BytesDisplay(name), ""),
    };
    let message = match enclosing_static_method_class(context.program, span) {
        Some(class) => format!(
            "Write `{}.{name}{call}`: a static method reaches the members of its class through the class name.",
            BytesDisplay(class.name.value)
        ),
        None => format!("Write `this.{name}{call}`: members of the same object are always written with `this.`."),
    };

    context.report(Issue::error(message).with_annotation(Annotation::primary(span).with_message("Used here.")));
}

/// The class of the static method whose body holds `span`, which has no `this`.
fn enclosing_static_method_class<'ast, 'arena>(
    program: &'ast Program<'arena>,
    span: Span,
) -> Option<&'ast Class<'arena>> {
    let class = enclosing_class(program, span)?;

    class
        .members
        .iter()
        .any(|member| {
            matches!(member, ClassLikeMember::Method(method)
                if method.span().contains(&span.start)
                    && method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Static(_))))
        })
        .then_some(class)
}

/// The method named `name` of the class whose body holds `span`. Method names are case-insensitive.
fn enclosing_class_method<'ast, 'arena>(
    program: &'ast Program<'arena>,
    span: Span,
    name: &[u8],
) -> Option<&'ast Method<'arena>> {
    enclosing_class(program, span)?.members.iter().find_map(|member| match member {
        ClassLikeMember::Method(method) if method.name.value.eq_ignore_ascii_case(name) => Some(method),
        _ => None,
    })
}

/// The class of a PHP# file whose body holds `span`.
fn enclosing_class<'ast, 'arena>(program: &'ast Program<'arena>, span: Span) -> Option<&'ast Class<'arena>> {
    declarations(program).0.into_iter().find(|class| class.span().contains(&span.start))
}

fn starts_uppercase(name: &[u8]) -> bool {
    name.first().is_some_and(u8::is_ascii_uppercase)
}
