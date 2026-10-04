use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
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
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Hint;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::PositionalArgument;
use mago_syntax::cst::Program;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefixOperator;
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
use crate::sharp_kind::SHARP_AST_ASSIGN_OP;
use crate::sharp_kind::SHARP_AST_BINARY_OP;
use crate::sharp_kind::SHARP_AST_CLASS;
use crate::sharp_kind::SHARP_AST_CONST;
use crate::sharp_kind::SHARP_AST_CONST_DECL;
use crate::sharp_kind::SHARP_AST_CONST_ELEM;
use crate::sharp_kind::SHARP_AST_DECLARE;
use crate::sharp_kind::SHARP_AST_GREATER;
use crate::sharp_kind::SHARP_AST_GREATER_EQUAL;
use crate::sharp_kind::SHARP_AST_IF;
use crate::sharp_kind::SHARP_AST_IF_ELEM;
use crate::sharp_kind::SHARP_AST_METHOD;
use crate::sharp_kind::SHARP_AST_METHOD_CALL;
use crate::sharp_kind::SHARP_AST_NAMED_ARG;
use crate::sharp_kind::SHARP_AST_NAMESPACE;
use crate::sharp_kind::SHARP_AST_OR;
use crate::sharp_kind::SHARP_AST_PARAM;
use crate::sharp_kind::SHARP_AST_PARAM_LIST;
use crate::sharp_kind::SHARP_AST_POST_DEC;
use crate::sharp_kind::SHARP_AST_POST_INC;
use crate::sharp_kind::SHARP_AST_PRE_DEC;
use crate::sharp_kind::SHARP_AST_PRE_INC;
use crate::sharp_kind::SHARP_AST_PROP;
use crate::sharp_kind::SHARP_AST_RETURN;
use crate::sharp_kind::SHARP_AST_STATIC_CALL;
use crate::sharp_kind::SHARP_AST_STMT_LIST;
use crate::sharp_kind::SHARP_AST_UNARY_MINUS;
use crate::sharp_kind::SHARP_AST_UNARY_OP;
use crate::sharp_kind::SHARP_AST_UNARY_PLUS;
use crate::sharp_kind::SHARP_AST_VAR;
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

/// A null child.
const NULL: u32 = u32::MAX;

/// Runs the PHP# file at `path` through the parser, the binder and the semantic checks, and lowers it into the tree
/// php-src builds for the equivalent PHP. Any error returns diagnostics and no nodes. The file is PHP# whatever its
/// name, because `ext/sharp` decided that before calling.
pub(crate) fn lower(path: Vec<u8>, source: Vec<u8>) -> Box<Unit> {
    let file = File::ephemeral(Cow::Owned(path), Cow::Owned(source));
    let arena = LocalArena::new();
    let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
    if !program.errors.is_empty() {
        return Unit::failed(
            program
                .errors
                .iter()
                .map(|error| {
                    diagnostic(&file, Some(error.span()), sharp_severity::SHARP_PARSE_ERROR, error.to_string())
                })
                .collect(),
        );
    }

    let names = NameResolver::new(&arena).resolve(program);
    let errors: Vec<Diagnostic> = SemanticsChecker::new(PHPVersion::PHP85)
        .check(&file, program, &names)
        .iter()
        .filter(|issue| issue.level == Level::Error)
        .map(|issue| {
            diagnostic(&file, issue.primary_span(), sharp_severity::SHARP_COMPILE_ERROR, issue.message.clone())
        })
        .collect();
    if !errors.is_empty() {
        return Unit::failed(errors);
    }

    Lowering::new(&file, &names).program(program)
}

/// A diagnostic at the start of `span`. Without a span it is at line 0, column 0, which the ABI defines as no
/// position.
fn diagnostic(file: &File, span: Option<Span>, severity: sharp_severity, message: String) -> Diagnostic {
    let (line, column) = span
        .map_or((0, 0), |span| (file.line_number(span.start.offset) + 1, file.column_number(span.start.offset) + 1));

    Diagnostic { line, column, severity, message }
}

/// The line the Zend scanner ends on, which counts `\n`, `\r\n` and a lone `\r` each as one line ending.
fn last_line(source: &[u8]) -> u32 {
    let line_endings = source
        .iter()
        .enumerate()
        .filter(|&(index, &byte)| byte == b'\n' || (byte == b'\r' && source.get(index + 1) != Some(&b'\n')))
        .count();

    line_endings as u32 + 1
}

/// Lowers one checked file. Every node is pushed after its children, and each node's children are contiguous.
struct Lowering<'lowering, 'arena> {
    file: &'lowering File,
    names: &'lowering ResolvedNames<'arena>,
    nodes: Vec<sharp_node>,
    children: Vec<u32>,
    texts: Vec<Box<[u8]>>,
}

impl<'lowering, 'arena> Lowering<'lowering, 'arena> {
    fn new(file: &'lowering File, names: &'lowering ResolvedNames<'arena>) -> Self {
        Self { file, names, nodes: Vec::new(), children: Vec::new(), texts: Vec::new() }
    }

    /// `declare(strict_types=1);` first, then the namespaces and classes. Imports are not lowered: every class name
    /// in the tree is fully qualified.
    fn program(mut self, program: &Program) -> Box<Unit> {
        let mut statements = vec![self.strict_types()];
        for statement in &program.statements {
            self.file_statement(statement, &mut statements);
        }

        let root = self.node(SHARP_AST_STMT_LIST, 0, 1, &statements);
        self.nodes[root as usize].end_line = last_line(&self.file.contents);

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
            _ => unreachable!("check_slice refuses the file statement `{statement}`"),
        }
    }

    fn class(&mut self, class: &Class) -> u32 {
        let mut methods = Vec::new();
        for member in &class.members {
            let ClassLikeMember::Method(method) = member else {
                unreachable!("check_slice refuses the class member `{member}`");
            };

            methods.push(self.method(method));
        }

        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(class.left_brace), &methods);

        self.declaration(
            SHARP_AST_CLASS,
            0,
            class.class.span,
            class.right_brace,
            class.name.value,
            &[NULL, NULL, members, NULL, NULL],
        )
    }

    /// A method is a `function` with its return type after its parameters. Its first line is where PHP writes
    /// `function`: the return type, which every PHP# method starts with.
    fn method(&mut self, method: &Method) -> u32 {
        let mut flags = 0;
        for modifier in &method.modifiers {
            flags |= match modifier {
                Modifier::Public(_) => ZEND_ACC_PUBLIC,
                Modifier::Protected(_) => ZEND_ACC_PROTECTED,
                Modifier::Private(_) => ZEND_ACC_PRIVATE,
                Modifier::Static(_) => ZEND_ACC_STATIC,
                _ => unreachable!("check_slice refuses the method modifier `{modifier}`"),
            };
        }

        let mut parameters = Vec::new();
        for parameter in &method.parameter_list.parameters {
            parameters.push(self.parameter(parameter));
        }

        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, self.line(&method.parameter_list), &parameters);
        let MethodBody::Concrete(body) = &method.body else {
            unreachable!("semantics refuses a method without a body");
        };
        let body = self.block(body);
        let Some(return_type_hint) = &method.return_type_hint else {
            unreachable!("the PHP# parser gives every method its return type");
        };
        let return_type = self.hint(&return_type_hint.hint);

        self.declaration(
            SHARP_AST_METHOD,
            flags,
            return_type_hint,
            method.body.span(),
            method.name.value,
            &[parameters, NULL, body, return_type, NULL],
        )
    }

    fn parameter(&mut self, parameter: &FunctionLikeParameter) -> u32 {
        let Some(hint) = &parameter.hint else {
            unreachable!("check_slice refuses a parameter without a type");
        };
        let hint = self.hint(hint);
        let name = self.string(0, self.line(parameter.variable.span), parameter.variable.name);
        let default = parameter.default_value.as_ref().map_or(NULL, |default| self.expression(default.value));

        self.node(SHARP_AST_PARAM, 0, self.line(parameter), &[hint, name, default, NULL, NULL, NULL])
    }

    /// A built-in type is written unqualified, and a class by its full name.
    fn hint(&mut self, hint: &Hint) -> u32 {
        match hint {
            Hint::Integer(name) | Hint::Float(name) | Hint::Bool(name) | Hint::String(name) | Hint::Void(name) => {
                self.string(ZEND_NAME_NOT_FQ, self.line(name.span), name.value)
            }
            Hint::Identifier(class) => self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class)),
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
            Statement::LocalDeclaration(local) => {
                let variable = self.variable(local.name.span, local.name.value);
                let value = self.expression(local.value);

                self.node(SHARP_AST_ASSIGN, 0, self.line(local), &[variable, value])
            }
            Statement::If(r#if) => self.r#if(r#if),
            _ => unreachable!("check_slice refuses the statement `{statement}`"),
        }
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
            Expression::Access(Access::Property(access)) => {
                let object = self.expression(access.object);
                let property = self.member(&access.property);

                self.node(SHARP_AST_PROP, 0, line, &[object, property])
            }
            _ => unreachable!("check_slice refuses the expression `{expression}`"),
        }
    }

    /// What an assignment, a compound assignment, `++` or `--` writes: a local or parameter, or `object.name`, as
    /// php-src's `variable` rule takes them.
    fn target(&mut self, target: &Expression) -> u32 {
        match target {
            Expression::ConstantAccess(name) if matches!(self.names.binding(&name.name), Some(Binding::Local(_))) => {
                self.variable(name.span(), name.name.value())
            }
            Expression::Access(Access::Property(_)) => self.expression(target),
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
            let argument = match argument {
                Argument::Positional(PositionalArgument { ellipsis: None, value }) => self.expression(value),
                Argument::Named(named) => {
                    let line = self.line(named.name.span);
                    let name = self.string(0, line, named.name.value);
                    let value = self.expression(named.value);

                    self.node(SHARP_AST_NAMED_ARG, 0, line, &[name, value])
                }
                Argument::Positional(_) => unreachable!("check_slice refuses a spread argument"),
            };

            arguments.push(argument);
        }

        self.node(SHARP_AST_ARG_LIST, 0, self.line(list), &arguments)
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
        let end_line = self.file.line_number(end.span().end.offset) + 1;
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
        self.file.line_number(node.span().start.offset) + 1
    }
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
        AssignmentOperator::Modulo(_)
        | AssignmentOperator::Exponentiation(_)
        | AssignmentOperator::Concat(_)
        | AssignmentOperator::BitwiseAnd(_)
        | AssignmentOperator::BitwiseOr(_)
        | AssignmentOperator::BitwiseXor(_)
        | AssignmentOperator::LeftShift(_)
        | AssignmentOperator::RightShift(_)
        | AssignmentOperator::Coalesce(_) => unreachable!("check_slice refuses the operator `{operator}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catch_panic;

    /// Lowers a method with `parameters` and `body`, skipping the semantic checks that would refuse them.
    fn lower_unchecked(parameters: &str, body: &str) -> Box<Unit> {
        let source = format!("class Report\n{{\n    public void run({parameters})\n    {{\n{body}    }}\n}}\n");

        catch_panic(|| {
            let file = File::ephemeral(Cow::Borrowed(b"src/Report.sharp"), Cow::Owned(source.into_bytes()));
            let arena = LocalArena::new();
            let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
            let names = NameResolver::new(&arena).resolve(program);

            Lowering::new(&file, &names).program(program)
        })
    }

    #[test]
    fn a_write_to_anything_but_a_local_or_a_member_returns_an_internal_error_and_no_nodes() {
        for body in ["        PHP_INT_MAX = 1;\n", "        PHP_INT_MAX += 1;\n", "        PHP_INT_MAX++;\n"] {
            let unit = lower_unchecked("", body);

            assert_eq!(unit.abi.node_count, 0, "{body}");
            assert_eq!(unit.diagnostics.len(), 1, "{body}");
            assert!(unit.texts[0].starts_with(b"internal error in the PHP# front end: "), "{body}");
        }
    }

    #[test]
    fn a_parameter_without_a_type_returns_an_internal_error_and_no_nodes() {
        let unit = lower_unchecked("$extra", "");

        assert_eq!(unit.abi.node_count, 0);
        assert_eq!(unit.diagnostics.len(), 1);
        assert!(unit.texts[0].starts_with(b"internal error in the PHP# front end: "));
    }
}
