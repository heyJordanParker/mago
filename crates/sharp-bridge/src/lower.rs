use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::php_method_name;
use mago_names::resolver::NameResolver;
use mago_names::scope::php_name;
use mago_php_version::PHPVersion;
use mago_reporting::Level;
use mago_semantics::SemanticsChecker;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::ForOfTarget;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Hint;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Interface;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::NamedArgument;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::PartialArgumentList;
use mago_syntax::cst::PositionalArgument;
use mago_syntax::cst::Program;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::WhileBody;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file_with_dialect;
use mago_syntax::settings::ParserSettings;
use mago_syntax_core::utils::parse_literal_integer_as_float;

use crate::Diagnostic;
use crate::Unit;
use crate::sharp_kind;
use crate::sharp_kind::SHARP_AST_AND;
use crate::sharp_kind::SHARP_AST_ARG_LIST;
use crate::sharp_kind::SHARP_AST_ASSIGN;
use crate::sharp_kind::SHARP_AST_ASSIGN_COALESCE;
use crate::sharp_kind::SHARP_AST_ASSIGN_OP;
use crate::sharp_kind::SHARP_AST_ATTRIBUTE;
use crate::sharp_kind::SHARP_AST_ATTRIBUTE_GROUP;
use crate::sharp_kind::SHARP_AST_ATTRIBUTE_LIST;
use crate::sharp_kind::SHARP_AST_BINARY_OP;
use crate::sharp_kind::SHARP_AST_BREAK;
use crate::sharp_kind::SHARP_AST_CLASS;
use crate::sharp_kind::SHARP_AST_CLASS_CONST;
use crate::sharp_kind::SHARP_AST_CLASS_CONST_DECL;
use crate::sharp_kind::SHARP_AST_CLASS_CONST_GROUP;
use crate::sharp_kind::SHARP_AST_CLASS_NAME;
use crate::sharp_kind::SHARP_AST_COALESCE;
use crate::sharp_kind::SHARP_AST_CONST;
use crate::sharp_kind::SHARP_AST_CONST_DECL;
use crate::sharp_kind::SHARP_AST_CONST_ELEM;
use crate::sharp_kind::SHARP_AST_CONTINUE;
use crate::sharp_kind::SHARP_AST_DECLARE;
use crate::sharp_kind::SHARP_AST_DO_WHILE;
use crate::sharp_kind::SHARP_AST_EXPR_LIST;
use crate::sharp_kind::SHARP_AST_FOR;
use crate::sharp_kind::SHARP_AST_FOREACH;
use crate::sharp_kind::SHARP_AST_GREATER;
use crate::sharp_kind::SHARP_AST_GREATER_EQUAL;
use crate::sharp_kind::SHARP_AST_IF;
use crate::sharp_kind::SHARP_AST_IF_ELEM;
use crate::sharp_kind::SHARP_AST_METHOD;
use crate::sharp_kind::SHARP_AST_METHOD_CALL;
use crate::sharp_kind::SHARP_AST_NAMED_ARG;
use crate::sharp_kind::SHARP_AST_NAMESPACE;
use crate::sharp_kind::SHARP_AST_NEW;
use crate::sharp_kind::SHARP_AST_NULLSAFE_METHOD_CALL;
use crate::sharp_kind::SHARP_AST_NULLSAFE_PROP;
use crate::sharp_kind::SHARP_AST_OR;
use crate::sharp_kind::SHARP_AST_PARAM;
use crate::sharp_kind::SHARP_AST_PARAM_LIST;
use crate::sharp_kind::SHARP_AST_POST_DEC;
use crate::sharp_kind::SHARP_AST_POST_INC;
use crate::sharp_kind::SHARP_AST_PRE_DEC;
use crate::sharp_kind::SHARP_AST_PRE_INC;
use crate::sharp_kind::SHARP_AST_PROP;
use crate::sharp_kind::SHARP_AST_PROP_DECL;
use crate::sharp_kind::SHARP_AST_PROP_ELEM;
use crate::sharp_kind::SHARP_AST_PROP_GROUP;
use crate::sharp_kind::SHARP_AST_RETURN;
use crate::sharp_kind::SHARP_AST_STATIC_CALL;
use crate::sharp_kind::SHARP_AST_STATIC_PROP;
use crate::sharp_kind::SHARP_AST_STMT_LIST;
use crate::sharp_kind::SHARP_AST_UNARY_MINUS;
use crate::sharp_kind::SHARP_AST_UNARY_OP;
use crate::sharp_kind::SHARP_AST_UNARY_PLUS;
use crate::sharp_kind::SHARP_AST_VAR;
use crate::sharp_kind::SHARP_AST_WHILE;
use crate::sharp_kind::SHARP_AST_ZVAL;
use crate::sharp_node;
use crate::sharp_severity;
use crate::sharp_str;
use crate::sharp_value;
use crate::store_text;

/// The values php-src gives the attrs the lowering emits, from `zend_compile.h` and `zend_vm_opcodes.h`.
const ZEND_NAME_FQ: u32 = 0;
const ZEND_NAME_NOT_FQ: u32 = 1;
const ZEND_ACC_PUBLIC: u32 = 1 << 0;
const ZEND_ACC_PROTECTED: u32 = 1 << 1;
const ZEND_ACC_PRIVATE: u32 = 1 << 2;
const ZEND_ACC_STATIC: u32 = 1 << 4;
const ZEND_ACC_FINAL: u32 = 1 << 5;
const ZEND_ACC_ABSTRACT: u32 = 1 << 6;
const ZEND_ACC_EXPLICIT_ABSTRACT_CLASS: u32 = 1 << 6;
const ZEND_ACC_INTERFACE: u32 = 1 << 0;
const ZEND_ACC_READONLY: u32 = 1 << 7;
const ZEND_ACC_PROTECTED_SET: u32 = 1 << 11;
const ZEND_ACC_PRIVATE_SET: u32 = 1 << 12;
const ZEND_TYPE_NULLABLE: u32 = 1 << 8;
const ZEND_ADD: u32 = 1;
const ZEND_SUB: u32 = 2;
const ZEND_MUL: u32 = 3;
const ZEND_DIV: u32 = 4;
const ZEND_MOD: u32 = 5;
const ZEND_BOOL_NOT: u32 = 14;
const ZEND_IS_IDENTICAL: u32 = 16;
const ZEND_IS_NOT_IDENTICAL: u32 = 17;
const ZEND_IS_EQUAL: u32 = 18;
const ZEND_IS_NOT_EQUAL: u32 = 19;
const ZEND_IS_SMALLER: u32 = 20;
const ZEND_IS_SMALLER_OR_EQUAL: u32 = 21;
/// php-sharp's own attr from `zend_compile.h`: a class constant fetch that falls back to the static property.
const ZEND_FETCH_CLASS_MEMBER: u32 = 1 << 0;

/// A null child.
const NULL: u32 = u32::MAX;

/// Runs the PHP# file at `path` through the parser, the binder and the semantic checks, and lowers it into the tree
/// php-src builds for the equivalent PHP. Any error returns diagnostics and no nodes. The file is PHP# whatever its
/// name, because `ext/sharp` decided that before calling.
pub(crate) fn lower(path: Vec<u8>, source: Vec<u8>) -> Box<Unit> {
    let file = File::ephemeral(Cow::Owned(path), Cow::Owned(source));
    let lines = Lines::new(&file.contents);
    let arena = LocalArena::new();
    let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
    if !program.errors.is_empty() {
        return Unit::failed(
            program
                .errors
                .iter()
                .map(|error| lines.diagnostic(Some(error.span()), sharp_severity::SHARP_PARSE_ERROR, error.to_string()))
                .collect(),
        );
    }

    let names = NameResolver::new(&arena).resolve(program);
    let errors: Vec<Diagnostic> = SemanticsChecker::new(PHPVersion::PHP85)
        .check(&file, program, &names)
        .iter()
        .filter(|issue| issue.level == Level::Error)
        .map(|issue| lines.diagnostic(issue.primary_span(), sharp_severity::SHARP_COMPILE_ERROR, issue.message.clone()))
        .collect();
    if !errors.is_empty() {
        return Unit::failed(errors);
    }

    Lowering::new(&lines, &names).program(program)
}

/// The offset each line starts at, counted as the Zend scanner counts: `\n`, `\r\n` and a lone `\r` each end a line.
struct Lines(Vec<u32>);

impl Lines {
    fn new(source: &[u8]) -> Self {
        let mut starts = vec![0];
        for (index, &byte) in source.iter().enumerate() {
            if byte == b'\n' || (byte == b'\r' && source.get(index + 1) != Some(&b'\n')) {
                starts.push(index as u32 + 1);
            }
        }

        Self(starts)
    }

    /// The 1-based line `offset` is on.
    fn line(&self, offset: u32) -> u32 {
        self.0.partition_point(|&start| start <= offset) as u32
    }

    /// The file's last line, the one after its last line ending.
    fn last(&self) -> u32 {
        self.0.len() as u32
    }

    /// A diagnostic at the start of `span`, with a 1-based line and byte column. Without a span it is at line 0,
    /// column 0, which the ABI defines as no position.
    fn diagnostic(&self, span: Option<Span>, severity: sharp_severity, message: String) -> Diagnostic {
        let (line, column) = span.map_or((0, 0), |span| {
            let line = self.line(span.start.offset);

            (line, span.start.offset - self.0[line as usize - 1] + 1)
        });

        Diagnostic { line, column, severity, message }
    }
}

/// Lowers one checked file. Every node is pushed after its children, and each node's children are contiguous.
struct Lowering<'lowering, 'arena> {
    lines: &'lowering Lines,
    names: &'lowering ResolvedNames<'arena>,
    nodes: Vec<sharp_node>,
    children: Vec<u32>,
    texts: Vec<Box<[u8]>>,
}

impl<'lowering, 'arena> Lowering<'lowering, 'arena> {
    fn new(lines: &'lowering Lines, names: &'lowering ResolvedNames<'arena>) -> Self {
        Self { lines, names, nodes: Vec::new(), children: Vec::new(), texts: Vec::new() }
    }

    /// `declare(strict_types=1);` first, then the namespaces and classes. Imports are not lowered: every class name
    /// in the tree is fully qualified.
    fn program(mut self, program: &Program) -> Box<Unit> {
        let mut statements = vec![self.strict_types()];
        for statement in &program.statements {
            self.file_statement(statement, &mut statements);
        }

        let root = self.node(SHARP_AST_STMT_LIST, 0, 1, &statements);
        self.nodes[root as usize].end_line = self.lines.last();

        Unit::boxed(self.nodes, self.children, root, Vec::new(), self.texts)
    }

    fn strict_types(&mut self) -> u32 {
        let name = self.string(0, 1, b"strict_types");
        let value = self.zval(1, sharp_value::SHARP_LONG, |node| node.long_value = 1);
        let element = self.node(SHARP_AST_CONST_ELEM, 0, 1, &[name, value, NULL]);
        let list = self.node(SHARP_AST_CONST_DECL, 0, 1, &[element]);

        self.node(SHARP_AST_DECLARE, 0, 1, &[list, NULL])
    }

    fn file_statement(&mut self, statement: &Statement, statements: &mut Vec<u32>) {
        match statement {
            Statement::Namespace(namespace) => {
                let (Some(name), NamespaceBody::Implicit(body)) = (&namespace.name, &namespace.body) else {
                    unreachable!("check_slice refuses a namespace without a name or with braces");
                };

                let name = self.string(0, self.line(name), &php_name(name));
                statements.push(self.node(SHARP_AST_NAMESPACE, 0, self.line(namespace), &[name, NULL]));
                for statement in &body.statements {
                    self.file_statement(statement, statements);
                }
            }
            Statement::Use(_) => {}
            Statement::Class(class) => statements.push(self.class(class)),
            Statement::Interface(interface) => statements.push(self.interface(interface)),
            _ => unreachable!("check_slice refuses the file statement `{statement}`"),
        }
    }

    /// A class sets the initial values PHP takes as no default, those that are not constant or belong to a `readonly`
    /// property, at the start of its constructor, in declaration order. A class without a constructor gets a public
    /// one that spans the class.
    fn class(&mut self, class: &Class) -> u32 {
        let mut initial_values = Vec::new();
        for member in &class.members {
            if let ClassLikeMember::Property(property) = member
                && let Some(value) = property.initial_value()
                && !is_default(property, value)
            {
                initial_values.push(self.initial_value(property.first_variable(), value));
            }
        }

        let mut members = Vec::new();
        let mut has_constructor = false;
        for member in &class.members {
            members.push(match member {
                ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => {
                    has_constructor = true;

                    self.method(method, modifier_flags(&method.modifiers), &initial_values)
                }
                ClassLikeMember::Method(method) => self.method(method, modifier_flags(&method.modifiers), &[]),
                ClassLikeMember::Property(property) => self.property(property),
                ClassLikeMember::Constant(constant) => self.constant(constant),
                _ => unreachable!("check_slice refuses the class member `{member}`"),
            });
        }

        if !has_constructor && !initial_values.is_empty() {
            let line = self.line(class.left_brace);
            let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
            let body = self.node(SHARP_AST_STMT_LIST, 0, line, &initial_values);

            members.push(self.declaration(
                SHARP_AST_METHOD,
                ZEND_ACC_PUBLIC,
                class.class.span,
                class.right_brace,
                b"__construct",
                &[parameters, NULL, body, NULL, NULL],
            ));
        }

        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(class.left_brace), &members);
        let attributes = self.attributes(&class.attribute_lists);

        self.declaration(
            SHARP_AST_CLASS,
            class_flags(&class.modifiers),
            class.class.span,
            class.right_brace,
            class.name.value,
            &[NULL, NULL, members, attributes, NULL],
        )
    }

    /// An interface is a class declaration with `ZEND_ACC_INTERFACE`, and its methods are `public`, as php-src's
    /// grammar builds `interface Measured { public function area(): float; }`.
    fn interface(&mut self, interface: &Interface) -> u32 {
        let mut members = Vec::new();
        for member in &interface.members {
            let ClassLikeMember::Method(method) = member else {
                unreachable!("check_slice refuses the interface member `{member}`");
            };

            members.push(self.method(method, ZEND_ACC_PUBLIC, &[]));
        }

        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(interface.left_brace), &members);

        self.declaration(
            SHARP_AST_CLASS,
            ZEND_ACC_INTERFACE,
            interface.interface.span,
            interface.right_brace,
            interface.name.value,
            &[NULL, NULL, members, NULL, NULL],
        )
    }

    /// A method is a `function` with its return type after its parameters, and with `flags`. Its first line is where
    /// PHP writes `function`: the return type, or the name of the constructor, which runs as `__construct` and has
    /// no return type. The constructor's body starts with the class's initial values that are not constant, and an
    /// abstract method has no statement list.
    fn method(&mut self, method: &Method, flags: u32, initial_values: &[u32]) -> u32 {
        if flags & (ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED | ZEND_ACC_PRIVATE) == 0 {
            unreachable!("check_slice refuses a method without an access modifier");
        }

        let mut parameters = Vec::new();
        for parameter in &method.parameter_list.parameters {
            parameters.push(self.parameter(parameter));
        }

        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, self.line(&method.parameter_list), &parameters);
        let body = match &method.body {
            MethodBody::Concrete(body) => {
                let mut statements = initial_values.to_vec();
                for statement in &body.statements {
                    statements.push(self.statement(statement));
                }

                self.node(SHARP_AST_STMT_LIST, 0, self.line(body), &statements)
            }
            MethodBody::Abstract(_) => NULL,
        };
        let (start, return_type) = match &method.return_type_hint {
            Some(return_type_hint) => (return_type_hint.span(), self.hint(&return_type_hint.hint)),
            None => (method.name.span, NULL),
        };
        let attributes = self.attributes(&method.attribute_lists);

        self.declaration(
            SHARP_AST_METHOD,
            flags,
            start,
            method.body.span(),
            php_method_name(method),
            &[parameters, NULL, body, return_type, attributes],
        )
    }

    fn parameter(&mut self, parameter: &FunctionLikeParameter) -> u32 {
        let Some(hint) = &parameter.hint else {
            unreachable!("semantics refuses a parameter without a type");
        };
        let hint = self.hint(hint);
        let name = self.string(0, self.line(parameter.variable.span), parameter.variable.name);
        let default = parameter.default_value.as_ref().map_or(NULL, |default| self.expression(default.value));
        let accessor_flags =
            parameter.hooks.as_ref().map_or(0, |accessors| accessor_flags(&parameter.modifiers, accessors));
        let flags = modifier_flags(&parameter.modifiers) | accessor_flags;
        let attributes = self.attributes(&parameter.attribute_lists);

        self.node(SHARP_AST_PARAM, flags, self.line(parameter), &[hint, name, default, attributes, NULL, NULL])
    }

    /// A field or an auto-property is a property group of one property, as php-src's grammar builds
    /// `private int $count = 0;`, `public private(set) int $views = 0;` and `public readonly int $id;`. A constant
    /// initial value is its default, unless the property is `readonly`.
    fn property(&mut self, property: &Property) -> u32 {
        let (accessor_flags, attribute_lists) = match property {
            Property::Plain(field) => (0, &field.attribute_lists),
            Property::Hooked(auto_property) => {
                (accessor_flags(&auto_property.modifiers, &auto_property.hook_list), &auto_property.attribute_lists)
            }
        };
        let Some(hint) = property.hint() else {
            unreachable!("the PHP# parser gives every field and property its type");
        };
        let hint = self.hint(hint);
        let variable = property.first_variable();
        let line = self.line(variable);
        let name = self.string(0, line, variable.name);
        let default = match property.initial_value() {
            Some(value) if is_default(property, value) => self.expression(value),
            _ => NULL,
        };
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, default, NULL, NULL]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);
        let flags = modifier_flags(property.modifiers()) | accessor_flags;
        let attributes = self.attributes(attribute_lists);

        self.node(SHARP_AST_PROP_GROUP, flags, self.line(property), &[hint, declaration, attributes])
    }

    /// A declaration's attributes are one `ATTRIBUTE_LIST` with an `ATTRIBUTE_GROUP` per `[...]`, as php-src's grammar
    /// builds `#[...]`, or null without attributes. Each attribute names its class by its full name.
    fn attributes(&mut self, lists: &Sequence<AttributeList>) -> u32 {
        let Some(first) = lists.first() else {
            return NULL;
        };

        let mut groups = Vec::new();
        for list in lists {
            let mut attributes = Vec::new();
            for attribute in &list.attributes {
                let line = self.line(attribute.name);
                let name = self.string(ZEND_NAME_FQ, line, self.names.get(&attribute.name));
                let arguments = attribute.argument_list.as_ref().map_or(NULL, |list| self.attribute_arguments(list));

                attributes.push(self.node(SHARP_AST_ATTRIBUTE, 0, line, &[name, arguments]));
            }

            groups.push(self.node(SHARP_AST_ATTRIBUTE_GROUP, 0, self.line(list), &attributes));
        }

        self.node(SHARP_AST_ATTRIBUTE_LIST, 0, self.line(first), &groups)
    }

    /// A constant is a class constant group of one constant, as php-src's grammar builds `public const int MAX = 3;`:
    /// the constant list, no attributes, then the type.
    fn constant(&mut self, constant: &ClassLikeConstant) -> u32 {
        let item = constant.first_item();
        let line = self.line(item.name);
        let name = self.string(0, line, item.name.value);
        let value = self.expression(item.value);
        let element = self.node(SHARP_AST_CONST_ELEM, 0, line, &[name, value, NULL]);
        let declaration = self.node(SHARP_AST_CLASS_CONST_DECL, 0, line, &[element]);
        let hint = constant.hint.as_ref().map_or(NULL, |hint| self.hint(hint));

        self.node(
            SHARP_AST_CLASS_CONST_GROUP,
            modifier_flags(&constant.modifiers),
            self.line(constant),
            &[declaration, NULL, hint],
        )
    }

    /// `$this->name = value;` on the line of the member's name.
    fn initial_value(&mut self, variable: &DirectVariable, value: &Expression) -> u32 {
        let line = self.line(variable);
        let this = self.variable(variable.span, b"this");
        let name = self.string(0, line, variable.name);
        let property = self.node(SHARP_AST_PROP, 0, line, &[this, name]);
        let value = self.expression(value);

        self.node(SHARP_AST_ASSIGN, 0, line, &[property, value])
    }

    /// A built-in type is written unqualified, and a class by its full name. A nullable type is its type with
    /// `ZEND_TYPE_NULLABLE`, as php-src's grammar builds `?int`.
    fn hint(&mut self, hint: &Hint) -> u32 {
        match hint {
            Hint::Integer(name) | Hint::Float(name) | Hint::Bool(name) | Hint::String(name) | Hint::Void(name) => {
                self.string(ZEND_NAME_NOT_FQ, self.line(name.span), name.value)
            }
            Hint::Identifier(class) => self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class)),
            Hint::Nullable(nullable) => {
                let index = self.hint(nullable.hint);
                self.nodes[index as usize].attr |= ZEND_TYPE_NULLABLE;

                index
            }
            _ => unreachable!("check_slice refuses the type `{hint}`"),
        }
    }

    fn block(&mut self, block: &Block) -> u32 {
        let mut statements = Vec::new();
        for statement in &block.statements {
            statements.push(self.statement(statement));
        }

        self.node(SHARP_AST_STMT_LIST, 0, self.line(block), &statements)
    }

    fn statement(&mut self, statement: &Statement) -> u32 {
        match statement {
            Statement::Block(block) => self.block(block),
            Statement::Expression(statement) => self.expression(statement.expression),
            Statement::Return(r#return) => {
                let value = r#return.value.map_or(NULL, |value| self.expression(value));

                self.node(SHARP_AST_RETURN, 0, self.line(r#return), &[value])
            }
            Statement::LocalDeclaration(local) => self.local(local),
            Statement::If(r#if) => self.r#if(r#if),
            Statement::For(r#for) => self.r#for(r#for),
            Statement::ForOf(for_of) => {
                let collection = self.expression(for_of.expression);
                let (key, value) = match &for_of.target {
                    ForOfTarget::Value(value) => (NULL, self.variable(value.span, value.value)),
                    ForOfTarget::KeyValue(pair) => {
                        (self.variable(pair.key.span, pair.key.value), self.variable(pair.value.span, pair.value.value))
                    }
                };
                let body = self.statement(for_of.body);

                self.node(SHARP_AST_FOREACH, 0, self.line(for_of), &[collection, value, key, body])
            }
            Statement::While(r#while) => {
                let WhileBody::Statement(body) = &r#while.body else {
                    unreachable!("check_slice refuses a colon-delimited `while`");
                };
                let condition = self.expression(r#while.condition);
                let body = self.statement(body);

                self.node(SHARP_AST_WHILE, 0, self.line(r#while), &[condition, body])
            }
            Statement::DoWhile(do_while) => {
                let body = self.statement(do_while.statement);
                let condition = self.expression(do_while.condition);

                self.node(SHARP_AST_DO_WHILE, 0, self.line(do_while), &[body, condition])
            }
            Statement::Break(r#break) => self.node(SHARP_AST_BREAK, 0, self.line(r#break), &[NULL]),
            Statement::Continue(r#continue) => self.node(SHARP_AST_CONTINUE, 0, self.line(r#continue), &[NULL]),
            _ => unreachable!("check_slice refuses the statement `{statement}`"),
        }
    }

    /// A `let` or `const` local is the assignment of its value to its variable.
    fn local(&mut self, local: &LocalDeclaration) -> u32 {
        let variable = self.variable(local.name.span, local.name.value);
        let value = self.expression(local.value);

        self.node(SHARP_AST_ASSIGN, 0, self.line(local), &[variable, value])
    }

    /// Each part of the header is an `EXPR_LIST`, or null when it is empty. A `let` or `const` counter is the
    /// first part.
    fn r#for(&mut self, r#for: &For) -> u32 {
        let ForBody::Statement(body) = &r#for.body else {
            unreachable!("check_slice refuses a colon-delimited `for`");
        };

        let mut initializations = Vec::new();
        if let Some(declaration) = &r#for.declaration {
            initializations.push(self.local(declaration));
        }
        for initialization in &r#for.initializations {
            initializations.push(self.expression(initialization));
        }

        let initializations = self.expression_list(&initializations);
        let conditions: Vec<u32> = r#for.conditions.iter().map(|condition| self.expression(condition)).collect();
        let conditions = self.expression_list(&conditions);
        let increments: Vec<u32> = r#for.increments.iter().map(|increment| self.expression(increment)).collect();
        let increments = self.expression_list(&increments);
        let body = self.statement(body);

        self.node(SHARP_AST_FOR, 0, self.line(r#for), &[initializations, conditions, increments, body])
    }

    /// An `EXPR_LIST` at the line of its first expression, or null when there are no expressions.
    fn expression_list(&mut self, expressions: &[u32]) -> u32 {
        let Some(&first) = expressions.first() else {
            return NULL;
        };

        let line = self.nodes[first as usize].line;

        self.node(SHARP_AST_EXPR_LIST, 0, line, expressions)
    }

    /// An `IF` list with one `IF_ELEM` per branch, and a null condition for `else`. php-src's grammar reads
    /// `else if` as an `else` whose statement is the next `if`.
    fn r#if(&mut self, r#if: &If) -> u32 {
        let IfBody::Statement(body) = &r#if.body else {
            unreachable!("check_slice refuses a colon-delimited `if`");
        };

        let condition = self.expression(r#if.condition);
        let statement = self.statement(body.statement);
        let mut branches = vec![self.node(SHARP_AST_IF_ELEM, 0, self.line(r#if.condition), &[condition, statement])];
        if let Some(else_clause) = &body.else_clause {
            let statement = self.statement(else_clause.statement);
            branches.push(self.node(SHARP_AST_IF_ELEM, 0, self.line(else_clause.statement), &[NULL, statement]));
        }

        self.node(SHARP_AST_IF, 0, self.line(r#if), &branches)
    }

    fn expression(&mut self, expression: &Expression) -> u32 {
        let line = self.line(expression);

        match expression {
            Expression::Literal(literal) => self.literal(literal),
            Expression::Parenthesized(parenthesized) => self.expression(parenthesized.expression),
            Expression::ConstantAccess(name) => self.name(name),
            Expression::Binary(binary) => {
                let (kind, attr) = binary_kind(binary.operator);
                let lhs = self.expression(binary.lhs);
                let rhs = self.expression(binary.rhs);

                self.node(kind, attr, line, &[lhs, rhs])
            }
            Expression::UnaryPrefix(unary) => {
                let (kind, attr) = prefix_kind(&unary.operator);
                let operand = if unary.operator.is_increment_or_decrement() {
                    self.target(unary.operand)
                } else {
                    self.expression(unary.operand)
                };

                self.node(kind, attr, line, &[operand])
            }
            Expression::UnaryPostfix(unary) => {
                let kind = match unary.operator {
                    UnaryPostfixOperator::PostIncrement(_) => SHARP_AST_POST_INC,
                    UnaryPostfixOperator::PostDecrement(_) => SHARP_AST_POST_DEC,
                };
                let operand = self.target(unary.operand);

                self.node(kind, 0, line, &[operand])
            }
            Expression::Assignment(assignment) => {
                let (kind, attr) = assignment_kind(&assignment.operator);
                let lhs = self.target(assignment.lhs);
                let rhs = self.expression(assignment.rhs);

                self.node(kind, attr, line, &[lhs, rhs])
            }
            Expression::Call(Call::Method(call)) => self.method_call(call),
            Expression::Instantiation(Instantiation {
                class: Expression::Identifier(class),
                argument_list: Some(arguments),
                ..
            }) => {
                let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class));
                let arguments = self.arguments(arguments);

                self.node(SHARP_AST_NEW, 0, line, &[class, arguments])
            }
            // Spec section 4 looks up the kind of `Class.y` when it runs, so the read is a class constant fetch
            // marked to fall back to the static property of the same name.
            Expression::Access(Access::Property(access)) => match self.names.static_property_class(access) {
                Some(class) => {
                    let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(&class.name));
                    let member = self.member(&access.property);

                    self.node(SHARP_AST_CLASS_CONST, ZEND_FETCH_CLASS_MEMBER, line, &[class, member])
                }
                None => {
                    let object = self.expression(access.object);
                    let property = self.member(&access.property);

                    self.node(SHARP_AST_PROP, 0, line, &[object, property])
                }
            },
            Expression::Call(Call::NullSafeMethod(call)) => {
                let object = self.expression(call.object);
                let method = self.member(&call.method);
                let arguments = self.arguments(&call.argument_list);

                self.node(SHARP_AST_NULLSAFE_METHOD_CALL, 0, line, &[object, method, arguments])
            }
            Expression::Access(Access::NullSafeProperty(access)) => {
                let object = self.expression(access.object);
                let property = self.member(&access.property);

                self.node(SHARP_AST_NULLSAFE_PROP, 0, line, &[object, property])
            }
            Expression::TypeOf(type_of) => {
                let class = self.string(ZEND_NAME_FQ, self.line(type_of.class), self.names.get(&type_of.class));

                self.node(SHARP_AST_CLASS_NAME, 0, line, &[class])
            }
            _ => unreachable!("check_slice refuses the expression `{expression}`"),
        }
    }

    /// What an assignment, a compound assignment, `++` or `--` writes: a local or parameter, `object.name`, or
    /// `Class.name`, which php-src's grammar builds as the static property `Class::$name`.
    fn target(&mut self, target: &Expression) -> u32 {
        match target {
            Expression::ConstantAccess(name) if matches!(self.names.binding(&name.name), Some(Binding::Local(_))) => {
                self.variable(name.span(), name.name.value())
            }
            Expression::Access(Access::Property(access)) => match self.names.static_property_class(access) {
                Some(class) => {
                    let line = self.line(target);
                    let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(&class.name));
                    let property = self.member(&access.property);

                    self.node(SHARP_AST_STATIC_PROP, 0, line, &[class, property])
                }
                None => self.expression(target),
            },
            _ => unreachable!("check_slice refuses writing to `{target}`"),
        }
    }

    /// A local, a parameter or `this` is a PHP variable of the same name. Any other bare name outside a call is a
    /// constant, which the engine looks up in the namespace, then globally, as PHP does for an unqualified name.
    fn name(&mut self, name: &ConstantAccess) -> u32 {
        let line = self.line(name);

        match self.names.binding(&name.name) {
            Some(Binding::Local(_) | Binding::This) => self.variable(name.span(), name.name.value()),
            Some(Binding::Constant) => {
                let constant = self.string(ZEND_NAME_NOT_FQ, line, name.name.value());

                self.node(SHARP_AST_CONST, 0, line, &[constant])
            }
            binding => unreachable!("check_slice refuses the name `{}` bound as {binding:?}", name.name),
        }
    }

    fn variable(&mut self, span: Span, name: &[u8]) -> u32 {
        let line = self.line(span);
        let name = self.string(0, line, name);

        self.node(SHARP_AST_VAR, 0, line, &[name])
    }

    /// `Class.m()` is a static call on the class's full name. Any other `object.m()` is an instance call.
    fn method_call(&mut self, call: &MethodCall) -> u32 {
        let line = self.line(call);
        let (kind, object) = match self.names.static_call_class(call) {
            Some(class) => {
                let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(&class.name));

                (SHARP_AST_STATIC_CALL, class)
            }
            None => (SHARP_AST_METHOD_CALL, self.expression(call.object)),
        };
        let method = self.member(&call.method);
        let arguments = self.arguments(&call.argument_list);

        self.node(kind, 0, line, &[object, method, arguments])
    }

    fn member(&mut self, member: &ClassLikeMemberSelector) -> u32 {
        let ClassLikeMemberSelector::Identifier(name) = member else {
            unreachable!("check_slice refuses the member name `{member}`");
        };

        self.string(0, self.line(name.span), name.value)
    }

    fn arguments(&mut self, list: &ArgumentList) -> u32 {
        let mut arguments = Vec::new();
        for argument in &list.arguments {
            arguments.push(match argument {
                Argument::Positional(positional) => self.positional_argument(positional),
                Argument::Named(named) => self.named_argument(named),
            });
        }

        self.node(SHARP_AST_ARG_LIST, 0, self.line(list), &arguments)
    }

    /// An attribute's arguments, which the parser reads as a partial argument list without placeholders.
    fn attribute_arguments(&mut self, list: &PartialArgumentList) -> u32 {
        let mut arguments = Vec::new();
        for argument in &list.arguments {
            arguments.push(match argument {
                PartialArgument::Positional(positional) => self.positional_argument(positional),
                PartialArgument::Named(named) => self.named_argument(named),
                _ => unreachable!("check_slice refuses a placeholder argument"),
            });
        }

        self.node(SHARP_AST_ARG_LIST, 0, self.line(list), &arguments)
    }

    fn positional_argument(&mut self, argument: &PositionalArgument) -> u32 {
        if argument.ellipsis.is_some() {
            unreachable!("check_slice refuses a spread argument");
        }

        self.expression(argument.value)
    }

    fn named_argument(&mut self, argument: &NamedArgument) -> u32 {
        let line = self.line(argument.name.span);
        let name = self.string(0, line, argument.name.value);
        let value = self.expression(argument.value);

        self.node(SHARP_AST_NAMED_ARG, 0, line, &[name, value])
    }

    /// An integer literal too large for `int` is a float, as PHP reads it.
    fn literal(&mut self, literal: &Literal) -> u32 {
        let line = self.line(literal);

        match literal {
            Literal::String(string) => {
                let Some(value) = string.value else {
                    unreachable!("semantics refuses an invalid codepoint escape");
                };

                self.string(0, line, value)
            }
            Literal::Integer(integer) => match integer.value.and_then(|value| i64::try_from(value).ok()) {
                Some(value) => self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = value),
                None => {
                    #[allow(clippy::expect_used)]
                    let value =
                        parse_literal_integer_as_float(integer.raw).expect("the lexer reads only valid integers");

                    self.zval(line, sharp_value::SHARP_DOUBLE, |node| node.double_value = value)
                }
            },
            Literal::Float(float) => {
                let value = float.value.into_inner();

                self.zval(line, sharp_value::SHARP_DOUBLE, |node| node.double_value = value)
            }
            Literal::True(_) => self.zval(line, sharp_value::SHARP_TRUE, |_| {}),
            Literal::False(_) => self.zval(line, sharp_value::SHARP_FALSE, |_| {}),
            Literal::Null(_) => self.zval(line, sharp_value::SHARP_NULL, |_| {}),
        }
    }

    fn declaration(
        &mut self,
        kind: sharp_kind,
        flags: u32,
        start: impl HasSpan,
        end: impl HasSpan,
        name: &[u8],
        children: &[u32],
    ) -> u32 {
        let index = self.node(kind, flags, self.line(start), children);
        let end_line = self.lines.line(end.span().end.offset);
        let name = store_text(&mut self.texts, name.to_vec());

        let node = &mut self.nodes[index as usize];
        node.end_line = end_line;
        node.text = name;

        index
    }

    fn string(&mut self, attr: u32, line: u32, text: &[u8]) -> u32 {
        let text = store_text(&mut self.texts, text.to_vec());

        self.zval(line, sharp_value::SHARP_STRING, |node| {
            node.attr = attr;
            node.text = text;
        })
    }

    fn zval(&mut self, line: u32, value: sharp_value, set: impl FnOnce(&mut sharp_node)) -> u32 {
        let index = self.node(SHARP_AST_ZVAL, 0, line, &[]);

        let node = &mut self.nodes[index as usize];
        node.value = value;
        set(node);

        index
    }

    fn node(&mut self, kind: sharp_kind, attr: u32, line: u32, children: &[u32]) -> u32 {
        let first_child = self.children.len() as u32;
        self.children.extend_from_slice(children);
        self.nodes.push(sharp_node {
            kind,
            attr,
            line,
            end_line: 0,
            first_child,
            child_count: children.len() as u32,
            value: sharp_value::SHARP_NULL,
            long_value: 0,
            double_value: 0.0,
            text: sharp_str::EMPTY,
        });

        (self.nodes.len() - 1) as u32
    }

    fn line(&self, node: impl HasSpan) -> u32 {
        self.lines.line(node.span().start.offset)
    }
}

/// The flags of a member's modifiers. Every modifier is named, so a new one does not compile until it is decided.
fn modifier_flags(modifiers: &Sequence<Modifier>) -> u32 {
    let mut flags = 0;
    for modifier in modifiers {
        flags |= match modifier {
            Modifier::Public(_) => ZEND_ACC_PUBLIC,
            Modifier::Protected(_) => ZEND_ACC_PROTECTED,
            Modifier::Private(_) => ZEND_ACC_PRIVATE,
            Modifier::Static(_) => ZEND_ACC_STATIC,
            Modifier::Abstract(_) => ZEND_ACC_ABSTRACT,
            Modifier::Final(_)
            | Modifier::Readonly(_)
            | Modifier::PublicSet(_)
            | Modifier::ProtectedSet(_)
            | Modifier::PrivateSet(_) => unreachable!("check_slice refuses the modifier `{modifier}`"),
        };
    }

    flags
}

/// The flags of a class's modifiers. `public` adds none, because every PHP class is public.
fn class_flags(modifiers: &Sequence<Modifier>) -> u32 {
    let mut flags = 0;
    for modifier in modifiers {
        flags |= match modifier {
            Modifier::Public(_) => 0,
            Modifier::Abstract(_) => ZEND_ACC_EXPLICIT_ABSTRACT_CLASS,
            Modifier::Final(_) => ZEND_ACC_FINAL,
            Modifier::Protected(_)
            | Modifier::Private(_)
            | Modifier::Static(_)
            | Modifier::Readonly(_)
            | Modifier::PublicSet(_)
            | Modifier::ProtectedSet(_)
            | Modifier::PrivateSet(_) => unreachable!("check_slice refuses the class modifier `{modifier}`"),
        };
    }

    flags
}

/// Whether PHP takes an initial value as the property's default: a constant expression without `new`, on a property
/// that is not `readonly`, which takes no default.
fn is_default(property: &Property, value: &Expression) -> bool {
    !matches!(property, Property::Hooked(auto_property) if auto_property.hook_list.is_get_only())
        && value.is_constant(&PHPVersion::PHP85, false)
}

/// The flags an auto-property's accessors add: `readonly` for a get-only property, which spec section 6.1 sets in
/// the constructor, or the set visibility php-src writes `private(set)` or `protected(set)` from the `set` accessor's
/// access modifier. A private property needs no set visibility.
fn accessor_flags(modifiers: &Sequence<Modifier>, accessors: &PropertyHookList) -> u32 {
    if accessors.is_get_only() {
        return ZEND_ACC_READONLY;
    }

    let set = accessors.hooks.iter().find(|accessor| accessor.name.value == b"set");
    let flags = match set.and_then(|set| set.modifiers.first()) {
        None => 0,
        Some(Modifier::Protected(_)) => ZEND_ACC_PROTECTED_SET,
        Some(Modifier::Private(_)) => ZEND_ACC_PRIVATE_SET,
        Some(modifier) => unreachable!("check_slice refuses the accessor modifier `{modifier}`"),
    };

    if modifiers.contains_private() { 0 } else { flags }
}

/// The binary operators of the slice, as php-src's grammar builds them. Every operator is named, so a new one does
/// not compile until it is decided.
fn binary_kind(operator: BinaryOperator) -> (sharp_kind, u32) {
    match operator {
        BinaryOperator::Addition(_) => (SHARP_AST_BINARY_OP, ZEND_ADD),
        BinaryOperator::Subtraction(_) => (SHARP_AST_BINARY_OP, ZEND_SUB),
        BinaryOperator::Multiplication(_) => (SHARP_AST_BINARY_OP, ZEND_MUL),
        BinaryOperator::Division(_) => (SHARP_AST_BINARY_OP, ZEND_DIV),
        BinaryOperator::Modulo(_) => (SHARP_AST_BINARY_OP, ZEND_MOD),
        BinaryOperator::Equal(_) => (SHARP_AST_BINARY_OP, ZEND_IS_EQUAL),
        BinaryOperator::NotEqual(_) => (SHARP_AST_BINARY_OP, ZEND_IS_NOT_EQUAL),
        BinaryOperator::Identical(_) => (SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL),
        BinaryOperator::NotIdentical(_) => (SHARP_AST_BINARY_OP, ZEND_IS_NOT_IDENTICAL),
        BinaryOperator::LessThan(_) => (SHARP_AST_BINARY_OP, ZEND_IS_SMALLER),
        BinaryOperator::LessThanOrEqual(_) => (SHARP_AST_BINARY_OP, ZEND_IS_SMALLER_OR_EQUAL),
        BinaryOperator::GreaterThan(_) => (SHARP_AST_GREATER, 0),
        BinaryOperator::GreaterThanOrEqual(_) => (SHARP_AST_GREATER_EQUAL, 0),
        BinaryOperator::And(_) => (SHARP_AST_AND, 0),
        BinaryOperator::Or(_) => (SHARP_AST_OR, 0),
        BinaryOperator::NullCoalesce(_) => (SHARP_AST_COALESCE, 0),
        BinaryOperator::Exponentiation(_)
        | BinaryOperator::BitwiseAnd(_)
        | BinaryOperator::BitwiseOr(_)
        | BinaryOperator::BitwiseXor(_)
        | BinaryOperator::LeftShift(_)
        | BinaryOperator::RightShift(_)
        | BinaryOperator::AngledNotEqual(_)
        | BinaryOperator::Spaceship(_)
        | BinaryOperator::StringConcat(_)
        | BinaryOperator::Instanceof(_)
        | BinaryOperator::LowAnd(_)
        | BinaryOperator::LowOr(_)
        | BinaryOperator::LowXor(_) => unreachable!("check_slice refuses the operator `{operator}`"),
    }
}

/// The prefix operators of the slice, as php-src's grammar builds them. Every operator is named, so a new one does
/// not compile until it is decided.
fn prefix_kind(operator: &UnaryPrefixOperator) -> (sharp_kind, u32) {
    match operator {
        UnaryPrefixOperator::Negation(_) => (SHARP_AST_UNARY_MINUS, 0),
        UnaryPrefixOperator::Plus(_) => (SHARP_AST_UNARY_PLUS, 0),
        UnaryPrefixOperator::Not(_) => (SHARP_AST_UNARY_OP, ZEND_BOOL_NOT),
        UnaryPrefixOperator::PreIncrement(_) => (SHARP_AST_PRE_INC, 0),
        UnaryPrefixOperator::PreDecrement(_) => (SHARP_AST_PRE_DEC, 0),
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
        | UnaryPrefixOperator::BitwiseNot(_) => unreachable!("check_slice refuses the operator `{operator}`"),
    }
}

/// The assignment operators of the slice, as php-src's grammar builds them. Every operator is named, so a new one
/// does not compile until it is decided.
fn assignment_kind(operator: &AssignmentOperator) -> (sharp_kind, u32) {
    match operator {
        AssignmentOperator::Assign(_) => (SHARP_AST_ASSIGN, 0),
        AssignmentOperator::Addition(_) => (SHARP_AST_ASSIGN_OP, ZEND_ADD),
        AssignmentOperator::Subtraction(_) => (SHARP_AST_ASSIGN_OP, ZEND_SUB),
        AssignmentOperator::Multiplication(_) => (SHARP_AST_ASSIGN_OP, ZEND_MUL),
        AssignmentOperator::Division(_) => (SHARP_AST_ASSIGN_OP, ZEND_DIV),
        AssignmentOperator::Coalesce(_) => (SHARP_AST_ASSIGN_COALESCE, 0),
        AssignmentOperator::Modulo(_)
        | AssignmentOperator::Exponentiation(_)
        | AssignmentOperator::Concat(_)
        | AssignmentOperator::BitwiseAnd(_)
        | AssignmentOperator::BitwiseOr(_)
        | AssignmentOperator::BitwiseXor(_)
        | AssignmentOperator::LeftShift(_)
        | AssignmentOperator::RightShift(_) => unreachable!("check_slice refuses the operator `{operator}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catch_panic;

    /// Lowers a class holding `method`, skipping the semantic checks that would refuse it, and asserts the result is
    /// one internal error and no nodes.
    fn assert_internal_error(method: &str) {
        let source = format!("class Report\n{{\n    {method}\n}}\n");

        let unit = catch_panic(|| {
            let file = File::ephemeral(Cow::Borrowed(b"src/Report.sharp"), Cow::Owned(source.into_bytes()));
            let arena = LocalArena::new();
            let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
            let names = NameResolver::new(&arena).resolve(program);

            Lowering::new(&Lines::new(&file.contents), &names).program(program)
        });

        assert_eq!(unit.abi.node_count, 0, "{method}");
        assert_eq!(unit.diagnostics.len(), 1, "{method}");
        assert!(unit.texts[0].starts_with(b"internal error in the PHP# front end: "), "{method}");
    }

    #[test]
    fn a_write_to_anything_but_a_local_or_a_member_returns_an_internal_error_and_no_nodes() {
        assert_internal_error("public void run() { PHP_INT_MAX = 1; }");
        assert_internal_error("public void run() { PHP_INT_MAX += 1; }");
        assert_internal_error("public void run() { PHP_INT_MAX++; }");
    }

    #[test]
    fn a_parameter_without_a_type_returns_an_internal_error_and_no_nodes() {
        assert_internal_error("public void run($extra) {}");
    }
}
