use mago_bytes::BytesDisplay;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::BindingError;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_names::binding::php_method_name;
use mago_names::scope::php_name;
use mago_php_version::PHPVersion;
use mago_reporting::Annotation;
use mago_reporting::AnnotationKind;
use mago_reporting::Issue;
use mago_span::HasPosition;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Break;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::CompositeString;
use mago_syntax::cst::Conditional;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Construct;
use mago_syntax::cst::Continue;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::GenericHint;
use mago_syntax::cst::Global;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Keyword;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::MagicConstant;
use mago_syntax::cst::Method;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::Namespace;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Node;
use mago_syntax::cst::NullableHint;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PatternMatchArmBody;
use mago_syntax::cst::Program;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyHook;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookConcreteBody;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::cst::PropertyItem;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StringPart;
use mago_syntax::cst::Terminator;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::TypeParameterList;
use mago_syntax::cst::TypePattern;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::UnionHint;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItem;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::Variable;
use mago_syntax::cst::While;
use mago_syntax::cst::WhileBody;
use mago_syntax_core::stack::ensure_sufficient_stack;

use crate::internal::consts::RESERVED_CLASS_NAMES;
use crate::internal::consts::RESERVED_KEYWORDS;
use crate::internal::consts::SOFT_RESERVED_KEYWORDS_MINUS_SYMBOL_ALLOWED;
use crate::internal::context::Context;

/// The PHP superglobals, which spec section 29 removes. A PHP# local or parameter of one of these names would read or
/// replace it.
const SUPERGLOBALS: [&[u8]; 9] =
    [b"GLOBALS", b"_SERVER", b"_GET", b"_POST", b"_FILES", b"_COOKIE", b"_SESSION", b"_REQUEST", b"_ENV"];

/// How PHP# writes PHP's `mixed`. The parser reads both as `Hint::Mixed`, and only `Any` is PHP#.
const ANY: &[u8] = b"Any";

/// Checks a PHP# file against the slice: the only constructs a `.sharp` file may use, and the contract the
/// engine's lowering implements.
///
/// - At file level: `namespace`, `import`, `class`, `interface` and `enum`. A file has at most one namespace, named
///   and written without braces.
/// - A class: attributes, an optional `public`, `abstract` or `final`, a name, optional type parameters, an optional
///   `: Base, Interface` header, constants, fields, properties and methods, with no other modifiers, `extends` or
///   `implements`. The engine tells the base class from the interfaces when it links the class.
/// - An interface: an optional `public`, a name, optional type parameters, an optional `: Interface` header and
///   methods, with no attributes, other modifiers or `extends`. An interface method has optional type parameters,
///   parameters, a return type and no body, as spec section 29 writes `Money quote(Cart cart);`. A modifier on it is an
///   error, because every interface method is public.
/// - Type parameters, as spec section 11 declares them: on a class, an interface and a method, as in
///   `<out TItem : DatabaseEntity & Shareable, TKey>`. A name is `T`, or `T` and an uppercase ASCII letter, as in
///   `TItem`, and any other name is an error, as C# names them. A list that declares a name twice is an error. `in` and
///   `out` on a method's type parameter are errors, because only a class or an interface declares variance, spec
///   section 11.1. A bound is a class or an interface, a generic class type, as in `Comparable<TItem>`, or several of
///   them joined with `&`, and any other bound is an error. A type parameter is in scope in its class's or interface's
///   whole declaration, its header included, and in its method's signature and body, as the binder decides. A static
///   member uses none of its class's type parameters, because G1 erases them and every `Box<…>` shares the member: not
///   a static field's or property's type, nor a static method's signature or body. A static method's own type
///   parameters are its own. The analyzer checks variance, bounds and inference.
/// - An enum: attributes, an optional `public`, a name, an optional `: string, Interface` header, constants, cases and
///   methods, with no other modifiers or `implements`. A leading `int` or `string` in the header is the backing type,
///   and every class name is an interface. A constant follows a class constant's rules. A case has attributes as a
///   class has them, and is `case Active;` or, in a backed enum, `case Active = "a";`, whose value is a constant
///   expression. A case named `class`, compared ignoring case, is an error, as in PHP. A method follows a class's
///   method rules, but an enum has no constructor, so a method without a return type is an error. PHP's `check_enum`
///   reports a property and any other backing type, and the analyzer a class name in the header.
/// - A header of a class, an interface or an enum that names one type twice is an error, and so is an enum header that
///   names `UnitEnum` or `BackedEnum`, because the engine refuses both when it declares the class. A header names each
///   class and interface by its short name, and a generic one with its type arguments, as in `: PaginatedList<Order>`,
///   whose type arguments are a parameter's types. Any other type, a type parameter, `List`, `Map` and `Class`
///   included, is not supported yet.
/// - Attributes: on a class, an enum, an enum case, a method, a field, a property and a parameter, written `[Name]` or
///   `[Name(arguments)]`, several in one list, as in `[Field("Name"), Searchable]`, or in several lists. An argument
///   is positional or named, and is a constant expression as a parameter default is, list and map literals included.
///   PHP's own check reports a spread argument. An attribute target, as in `[return: NotNull]`, is a parse error.
/// - A constant: `public`, `protected` or `private`, an optional type from the types below but `void`, one name, and a
///   value as a parameter default is.
/// - A field: `private` or `protected`, an optional `static`, a type, one name and an optional initial value. A
///   `public` field is an error, because spec section 6 has no public fields.
/// - An override: a field written `override`, which replaces a plain PHP parent's property, spec section 6.1. It may
///   be `public`, as the parent's property is, and has a constant initial value. The analyzer checks the parent.
/// - A static field or property: an initial value that is constant, which PHP stores as its default, and a `set`
///   accessor, because PHP has no `readonly` static property.
/// - An initial value: any expression a method body has, without `this`, which is an error. A constant initial value,
///   as a parameter default is, becomes the member's default, and any other runs at the start of the constructor.
/// - A property: `public`, `protected` or `private`, an optional `static`, a type, one name, the accessors `get` and
///   an optional `set` that may take an access modifier narrower than the property's, and an optional initial value
///   after the accessors, which is a field's. A property without `get`, an accessor declared twice, or a `set` access
///   modifier as wide as the property's is an error, as in C#. A get-only property is set only where `readonly`
///   allows, and the analyzer reports every other write to it.
/// - An accessor: `get;` or `set;`, which is an auto accessor, or a body, `=> expr;` or a block, which is a method
///   body's. A body names the property's storage `field`, and a `set` body names the incoming value `value`. A
///   property has storage when an accessor is auto or a body uses `field`, and a property without storage takes no
///   initial value and is not declared by a constructor parameter, because PHP runs it as a virtual property. A `get`
///   block returns a value and a `set` block returns none, as php-src types its hooks. `field` in a lambda is an
///   error, because PHP runs a lambda as a function of its own, where `$this->name` calls the accessor again.
///   `this.name` or `this?.name` inside `name`'s own accessor is an error that names `field`, because PHP reads the
///   storage there, while C# calls the accessor again. A static property with a body is not supported yet, because
///   PHP has no hooks on a static property, and neither is a non-constant initial value on a property whose `set` has
///   a body, which would run the body in the constructor.
/// - A computed property: `public`, `protected` or `private`, a type, one name and an expression body, as in
///   `public string slug => Str.slug(name);`. Its expression is a method body's expression and runs on each read. A
///   `static` computed property is not supported yet, because PHP has no hooks on a static property.
/// - A method: `public`, `protected` or `private`, an optional `static`, `virtual` or `override`, optional type
///   parameters, parameters, a return type and a body, or `abstract` and no body. The analyzer decides `virtual` and `override`, as spec section 22
///   writes them. Its name does not start with `__`, which PHP reserves for magic methods, and is not its class's name,
///   compared ignoring case, which PHP# gives to the constructor, nor, compared ignoring case, a property's of its
///   class.
/// - The constructor: a method named exactly after its class, without a return type and not `static`. A method
///   without a return type named otherwise is an error. A constructor parameter with an access modifier declares a
///   member: a field when `private` or `protected` without accessors, and a property with accessors, which follow the
///   auto-property rules. A `public` parameter without accessors is an error, as spec section 9 says. The constructor
///   alone may be `required`, which keeps it callable as `new Self(…)` on every subclass, spec section 25. The parser
///   reports `required` on any other member as not supported yet.
/// - A parameter: always a type, a name and an optional default, and never by reference. A default is a constant
///   expression: a literal, a constant, `typeof(X)`, a list or map literal of them, or the operators below on them,
///   without `++` and `--`. A parameter written `int ...values` is variadic, as spec section 7 writes it, and PHP's
///   own checks report a variadic parameter that is not the last or has a default. `check_parameter_list` reports a
///   variadic parameter that declares a member or has a `void` type in a `.sharp` file only, as upstream Mago reports
///   neither. An optional parameter before a required one is an error, because PHP would make it required. A
///   variadic parameter is not a required one, as in PHP. A default of `Position.current()` of the standard library is
///   not supported yet, because as a default it gives the caller's position, spec section 27, which waits for typed
///   compilation.
/// - Types: `int`, `float`, `bool`, `string`, `Any`, a class or a type parameter in scope written by its short name,
///   generic types of these, function types `Function<R(P1, P2)>` of these, and `void` as a return type, a function
///   type's too. A generic type is `List<T>` and `Map<TKey, TValue>` of spec section 12, `Class<T>` of section 25,
///   whose type argument is a class, an interface or a type parameter, and a class with one or more type arguments of
///   section 11, as in `PaginatedList<Order>`, whose arity and bounds the analyzer checks. A type argument is any of
///   these types but `void`, and a type parameter takes none. A `Map`'s key is `int` or `string`, or a type the
///   analyzer finds an `int` or `string` backing value for. PHP's own check reports a `void` parameter and a `void`
///   field. Each of them is nullable when written with `?` after it, as in `int?` or `Map<string, Any?>`, and PHP's
///   own check reports `void?`. PHP's `mixed` is an error, because spec section 24 writes it `Any?`. A union of them but
///   `void`, written inline as spec section 24 writes it, as in `int|string` or `List<int>|string`, goes wherever a
///   type goes, and each member takes the type arguments it takes outside a union. PHP's own check reports `void` and
///   a nullable type, as in `int?|string`, in a union, and `check_union` reports a type written twice, which the
///   engine refuses. A union holds null only when written in parentheses with `?` after it, as in `(int|string)?`,
///   which the engine compiles as `int|string|null`.
///   `check_union` reports `null` written in a union, as in `int|null`, as not supported yet. PHP's own check reports
///   a nullable union inside another union, and a single type in parentheses, as in `(Calc)?`, which PHP# writes
///   `Calc?`. `(int)?` is a parse error, as PHP lexes `(int)` as a cast. A field, or a property with storage and `set`,
///   of a nullable type or a nullable union starts as null without an initial value, as in C# and Swift, so
///   `private int? total;` holds null until it is written, and the engine gives it the default `null`. A get-only
///   property with storage of one without an initial value is not supported yet, because it is set only where
///   `readonly` allows, and `readonly` takes no default. A property without storage holds nothing to start with.
/// - `Self`, written exactly so, is the class a static method is called on, spec section 25, and PHP's `static`. It is
///   a method's return type only, as in Swift, nullable as in `Self?`, in a union as in `Self|int`, or a type argument
///   there as in `List<Self>`, and any other type position, a function type's included, is an error. The lexer reads
///   `Self` and `self` as one keyword, and every other spelling is PHP's `self`, which PHP# removes: `self` and
///   `static`, as a type or in an expression, are errors that name the class, the enum or `Self`.
/// - A method body: a block, or an expression body, `=> expr;`, which returns the expression, or runs it as a
///   statement in a `void` method and the constructor, as in C#.
/// - In a method body: blocks, expression statements, `return`, `let` and `const` declarations, `if` with `else if`
///   and `else`, `while`, `do … while`, `for` with a `let` or `const` counter or with expressions, `for … of` over a
///   value or a key and value, `break` and `continue` without a level, and `try` with `catch` clauses and `finally`.
///   The body of `if`, `else` and each loop is a block in braces. A catch clause names one or more classes separated
///   by `|`, and an optional variable written without `$`, which lives until the clause's block ends. A type parameter
///   as a catch type is not supported yet. A local
///   statement can have its type written, as in `Money? total = null;` or `const int base = 2;`, from the types
///   above but `void`.
/// - Writes: `=`, compound assignment, `++` and `--` write only a local, a parameter, a member written
///   `object.name` or, when static, `Class.name`, or an index of one of them written `target[key]`. The analyzer,
///   which knows the types, allows an index write only on a `Map`, and a read not under `??` or `?.` only on a `List`,
///   as spec section 12 decides.
/// - A constant, an enum case or a static member is reached through its class name, as in `Report.count`, and the
///   engine looks up which one it is when it runs. A bare constant or static member name is an error that names the
///   class. A constant expression, which is a parameter default, an attribute argument, a constant's value, a
///   constant initial value or an enum case's value, reads only a constant or an enum case this way, as PHP does: a
///   static field or property there is an error when this file declares it, and the analyzer reports it otherwise.
/// - In expressions: literals, list literals `[a, b]` and `[...a, b]`, map literals `["key": value]` and `[:]`, index reads
///   `value[key]`, templates, parentheses, bare names, assignment, the operators below, and method calls and property
///   reads written with `.` or `?.` and a member name, `new Class(...)` on a class written by its short name,
///   `new Self(...)` in a class whose constructor is `required`, calls of a function by its bare name,
///   `super.method(...)`, which calls the parent's method, and `Self.method(...)`, which calls a static method of the
///   class a static method is called on, each with positional, named and spread arguments, as in
///   `Money.sum(...prices)` and `max(...prices)`, `throw`, which is an expression as in PHP, `exit(code)` and
///   `exit()`, which spec section 8 keeps as PHP 8.4's built-in function, and `typeof(X)` on a class written by its
///   short name, without a member read or called on it. `new` and each method call, `super.` and
///   `Self.` calls included, take type arguments, as in `new PaginatedList<Order>(rows)` and
///   `Json.decode<WebhookPayload>(body)`, each a method body's type but `void`. `new Self(...)` in an enum is an error,
///   because an enum has no constructor. `Self.name` read or written is not supported yet, as `super.name` is not.
///   PHP's own check reports a positional argument after a spread and a spread after a named argument, and
///   `check_function_call` reports `assert` with a spread as its only argument, after which PHP adds a positional
///   description. `?.` never follows a class. A function is the global function of that name, PHP's own or one a
///   library or the app declares, as spec sections 8 and 29 keep them, and the engine calls the global one. A string
///   literal's `\u{...}` escapes are valid codepoints, as PHP requires. A `"…"` string never interpolates, and a
///   template, `` `Order ${number}` ``, interpolates any expression of this list in each `${…}` and takes
///   JavaScript's escapes, as spec section 18 writes them.
/// - Lambdas, as spec section 3 writes them: `x => x.id`, `(a, b) => a + b` and `() => { … }`, whose body is an
///   expression or a block of a method body. A parameter is a method's, with its type optional, so the last one can
///   be variadic, as in `(int ...values) => count(values)`. A lambda captures the variable itself, except a loop
///   variable that code changes, whose capture is not supported yet. A call of a local by its bare name, `f(x)`,
///   calls the lambda the local holds, with positional, named and spread arguments.
/// - Operators: `+ - * / % **`, `== != === !== < > <= >=`, `&& || !`, `??`, unary `-` and `+`, `++` and `--`, and
///   `= += -= *= /= **= ??=`.
/// - The ternary `c ? a : b` in a method body, as spec section 21 writes it. PHP's `a ?: b` is an error, and a
///   ternary as the condition of another needs parentheses, as in PHP 8.
/// - Pattern matching in a method body, as spec section 21 writes it: `x is pattern`, `x as T` to a type that is not
///   nullable or `void`, and `match`, whose arms are `pattern => value`, `pattern when condition => value` and one
///   `default => value`. A `match` needs `default` unless its arms name only `Class.y` values or `null`, as a `match`
///   on an enum does, and the analyzer reports the cases such a `match` misses. A `match` statement's arm may be a
///   block. A pattern is a type with an optional name, as in `int count`, a value, a comparison such as `< 10`, a
///   properties pattern such as `{ total: > 0 }`, or patterns joined by `and`, `or` and `not`. A type pattern is never
///   nullable, and a name under `or` or `not` is an error, as C#'s CS8780, except under the `not` that starts the
///   pattern of `is`. The parser reports list patterns, and enum case patterns with fields or a name, as not supported
///   yet.
/// - What needs a type argument while the code runs, which G1 erases, is not supported yet: a type parameter in a
///   pattern, a `match` arm, `as` or a catch clause, `typeof` and `new` of a type parameter, a static member reached
///   through a type parameter, as in `TItem.make()` and `TItem.LIMIT`, and any type with type arguments in a pattern or
///   `as`, as in `value is List<int>`, `value as PaginatedList<Order>` and `value is Class<Order>`.
/// - Casts: `(int)`, `(float)` and `(string)` in a method body, as spec section 24 writes them. PHP's other casts and
///   its cast aliases, such as `(bool)` and `(integer)`, are errors.
/// - A bare `Int`, `Float`, `Position`, `Environment` or `List` names the class `Sharp\<Name>` of the engine's standard
///   library wherever a class is named, unless the file imports or declares a class-like of that name, spec section 23.
///   So `Int.parse(text)`, `Position.current()`, `new Environment()` and `List.wrap(value)` call it. PHP reserves
///   `Int`, `Float` and `List`, so no file declares a class of those.
///
/// The check runs on every node the checking walk enters, and refuses any node, or any position of a node, that this
/// list does not name. It reports each refusal once, at its outermost node, and skips the nodes inside the refusal's
/// span. A node refused at a part, such as a method at its name or a cast at its operator, keeps its other parts
/// checked, so every PHP form PHP# removes is refused by name wherever it is written. It does not run on a file with a
/// parse error, which is the one error to fix first. The constructs PHP# never has, such as `$`
/// variables, `global` and top-level functions, keep their own errors. So do the PHP forms PHP# removes, each an error
/// that names its replacement. In a method body, they are `echo`, `print`, `die`, and `exit` without parentheses or
/// with a string literal or a template as its first argument. In a method body and a constant expression, they are
/// superglobals, read or written, magic constants and other `__Something__` names, `(array)` and `(object)`. A
/// `__Something__` name is an error when it is declared too, as a class, interface, enum, enum case, method, property,
/// constant, parameter or local. A method name that starts but does not end with `__`, as PHP's magic methods do,
/// stays not supported yet.
///
/// Eleven more refusals need inferred types or the codebase, so the analyzer makes them as its part of this contract:
/// - `+` that may join a string with any other value, which spec section 18 makes an error, in
///   `analyze_arithmetic_operation`. `+` on two strings joins them.
/// - a condition of `if`, `while`, `do … while`, `for` or `? :`, or an operand of `&&`, `||` or `!`, that is not
///   `bool`, which spec section 21 makes an error, in `Context::report_non_bool_condition`.
/// - a cast of a value that is not an `int` or a `float`, which spec section 24 makes an error, in `UnaryPrefix`'s
///   `analyze`.
/// - an `exit` argument that is not an `int`, which spec section 8 makes an error, in `ExitConstruct`'s `analyze`.
///   `check_slice` refuses a string literal or template argument, because the engine runs `.sharp` files without the
///   analyzer.
/// - a call that resolves to a namespaced function, in `report_namespaced_function_call`.
/// - a spread of a value that is not a list, which spec section 7 spreads, in `report_non_list_spread`.
/// - a named argument that names a variadic parameter or no parameter, on a method or a plain PHP function that
///   takes any number of arguments too, which spec section 16 makes an error, in `analyze_invocation`.
/// - a default or an initial value that does not fit its declared type, such as `null` for an `int` or `false` for
///   an `int|string`, which the engine refuses, in `check_parameter_default_value` and the property's
///   `analyze_default_value`. A nullable type, as in `int? total = null`, holds `null`.
/// - a constructor of any descendant of a class whose constructor is `required` that changes its parent
///   constructor's parameters or adds one without a default, which spec section 25 makes an error, in
///   `validate_method_signature_compatibility`, as PHP's `@consistent-constructor` does for a child.
/// - an override of a method with a variadic parameter that is not variadic, which spec section 7 makes an error and
///   the engine refuses, in `validate_method_signature_compatibility`.
/// - a full name in code, such as `App.Shared.Money.of(1)`, which spec section 23 keeps in `import` lines, in
///   `report_full_name`. One file cannot tell it from a class and its member, such as `Status.Active`.
#[inline]
pub fn check_slice(node: Node<'_, '_>, context: &mut Context<'_, '_, '_>) {
    let place = match context.slice_places.last() {
        None => enter(node, Place::File, context),
        Some(Some(place)) => enter(node, *place, context),
        Some(None) => None,
    };

    context.slice_places.push(place);
}

/// Where a node of a PHP# file sits, for the parts of the slice that depend on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// The file's statements, its namespace and its imports.
    File,
    /// A class and its members.
    Class,
    /// An interface and its members.
    Interface,
    /// An interface method's parameters and return type.
    Signature,
    /// An enum and its members.
    Enum,
    /// The types of a class, interface or enum header, written after `:`.
    Header,
    /// The type parameters of a class, an interface or a method: each one's variance, name and bound.
    TypeParameter,
    /// A field or a property, both of which the CST calls a property: its modifiers, type and name.
    FieldOrProperty,
    /// A class constant: its modifiers, type and name.
    ClassConstant,
    /// A method's modifiers, parameters and return type.
    Method,
    /// One parameter.
    Parameter,
    /// A method body.
    Body,
    /// A lambda's parameters, whose body is the method body's.
    Lambda,
    /// `new` and the class it creates, whose arguments are the method body's.
    Instantiation,
    /// A call of a global function and the function's name, whose arguments are the method body's.
    FunctionCall,
    /// A catch clause, whose block is the method body's.
    TryCatchClause,
    /// `super.method(...)`, whose method and arguments are the method body's.
    SuperCall,
    /// `Self.method(...)`, whose method and arguments are the method body's.
    SelfCall,
    /// An expression of a method body reported over one of its parts, at this span: the walk skips that part and
    /// checks the rest as the method body, such as the value of a write whose target the slice refuses.
    RefusedPart(Span),
    /// An attribute list: its attributes' names and arguments.
    Attribute,
    /// A constant expression: a parameter default, an attribute argument, a class constant's value or an enum case's
    /// value.
    Constant,
    /// A pattern of `is` or of a `match` arm, whose values are the method body's.
    Pattern,
}

/// Decides one node at its place. Returns the place of its children when the slice has the node, and `None` when
/// the node is reported. A refusal skips the node's children only when its span covers them: a node refused at a
/// part, such as a method at its name or a cast at its operator, keeps the place it would have had, so the walk still
/// checks the rest.
fn enter(node: Node<'_, '_>, place: Place, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    use Place::Attribute;
    use Place::Body;
    use Place::Class;
    use Place::ClassConstant;
    use Place::Constant;
    use Place::Enum;
    use Place::FieldOrProperty;
    use Place::File;
    use Place::FunctionCall;
    use Place::Header;
    use Place::Instantiation;
    use Place::Interface;
    use Place::Lambda;
    use Place::Method;
    use Place::Parameter;
    use Place::SelfCall;
    use Place::Signature;
    use Place::SuperCall;
    use Place::TryCatchClause;

    if let Place::RefusedPart(part) = place {
        return if node.span() == part { None } else { enter(node, Body, context) };
    }

    if let Some(target) = write_target(node) {
        // PHP# never has `$` variables, and the walk reports a write to one at the variable, and a superglobal at its
        // name.
        if !matches!(target, Expression::Variable(_))
            && !is_slice_target(target, context)
            && superglobal_span(target, context).is_none()
        {
            report_not_supported(
                target.span(),
                "write target",
                "PHP# writes to a local, a parameter or a member written `object.name`.",
                context,
            );

            return Some(Place::RefusedPart(target.span()));
        }

        if let Expression::Access(Access::Property(property)) = target {
            check_member_access(property.object, &property.property, context);
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

            Some(File)
        }
        (Node::Hint(Hint::Identifier(name)), _) if let Some(class) = static_member_class(name, context) => {
            context.report(
                Issue::error(format!(
                    "A static member can't use `{}`, because every `{}<…>` shares it.",
                    BytesDisplay(name.value()),
                    BytesDisplay(class.name.value)
                ))
                .with_annotation(Annotation::primary(name.span()).with_message("Used in a static member.")),
            );

            None
        }
        (Node::Keyword(_) | Node::LocalIdentifier(_) | Node::Identifier(Identifier::Local(_)), _) => Some(place),
        (Node::Terminator(Terminator::Semicolon(_)), _) => Some(place),

        (
            Node::Program(_)
            | Node::Statement(
                Statement::Namespace(_)
                | Statement::Use(_)
                | Statement::Class(_)
                | Statement::Interface(_)
                | Statement::Enum(_),
            )
            | Node::Namespace(_)
            | Node::NamespaceBody(_)
            | Node::NamespaceImplicitBody(_)
            // A braced namespace's body, after its namespace was refused.
            | Node::Block(_)
            | Node::Identifier(Identifier::Dotted(_))
            | Node::DottedIdentifier(_)
            | Node::Use(_)
            | Node::UseItems(UseItems::Sequence(_))
            | Node::UseItemSequence(_)
            | Node::UseItem(_),
            File,
        ) => Some(File),
        (Node::Class(class), File) => {
            report_methods_named_as_properties(
                ClassLike { name: &class.name, span: class.span(), members: &class.members },
                context,
            );

            Some(Class)
        }
        // The walk checks a class's and an enum's name in `check_class_name`.
        (Node::Interface(interface), File) => {
            check_declared_name(interface.name.value, interface.name.span, context);

            Some(Interface)
        }
        (Node::Enum(_), File) => Some(Enum),
        (Node::FunctionLikeParameterList(parameters), Method | Signature) => {
            report_optional_before_required(parameters, context);

            Some(place)
        }
        // `check_class` reports `protected` and `private` on a class, and PHP's own check `abstract` with `final`.
        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Abstract(_)
                | Modifier::Final(_),
            ),
            Class,
        ) => Some(Class),
        (Node::Inheritance(inheritance), Class | Interface | Enum) => {
            check_header(inheritance, place, context);

            Some(Header)
        }
        // A header names each class and interface by its short name, and a generic one with type arguments, which are
        // a parameter's types.
        (Node::Hint(Hint::Identifier(name)), Header) if !is_type_parameter(name, context) => Some(Header),
        (Node::Hint(hint @ Hint::Generic(generic)), Header)
            if is_slice_type(hint) && built_in_generic_arity(generic.name.value).is_none() =>
        {
            Some(Parameter)
        }
        (Node::TypeParameterList(list), Class | Interface | Method | Signature) => {
            check_type_parameters(list, place, context);

            Some(Place::TypeParameter)
        }
        (Node::TypeParameter(_), Place::TypeParameter) => Some(Place::TypeParameter),
        (Node::TypeParameterBound(bound), Place::TypeParameter) => {
            if is_slice_bound(&bound.hint, context) {
                return Some(Place::TypeParameter);
            }

            context.report(
                Issue::error("A bound is a class or an interface, as in `<TItem : DatabaseEntity>`.")
                    .with_annotation(Annotation::primary(bound.hint.span()).with_message("Bound written here."))
                    .with_note("Spec section 11 bounds a type parameter by classes and interfaces, several joined with `&`."),
            );

            None
        }
        // `is_slice_bound` decided the bound, and a generic class's type arguments in it are a parameter's types.
        (Node::Hint(Hint::Identifier(_) | Hint::Intersection(_)) | Node::IntersectionHint(_), Place::TypeParameter) => {
            Some(Place::TypeParameter)
        }
        (Node::Hint(hint @ Hint::Generic(_)), Place::TypeParameter) if is_slice_type(hint) => Some(Parameter),
        (Node::Modifier(Modifier::Public(_)), Interface | Enum) => Some(place),
        (Node::ClassLikeMember(ClassLikeMember::Method(_)), Interface) => Some(Interface),
        (Node::Modifier(modifier), Signature) => {
            context.report(
                Issue::error("An interface method takes no modifier: every interface method is public.")
                    .with_annotation(Annotation::primary(modifier.span()).with_message("Remove this modifier.")),
            );

            None
        }
        (Node::Method(method), Interface) => match is_slice_signature(method) {
            Ok(()) => {
                check_declared_name(method.name.value, method.name.span, context);

                Some(Signature)
            }
            Err((message, help)) => report_refusal(
                Issue::error(message)
                    .with_annotation(Annotation::primary(method.name.span).with_message("Not supported yet."))
                    .with_note(help),
                method.span(),
                Signature,
                context,
            ),
        },
        // A body's block is not in the place, so `MethodBody` admits only `MethodAbstractBody`.
        (
            Node::FunctionLikeReturnTypeHint(_) | Node::MethodBody(_) | Node::MethodAbstractBody(_),
            Signature,
        ) => Some(Signature),
        (Node::AttributeList(_), Class | Enum | Method | FieldOrProperty | Parameter) => Some(Attribute),
        (
            Node::Attribute(_)
            | Node::PartialArgumentList(_)
            | Node::PartialArgument(PartialArgument::Positional(_) | PartialArgument::Named(_)),
            Attribute,
        ) => Some(Attribute),
        // PHP refuses a spread in attribute arguments, and `check_attribute_list` reports it.
        (Node::PositionalArgument(argument), Attribute) => argument.ellipsis.is_none().then_some(Constant),
        (Node::NamedArgument(_), Attribute) => Some(Constant),

        (Node::ClassLikeMember(ClassLikeMember::Method(_)), Class | Enum) => Some(place),
        // `check_enum` reports a property in an enum, and a backing type other than `int` or `string`.
        (Node::ClassLikeMember(ClassLikeMember::Property(_)) | Node::EnumBackingTypeHint(_), Enum) => None,
        (Node::EnumCase(case), Enum) if case.item.name().value.eq_ignore_ascii_case(b"class") => {
            context.report(
                Issue::error("An enum case cannot be named `class`: PHP reserves `class` for the class name.")
                    .with_annotation(Annotation::primary(case.item.name().span).with_message("Declared here.")),
            );

            Some(Enum)
        }
        (Node::EnumCase(case), Enum) => {
            check_declared_name(case.item.name().value, case.item.name().span, context);

            Some(Enum)
        }
        (
            Node::ClassLikeMember(ClassLikeMember::EnumCase(_)) | Node::EnumCaseItem(_) | Node::EnumCaseUnitItem(_),
            Enum,
        ) => Some(Enum),
        (Node::EnumCaseBackedItem(_), Enum) => Some(Constant),
        (Node::ClassLikeMember(ClassLikeMember::Property(property)), Class) => {
            match is_slice_property(property, &context.version, context.names) {
                Ok(()) => {
                    let variable = property.first_variable();
                    check_declared_name(variable.name, variable.span, context);

                    Some(FieldOrProperty)
                }
                // A refused initial value is skipped with its property, because the walk would check it as code and
                // name other fixes, such as setting the member in the constructor.
                Err(issue) => report_refusal(
                    *issue,
                    property.initial_value().map_or(property.span(), |value| value.span()),
                    FieldOrProperty,
                    context,
                ),
            }
        }
        (Node::ClassLikeMember(ClassLikeMember::Constant(constant)), Class | Enum) => match is_slice_constant(constant) {
            Ok(()) => {
                let name = &constant.first_item().name;
                check_declared_name(name.value, name.span, context);

                Some(ClassConstant)
            }
            Err(issue) => report_refusal(*issue, constant.span(), ClassConstant, context),
        },
        (
            Node::ClassLikeConstant(_)
            | Node::Modifier(Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)),
            ClassConstant,
        ) => Some(ClassConstant),
        (Node::ClassLikeConstantItem(_), ClassConstant) => Some(Constant),
        (Node::HookedProperty(property), FieldOrProperty) => {
            let variable = property.item.variable();
            check_accessors(&property.modifiers, &property.hook_list, variable.span, context);
            check_accessor_bodies(&property.hook_list, variable.name, context);
            if let Some(initial_value) = &property.initial_value
                && !context.names.has_storage(&property.hook_list)
            {
                report_no_storage(variable, initial_value.value.span(), "its initial value", context);
            }

            Some(FieldOrProperty)
        }
        // `check_accessors` reported each accessor outside the slice. An accessor body is a method body.
        (Node::PropertyHookList(_), FieldOrProperty | Parameter) => Some(place),
        (Node::PropertyHook(accessor), FieldOrProperty | Parameter) => is_slice_accessor(accessor).then_some(place),
        (
            Node::PropertyHookBody(_) | Node::PropertyHookAbstractBody(_) | Node::PropertyHookConcreteBody(_),
            FieldOrProperty | Parameter,
        ) => Some(place),
        // A computed property's expression runs on each read, as a method body does.
        (Node::ComputedProperty(_), FieldOrProperty) => Some(FieldOrProperty),
        (Node::Block(_) | Node::PropertyHookConcreteExpressionBody(_), FieldOrProperty | Parameter) => Some(Body),
        // A constant initial value is the member's default, which PHP evaluates as a constant expression.
        (Node::Expression(value), FieldOrProperty) => {
            let uses_this = report_this_in_initial_value(node, context);
            let place = if !uses_this && value.is_constant(&context.version, false) { Constant } else { Body };

            enter(node, place, context)
        }
        (
            Node::Property(_)
            | Node::PlainProperty(_)
            | Node::PropertyItem(_)
            | Node::PropertyAbstractItem(_)
            | Node::PropertyConcreteItem(_)
            | Node::DirectVariable(_)
            // `is_slice_property` refuses `override` on a member that is not a field.
            | Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Static(_)
                | Modifier::Override(_),
            ),
            FieldOrProperty,
        ) => Some(FieldOrProperty),
        (Node::Hint(Hint::Mixed(mixed)), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body)
            if mixed.value != ANY =>
        {
            context.report(
                Issue::error("PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.")
                    .with_annotation(Annotation::primary(mixed.span).with_message("Written here."))
                    .with_note("Spec section 24 removes PHP's `mixed`: `Any` holds a value of any type but null, and `Any?` also allows null."),
            );

            None
        }
        (Node::GenericHint(generic), FieldOrProperty | Method | Signature | Parameter | Body) => {
            check_generic(generic, place, context)
        }
        // A generic type's type arguments are checked where the generic type is, which `is_slice_type` checked for
        // `void`. Those of `new` and of a method call are a method body's types, which are never `void` there either.
        (
            Node::TypeArgumentList(list),
            FieldOrProperty | Method | Signature | Parameter | Body | Instantiation,
        ) => {
            if let Some(void) = list.arguments.iter().find(|argument| matches!(argument, Hint::Void(_))) {
                context.report(
                    Issue::error("A type argument cannot be `void`: `void` is only a return type.")
                        .with_annotation(Annotation::primary(void.span()).with_message("Written here.")),
                );

                return None;
            }

            Some(if place == Instantiation { Body } else { place })
        }
        // A function type's parts are checked as a parameter's type is, so its return type may be `void`, which
        // `is_slice_type` refuses for its parameters, and `Self` in it is an error, because `Self` is a method's return
        // type only.
        (Node::FunctionHint(_), FieldOrProperty | Method | Parameter | Body) => Some(Parameter),
        (Node::Method(method), Enum)
            if method.return_type_hint.is_none()
                && enclosing_class(context.program, method.span())
                    .is_some_and(|r#enum| r#enum.name.value.eq_ignore_ascii_case(method.name.value)) =>
        {
            report_refusal(
                Issue::error("An enum has no constructor: its cases are its only values.")
                    .with_annotation(
                        Annotation::primary(method.name.span).with_message("Declared without a return type here."),
                    )
                    .with_help("Give the method a return type, as in `public string label()`."),
                method.span(),
                Method,
                context,
            )
        }
        (Node::Method(method), Class | Enum) => match is_slice_method(method, context.program) {
            Ok(()) => {
                // The constructor, the one method without a return type, carries its class's name, which
                // `check_class_name` reports.
                if method.return_type_hint.is_some() {
                    check_declared_name(method.name.value, method.name.span, context);
                }

                Some(Method)
            }
            Err((message, help)) => report_refusal(
                Issue::error(message)
                    .with_annotation(Annotation::primary(method.name.span).with_message("Not supported yet."))
                    .with_note(help),
                method.span(),
                Method,
                context,
            ),
        },

        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Static(_)
                | Modifier::Abstract(_)
                | Modifier::Virtual(_)
                | Modifier::Override(_)
                // The parser keeps `required` on the constructor only.
                | Modifier::Required(_),
            )
            | Node::FunctionLikeReturnTypeHint(_)
            | Node::MethodBody(_)
            // PHP's own check reports a method without a body that is not `abstract`, and one with a body that is.
            | Node::MethodAbstractBody(_),
            Method,
        ) => Some(Method),
        // Outside a constructor, PHP's own check reports a parameter with an access modifier.
        (Node::FunctionLikeParameter(parameter), Method)
            if parameter.hooks.is_none()
                && let Some(public) = parameter.modifiers.get_public()
                && is_constructor_parameter(parameter, context.program) =>
        {
            report_refusal(
                Issue::error(
                    "A `public` constructor parameter needs accessors: a public member is a property, as in `public int id { get; }`.",
                )
                .with_annotation(Annotation::primary(public.span()).with_message("Declared `public` here."))
                .with_note("Spec section 9 makes a `public` parameter without accessors an error, as a public field is."),
                parameter.span(),
                Parameter,
                context,
            )
        }
        (Node::FunctionLikeParameter(parameter), Method | Signature | Lambda)
            if let Some(ampersand) = parameter.ampersand =>
        {
            report_refusal(
                Issue::error("A by-reference parameter is not supported yet in PHP#.")
                    .with_annotation(Annotation::primary(ampersand).with_message("Not supported yet."))
                    .with_note("The engine passes every PHP# argument by value."),
                parameter.span(),
                Parameter,
                context,
            )
        }
        (Node::FunctionLikeParameter(parameter), Method | Signature | Lambda) => {
            if let Some(accessors) = &parameter.hooks
                && parameter.modifiers.contains_visibility()
            {
                check_accessors(&parameter.modifiers, accessors, parameter.variable.span, context);
                check_accessor_bodies(accessors, parameter.variable.name, context);
                if !context.names.has_storage(accessors) {
                    report_no_storage(&parameter.variable, parameter.variable.span, "the constructor to set", context);
                }
            }

            Some(Parameter)
        }
        (Node::Hint(Hint::Void(_)), Body) => {
            context.report(
                Issue::error("A local cannot be `void`: `void` is only a return type.")
                    .with_annotation(Annotation::primary(node.span()).with_message("Declared `void` here.")),
            );

            None
        }
        (Node::Hint(Hint::Self_(keyword)), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            check_self_type(keyword, place, context).then_some(place)
        }
        (Node::Hint(Hint::Static(keyword)), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            report_php_static(keyword, context);

            None
        }
        // `check_hint` reports a type in parentheses that is not a union.
        (
            Node::Hint(Hint::Nullable(NullableHint { hint: Hint::Parenthesized(parenthesized), .. })),
            FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body,
        ) => {
            if let Hint::Union(union) = parenthesized.hint {
                check_union(union, place, context);
            }

            None
        }
        (Node::Hint(hint), FieldOrProperty | Method | Signature | Parameter | Body) if is_slice_type(hint) => {
            Some(place)
        }
        (Node::Hint(Hint::Union(union)), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            check_union(union, place, context);

            None
        }
        (Node::Hint(hint), ClassConstant) if is_slice_type(hint) && !matches!(hint, Hint::Void(_)) => {
            Some(ClassConstant)
        }
        (Node::NullableHint(_), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            Some(place)
        }
        (Node::DirectVariable(_), Parameter) => Some(Parameter),
        // An access modifier declares a member, and `check_accessors` checked its accessors. PHP reports `static`,
        // `final` and `abstract` on a parameter, a member declared outside the constructor, and accessors without one.
        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Static(_)
                | Modifier::Final(_)
                | Modifier::Abstract(_),
            ),
            Parameter,
        ) => Some(Parameter),
        (Node::FunctionLikeParameterDefaultValue(default), Parameter)
            if let Expression::Call(Call::Method(call)) = default.value
                && let Some(class) = context.names.static_call_class(call)
                && context.names.get(&class.name).eq_ignore_ascii_case(b"Sharp\\Position")
                && let ClassLikeMemberSelector::Identifier(method) = &call.method
                && method.value.eq_ignore_ascii_case(b"current") =>
        {
            context.report(
                Issue::error("A `Position.current()` default is not supported yet in PHP#.")
                    .with_annotation(Annotation::primary(call.span()).with_message("Not supported yet."))
                    .with_note(
                        "As a parameter's default, `Position.current()` gives the caller's position, which waits for typed compilation.",
                    ),
            );

            None
        }
        (Node::FunctionLikeParameterDefaultValue(_), Parameter) => Some(Constant),
        (Node::Block(_), Method | Body) => Some(Body),
        (Node::MethodExpressionBody(_), Method) => Some(Body),
        (Node::TryCatchClause(_), Body) => Some(TryCatchClause),
        (Node::Hint(hint), TryCatchClause) if let Some(erased) = context.names.erased_type(hint) => {
            report_not_supported(erased, "type", ERASED_TYPE_ARGUMENTS, context);

            None
        }
        // `check_try` reports a catch type that is not a class, and `check_try_catch_clause` the variable's name.
        (Node::Hint(_) | Node::DirectVariable(_), TryCatchClause) => None,
        (Node::Block(_), TryCatchClause) => Some(Body),

        (Node::Statement(statement), Body) if !has_braces(statement) => {
            report_not_supported(
                statement.span(),
                "statement without braces",
                "PHP# writes the body of `if`, `else` and each loop as a block in braces.",
                context,
            );

            None
        }
        (
            Node::Statement(
                Statement::Block(_)
                | Statement::Expression(_)
                | Statement::Return(_)
                | Statement::LocalDeclaration(_)
                | Statement::If(_)
                | Statement::While(_)
                | Statement::DoWhile(_)
                | Statement::For(_)
                | Statement::ForOf(_)
                | Statement::Try(_)
                | Statement::Break(Break { level: None, .. })
                | Statement::Continue(Continue { level: None, .. }),
            )
            | Node::Try(_)
            | Node::TryFinallyClause(_)
            | Node::Break(_)
            | Node::Continue(_)
            | Node::ExpressionStatement(_)
            | Node::Return(_)
            | Node::LocalDeclaration(_)
            | Node::If(_)
            | Node::IfBody(IfBody::Statement(_))
            | Node::IfStatementBody(_)
            | Node::IfStatementBodyElseClause(_)
            | Node::While(_)
            | Node::WhileBody(WhileBody::Statement(_))
            | Node::DoWhile(_)
            | Node::For(_)
            | Node::ForBody(ForBody::Statement(_))
            | Node::ForOf(_)
            | Node::ForOfTarget(_)
            | Node::ForOfKeyValueTarget(_)
            | Node::ForOfVariable(_),
            Body,
        ) => Some(Body),
        // Spec section 8 makes output and `exit` function calls, and keeps `exit(code)` as PHP 8.4's built-in
        // function. The analyzer refuses an `exit` argument that is not an `int`.
        (Node::Statement(Statement::Echo(_)) | Node::Expression(Expression::Construct(Construct::Print(_))), Body) => {
            let form: &[u8] = if matches!(node, Node::Statement(_)) { b"echo" } else { b"print" };
            report_removed(
                node.span(),
                form,
                "write `printf` or `fwrite`.",
                "Output is a function call in PHP#, as spec section 8 writes it.",
                context,
            );

            None
        }
        (Node::Expression(Expression::Construct(Construct::Die(_))), Body) => {
            report_die(node.span(), context);

            None
        }
        (Node::Expression(Expression::Construct(Construct::Exit(exit))), Body) if exit.arguments.is_none() => {
            context.report(
                Issue::error("PHP# calls `exit` as a function: write `exit(0)`.")
                    .with_annotation(Annotation::primary(exit.exit.span).with_message("Written here."))
                    .with_note("`exit` is PHP 8.4's built-in function in PHP#, as spec section 8 writes it."),
            );

            None
        }
        // The engine runs a `.sharp` file without the analyzer, so a string literal or template argument, in any
        // number of parentheses, is refused here.
        (Node::Expression(Expression::Construct(Construct::Exit(exit))), Body)
            if exit.arguments.as_ref().and_then(|arguments| arguments.arguments.first()).is_some_and(|argument| {
                let mut value = argument.value();
                while let Expression::Parenthesized(parenthesized) = value {
                    value = parenthesized.expression;
                }

                matches!(value, Expression::Literal(Literal::String(_)) | Expression::CompositeString(_))
            }) =>
        {
            report_die(node.span(), context);

            None
        }
        (
            Node::Expression(Expression::Construct(Construct::Exit(_)))
            | Node::Construct(Construct::Exit(_))
            | Node::ExitConstruct(_),
            Body,
        ) => Some(Body),

        (Node::Statement(Statement::PatternMatch(pattern_match)), Body) => {
            check_pattern_match(pattern_match, false, context).then_some(Body)
        }
        (Node::Expression(Expression::PatternMatch(pattern_match)), Body) => {
            check_pattern_match(pattern_match, true, context).then_some(Body)
        }
        (Node::Is(is), Body) => {
            // `is not T name` declares `name` where the test is false, so the outermost `not` hides no variable.
            let tested = if let Pattern::Not(not) = is.pattern { not.pattern } else { is.pattern };
            report_hidden_pattern_variables(tested, false, context);

            Some(Body)
        }
        (Node::PatternMatchPatternArm(arm), Body) => {
            report_hidden_pattern_variables(arm.pattern, false, context);

            Some(Body)
        }
        (Node::As(r#as), Body) => match r#as.hint {
            Hint::Nullable(_) | Hint::Void(_) => {
                context.report(
                    Issue::error("`as` converts to a type that is not nullable or `void`.")
                        .with_annotation(Annotation::primary(r#as.hint.span()).with_message("Written here."))
                        .with_note("`as T` already gives `T?`: the value as a `T`, or null when it is not one, as spec section 21 says."),
                );

                None
            }
            hint => match context.names.erased_type(hint) {
                Some(erased) => {
                    report_not_supported(erased, "type", ERASED_TYPE_ARGUMENTS, context);

                    None
                }
                None => Some(Body),
            },
        },
        (
            Node::Expression(Expression::Is(_) | Expression::As(_))
            | Node::PatternMatch(_)
            | Node::PatternMatchArm(_)
            | Node::PatternMatchDefaultArm(_)
            | Node::MatchGuard(_)
            | Node::PatternMatchArmBody(_),
            Body,
        ) => Some(Body),
        (Node::Pattern(_), Body | Place::Pattern) => Some(Place::Pattern),
        (Node::Hint(Hint::Nullable(_)), Place::Pattern) => {
            context.report(
                Issue::error("A type pattern is never nullable: null never matches a type.")
                    .with_annotation(Annotation::primary(node.span()).with_message("Written here."))
                    .with_help("Test for null with `x == null`, or join both with `or`, as in `x is int or null`."),
            );

            None
        }
        (Node::Hint(hint), Place::Pattern) if let Some(erased) = context.names.erased_type(hint) => {
            report_not_supported(erased, "type", ERASED_TYPE_ARGUMENTS, context);

            None
        }
        (Node::Hint(hint), Place::Pattern) if is_slice_type(hint) && !matches!(hint, Hint::Void(_)) => {
            Some(Place::Pattern)
        }
        (
            Node::TypePattern(_)
            | Node::ComparisonPattern(_)
            | Node::NotPattern(_)
            | Node::BinaryPattern(_)
            | Node::ParenthesizedPattern(_)
            | Node::PropertiesPattern(_)
            | Node::PropertyPattern(_)
            | Node::BinaryOperator(_),
            Place::Pattern,
        ) => Some(Place::Pattern),
        // A value or a comparison's value is a method body's expression.
        (Node::Expression(_), Place::Pattern) => enter(node, Body, context),

        // PHP refuses the file at compile time, so the engine would too.
        (Node::LiteralString(string), Body | Constant) if string.value.is_none() => {
            context.report(
                Issue::error("Invalid UTF-8 codepoint escape sequence.")
                    .with_annotation(Annotation::primary(string.span).with_message("Escape written here."))
                    .with_note("A `\\u{...}` escape holds hex digits for a codepoint up to `10FFFF`."),
            );

            None
        }
        // Spec sections 27 and 29 remove PHP's superglobals and its `__Something__` names, written bare as well. A
        // method body walks an index of a superglobal and reports the superglobal at its name. A constant expression
        // has no index, so it reports the superglobal over the whole index.
        (Node::Expression(expression @ (Expression::ConstantAccess(_) | Expression::Variable(_))), Body | Constant)
        | (Node::Expression(expression @ Expression::ArrayAccess(_)), Constant)
            if let Some(span) = superglobal_span(expression, context) =>
        {
            report_superglobal(span, context);

            None
        }
        (Node::Expression(expression @ Expression::ConstantAccess(_)), Body | Constant)
            if let Some(name) = magic_name(expression, context) =>
        {
            report_magic_constant(name.value, name.span, None, context);

            None
        }
        (Node::Expression(Expression::MagicConstant(constant)), Body | Constant) => {
            report_magic_constant(constant.value().value, constant.value().span, Some(constant), context);

            None
        }
        (Node::UnaryPrefix(unary_prefix), Body | Constant) if unary_prefix.operator.is_cast() => {
            check_cast(unary_prefix, place, context);

            Some(place)
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
            Body | Constant,
        ) => Some(place),
        (
            Node::Expression(Expression::Array(_))
            | Node::Array(_)
            | Node::ArrayElement(ArrayElement::Value(_) | ArrayElement::KeyValue(_) | ArrayElement::Variadic(_))
            | Node::ValueArrayElement(_)
            | Node::KeyValueArrayElement(_)
            | Node::VariadicArrayElement(_),
            Body | Constant,
        ) => Some(place),
        (Node::Expression(Expression::ArrayAccess(_)) | Node::ArrayAccess(_), Body) => Some(Body),
        (Node::ConstantAccess(access), Body) if is_type_parameter(&access.name, context) => {
            report_not_supported(access.span(), "static member of a type parameter", ERASED_TYPE_ARGUMENTS, context);

            None
        }
        (Node::ConstantAccess(_), Body) => Some(Body),
        (Node::TypeOf(type_of), Body | Constant) if is_type_parameter(&type_of.class, context) => {
            report_not_supported(type_of.span(), "`typeof` of a type parameter", ERASED_TYPE_ARGUMENTS, context);

            None
        }
        // `typeof(X)` is `X::class`, which PHP takes as a constant expression too.
        (Node::Expression(Expression::TypeOf(_)) | Node::TypeOf(_), Body | Constant) => Some(place),
        (Node::ConstantAccess(constant), Constant)
            if context.names.binding(&constant.name) == Some(Binding::Constant) =>
        {
            Some(Constant)
        }
        // The object decides a member of PHP's `self` or `static`, and of a superglobal or a `__Something__` name,
        // which binds as a class before `.`.
        (Node::Expression(Expression::Access(Access::Property(access))), Constant)
            if matches!(access.object, Expression::Self_(_) | Expression::Static(_))
                || superglobal_span(access.object, context).is_some()
                || magic_name(access.object, context).is_some() =>
        {
            enter(Node::Expression(access.object), Constant, context)
        }
        // The engine reads `Class.name` in a constant expression as the class constant or enum case, since PHP
        // cannot read a static property there. The analyzer reports a static property of a class this file does
        // not declare.
        (Node::Expression(Expression::Access(Access::Property(access))), Constant)
            if let Some(class) = context.names.static_property_class(access)
                && let ClassLikeMemberSelector::Identifier(member) = &access.property =>
        {
            report_static_member_in_constant(class, member, access.span(), context);

            None
        }
        (Node::Expression(Expression::Conditional(_)), Body) => Some(Body),
        (Node::Conditional(conditional), Body) => Some(check_conditional(conditional, context)),
        // Spec section 14.2: a lambda is a bare arrow, after one name or a parenthesized parameter list, and its body
        // is an expression or a block. The parser reads it into an arrow function or a closure without a keyword.
        (Node::Expression(Expression::ArrowFunction(_) | Expression::Closure(_)), Body) => Some(Body),
        (Node::ArrowFunction(_) | Node::Closure(_), Body) => {
            check_lambda_captures(node, context);

            Some(Lambda)
        }
        (Node::FunctionLikeParameterList(_), Lambda) => Some(Lambda),
        (Node::Block(_), Lambda) => Some(Body),
        (Node::Expression(_), Lambda) => enter(node, Body, context),
        (Node::BinaryOperator(operator), Body | Constant) if is_slice_binary_operator(operator) => Some(place),
        // `check_cast` decided the cast at its `UnaryPrefix`.
        (Node::UnaryPrefixOperator(operator), Body | Constant) if operator.is_cast() => Some(place),
        (Node::UnaryPrefixOperator(operator), Body | Constant) if is_slice_prefix_operator(operator, place) => {
            Some(place)
        }

        (
            Node::Expression(
                Expression::Assignment(_)
                | Expression::UnaryPostfix(_)
                | Expression::Call(Call::Method(_) | Call::NullSafeMethod(_))
                | Expression::Access(Access::Property(_) | Access::NullSafeProperty(_))
                | Expression::Throw(_)
                // The parser reads a template, and only a template, into an interpolated string.
                | Expression::CompositeString(CompositeString::Interpolated(_)),
            )
            | Node::Throw(_)
            | Node::CompositeString(CompositeString::Interpolated(_))
            | Node::InterpolatedString(_)
            | Node::StringPart(StringPart::Literal(_) | StringPart::BracedExpression(_))
            | Node::LiteralStringPart(_)
            | Node::BracedExpressionStringPart(_)
            | Node::Assignment(_)
            | Node::UnaryPostfix(_)
            | Node::UnaryPostfixOperator(UnaryPostfixOperator::PostIncrement(_) | UnaryPostfixOperator::PostDecrement(_))
            | Node::Call(Call::Method(_) | Call::NullSafeMethod(_))
            | Node::Access(Access::Property(_) | Access::NullSafeProperty(_))
            | Node::ClassLikeMemberSelector(ClassLikeMemberSelector::Identifier(_))
            | Node::ArgumentList(_)
            | Node::Argument(_)
            | Node::NamedArgument(_),
            Body,
        ) => Some(Body),
        (Node::Expression(Expression::Instantiation(instantiation)), Body)
            if matches!(
                instantiation.class,
                Expression::Identifier(Identifier::Local(_)) | Expression::Self_(_) | Expression::Static(_)
            ) =>
        {
            check_instantiation(instantiation, context)
        }
        // `check_instantiation` reported PHP's `self` and `static`.
        (
            Node::Instantiation(_)
            | Node::Expression(
                Expression::Identifier(Identifier::Local(_)) | Expression::Self_(_) | Expression::Static(_),
            ),
            Instantiation,
        ) => Some(Instantiation),
        (Node::ArgumentList(_), Instantiation) => Some(Body),
        (Node::AssignmentOperator(operator), Body) if is_slice_assignment_operator(operator) => Some(Body),
        (Node::PositionalArgument(_), Body) => Some(Body),
        // `super.label()` calls the parent's method, spec section 22. `super` is no value of its own.
        (Node::MethodCall(call), Body) if matches!(call.object, Expression::Parent(_)) => Some(SuperCall),
        // `Self.make()` calls a static method of the class a static method is called on, spec section 25.
        (Node::MethodCall(call), Body) if let Expression::Self_(keyword) = call.object => {
            if !is_sharp_self(keyword) {
                report_php_self(keyword, true, context);
            }

            Some(SelfCall)
        }
        (Node::Expression(Expression::Parent(_)), SuperCall) | (Node::Expression(Expression::Self_(_)), SelfCall) => None,
        (Node::ClassLikeMemberSelector(_) | Node::TypeArgumentList(_) | Node::ArgumentList(_), SuperCall | SelfCall) => {
            enter(node, Body, context)
        }
        (Node::MethodCall(call), Body) => {
            check_member_access(call.object, &call.method, context);

            Some(Body)
        }
        (Node::PropertyAccess(access), Body) => {
            check_member_access(access.object, &access.property, context);

            Some(Body)
        }
        (Node::NullSafeMethodCall(call), Body) => check_null_safe_object(call.span(), call.object, &call.method, context),
        (Node::NullSafePropertyAccess(access), Body) => {
            check_null_safe_object(access.span(), access.object, &access.property, context)
        }

        // PHP# never has `$` variables. A `$` variable inside a construct the walk refuses adds no second error.
        (Node::Expression(Expression::Variable(variable)), Body | Constant) => {
            check_variable(variable, context);

            None
        }
        // Spec section 25 removes PHP's `self` and `static`. `Self` is no value of its own.
        (Node::Expression(Expression::Self_(keyword)), Body | Constant) if !is_sharp_self(keyword) => {
            report_php_self(keyword, false, context);

            None
        }
        (Node::Expression(Expression::Static(keyword)), Body | Constant) => {
            report_php_static(keyword, context);

            None
        }
        // PHP# never has top-level functions, `global`, `compact()`, `extract()` or a member called without `this.`:
        // `check_function`, `check_global` and `check_function_call` report them. A function is reported at its name,
        // so its parameters and body are checked as a method's. `global` holds only the names it declares, and
        // `compact()` and `extract()` are reported over the whole call.
        (Node::Statement(Statement::Function(_)), File) => Some(File),
        (Node::Function(_), File) => Some(Method),
        (Node::Statement(Statement::Global(_)), Body) => None,
        (Node::Expression(Expression::Call(Call::Function(function_call))), Body)
            if let Expression::Identifier(name) = function_call.function
                && is_compact_or_extract(name.last_segment())
                && context.names.binding(name) != Some(Binding::Member) =>
        {
            None
        }
        // Spec sections 8 and 29: plain PHP functions are called by their bare names. The analyzer refuses a call
        // that resolves to a namespaced function. Spec section 14: a local holding a function is called the same way.
        // `check_function_call` reports a member called without `this.` at its name.
        (Node::Expression(Expression::Call(Call::Function(function_call))), Body)
            if let Expression::Identifier(name @ Identifier::Local(_)) = function_call.function
                && matches!(context.names.binding(name), None | Some(Binding::Local(_) | Binding::Member)) =>
        {
            Some(FunctionCall)
        }
        (
            Node::Call(Call::Function(_))
            | Node::FunctionCall(_)
            | Node::Expression(Expression::Identifier(Identifier::Local(_))),
            FunctionCall,
        ) => Some(FunctionCall),
        (Node::ArgumentList(_), FunctionCall) => Some(Body),

        _ => {
            report_unsupported(node, place, context);

            None
        }
    }
}

/// Whether the slice has a method, with the refusal's message and note when it does not. Each rule keeps out a method
/// whose meaning a later slice gives it: PHP's magic methods, PHP#'s `private` default of spec section 5, and a static
/// constructor. The constructor of spec section 9 is the one method without a return type, named after its class.
/// `enter` reports a `__Something__` name, which ends as well as starts with `__`.
fn is_slice_method(method: &Method, program: &Program) -> Result<(), (&'static str, &'static str)> {
    let class_name = enclosing_class(program, method.span()).map(|class| class.name.value);
    let refusal = if method.return_type_hint.is_none() && class_name != Some(method.name.value) {
        (
            "A PHP# method needs a return type: only the constructor, named after its class, has none.",
            "A method is written `public int total()`, and the constructor `public Report()`.",
        )
    } else if method.return_type_hint.is_none() && method.is_static() {
        ("A static constructor is not supported yet in PHP#.", "The main constructor of a PHP# class is not `static`.")
    } else if method.name.value.starts_with(b"__") && !is_magic_name(method.name.value) {
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

/// Whether the slice has an interface method, with the refusal's message and note when it does not. `enter` reports a
/// modifier, a body and a `__Something__` name.
fn is_slice_signature(method: &Method) -> Result<(), (&'static str, &'static str)> {
    if method.return_type_hint.is_none() {
        Err((
            "An interface method needs a return type in PHP#.",
            "An interface method is written `float area();`, with its return type first.",
        ))
    } else if method.name.value.starts_with(b"__") && !is_magic_name(method.name.value) {
        Err((
            "This method name is not supported yet in PHP#.",
            "PHP reserves method names that start with `__` for its magic methods.",
        ))
    } else {
        Ok(())
    }
}

/// Reports what the engine refuses in a header when it declares the class: a name the header already holds, and in an
/// enum, `UnitEnum` or `BackedEnum`, which the engine adds to every enum or every backed one. Names compare as PHP's
/// do, resolved and ignoring case. A generic type counts by its name, and `enter` refuses any type in a header that
/// is not a class or a generic class type, `List`, `Map` and `Class` included, which have no resolved name.
fn check_header(inheritance: &Inheritance, place: Place, context: &mut Context<'_, '_, '_>) {
    let mut named: Vec<&[u8]> = Vec::new();
    for name in inheritance.names() {
        let Some(resolved) = context.names.resolve(&name) else {
            continue;
        };
        let issue = if place == Place::Enum
            && (resolved.eq_ignore_ascii_case(b"UnitEnum") || resolved.eq_ignore_ascii_case(b"BackedEnum"))
        {
            Issue::error(
                "Every enum implements `UnitEnum`, and every backed enum `BackedEnum`, so an enum header never names them.",
            )
            .with_annotation(Annotation::primary(name.span()).with_message("Remove this name."))
        } else if named.iter().any(|earlier| earlier.eq_ignore_ascii_case(resolved)) {
            Issue::error(format!("This header names `{}` twice.", BytesDisplay(name.value())))
                .with_annotation(Annotation::primary(name.span()).with_message("Remove this name."))
        } else {
            named.push(resolved);

            continue;
        };

        context.report(issue.with_note("The engine refuses this header when it declares the class."));
    }
}

/// Reports each optional parameter before the last required one, which PHP makes required with a deprecation. A
/// variadic parameter is not a required one, as in PHP.
fn report_optional_before_required(parameters: &FunctionLikeParameterList, context: &mut Context<'_, '_, '_>) {
    let Some(last_required) = parameters
        .parameters
        .iter()
        .rev()
        .find(|parameter| parameter.default_value.is_none() && parameter.ellipsis.is_none())
    else {
        return;
    };

    for parameter in parameters.parameters.iter().take_while(|parameter| parameter.span() != last_required.span()) {
        if parameter.default_value.is_some() {
            context.report(
                Issue::error(format!(
                    "The optional parameter `{}` comes before the required parameter `{}`: PHP would make it required.",
                    BytesDisplay(parameter.variable.name),
                    BytesDisplay(last_required.variable.name)
                ))
                .with_annotation(Annotation::primary(parameter.span()).with_message("Optional here."))
                .with_help("Move the optional parameter after the required ones, or give the required one a default."),
            );
        }
    }
}

/// Whether the slice has a field or a property, with the refusal when it does not. Spec section 6 makes a field
/// storage, which is `private` or `protected`, and a property the API, whose accessors `check_accessors` decides.
///
/// A static member takes only a constant initial value, which PHP stores as its default, because any other runs in
/// the constructor. A static property has `set`, because PHP has no `readonly` static property.
///
/// A field written `override` replaces a plain PHP parent's property, spec section 6.1. It takes the parent's access
/// level, which may be `public` and which the analyzer checks, and a constant initial value, its new default, because
/// the parent's constructor may read the property before this class's constructor sets any other.
fn is_slice_property(property: &Property, version: &PHPVersion, names: &ResolvedNames) -> Result<(), Box<Issue>> {
    let not_supported = |span: Span, message: &str, note: &str| {
        Issue::error(message)
            .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
            .with_note(note)
    };
    let no_access_modifier = "A PHP# member without an access modifier is `private`, while PHP makes it `public`.";

    if let Some(r#static) = property.modifiers().get_static() {
        if let Property::Hooked(hooked) = property
            && hooked.hook_list.hooks.iter().any(|accessor| matches!(accessor.body, PropertyHookBody::Concrete(_)))
        {
            return Err(Box::new(not_supported(
                r#static.span(),
                "A static property with an accessor body is not supported yet in PHP#.",
                "PHP has no hooks on a static property.",
            )));
        }

        if let Property::Hooked(auto_property) = property
            && auto_property.hook_list.is_get_only()
        {
            return Err(Box::new(not_supported(
                property.first_variable().span,
                "A get-only static property is not supported yet in PHP#.",
                "A get-only property runs as `readonly`, and PHP has no `readonly` static property.",
            )));
        }

        if let Some(value) = property.initial_value()
            && !value.is_constant(version, false)
        {
            return Err(Box::new(not_supported(
                value.span(),
                "A static member's initial value that is not constant is not supported yet in PHP#.",
                "An initial value that is not constant runs in the constructor, which a static member does not wait for.",
            )));
        }
    }

    let is_override = property.modifiers().iter().any(|modifier| matches!(modifier, Modifier::Override(_)));
    if is_override && !matches!(property, Property::Plain(_)) {
        return Err(Box::new(not_supported(
            property.first_variable().span,
            "Overriding a property is not supported yet in PHP#.",
            "`override` takes a field that replaces a plain PHP parent's property, as in `protected override string table = \"orders\";`.",
        )));
    }

    match property {
        Property::Plain(field) => {
            if let Some(public) = field.modifiers.get_public()
                && !is_override
            {
                Err(Box::new(
                    Issue::error("A PHP# field cannot be `public`: a field is `private` or `protected`.")
                        .with_annotation(Annotation::primary(public.span()).with_message("Declared `public` here."))
                        .with_help("Declare a property, as in `public int views { get; set; }`, to make it public."),
                ))
            } else if is_override && property.initial_value().is_none() {
                Err(Box::new(
                    Issue::error(
                        "An override needs an initial value: it replaces the default of the parent's property.",
                    )
                    .with_annotation(
                        Annotation::primary(property.first_variable().span)
                            .with_message("Declared without a value here."),
                    )
                    .with_help("Write the new default, as in `protected override string table = \"orders\";`."),
                ))
            } else if is_override
                && let Some(value) = property.initial_value()
                && !value.is_constant(version, false)
            {
                Err(Box::new(
                    Issue::error(
                        "The initial value of an override must be constant: the parent's constructor may read it before this class's code runs.",
                    )
                    .with_annotation(Annotation::primary(value.span()).with_message("Not constant."))
                    .with_help("Write a constant value, such as a literal or a list of literals."),
                ))
            } else if !field.modifiers.contains_visibility() {
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
            } else if let Some(initial_value) = &auto_property.initial_value
                && !initial_value.value.is_constant(version, false)
                && auto_property.hook_list.hooks.iter().any(|accessor| {
                    accessor.name.value == b"set" && matches!(accessor.body, PropertyHookBody::Concrete(_))
                })
            {
                Err(Box::new(not_supported(
                    initial_value.value.span(),
                    "An initial value that is not constant, on a property whose `set` has a body, is not supported yet in PHP#.",
                    "A constant initial value is the property's default, while any other would run the `set` body at the start of the constructor.",
                )))
            } else if auto_property.initial_value.is_none()
                && matches!(auto_property.hint, Some(Hint::Nullable(_)))
                && auto_property.hook_list.is_get_only()
                && names.has_storage(&auto_property.hook_list)
            {
                Err(Box::new(not_supported(
                    property.first_variable().span,
                    "A get-only nullable property without an initial value is not supported yet in PHP#.",
                    "A get-only property runs as PHP's `readonly`, which takes no default, so it cannot start as null: give it an initial value, as in `public int? total { get; } = null;`, or a `set` accessor.",
                )))
            } else {
                Ok(())
            }
        }
        Property::Computed(computed) => {
            if !computed.modifiers.contains_visibility() {
                Err(Box::new(not_supported(
                    computed.variable.span,
                    "A property without `public`, `protected` or `private` is not supported yet in PHP#.",
                    no_access_modifier,
                )))
            } else if let Some(r#static) = computed.modifiers.get_static() {
                Err(Box::new(not_supported(
                    r#static.span(),
                    "A static computed property is not supported yet in PHP#.",
                    "PHP has no hooks on a static property, so a computed property is an instance property.",
                )))
            } else {
                Ok(())
            }
        }
    }
}

/// Reports each method named as a property of its class, compared ignoring case as PHP finds a method. Spec section 14
/// calls a property holding a function as `order.priceOf(line)`, and the engine calls the property only when the
/// class has no method of that name, so one name declares one member, as in C#.
fn report_methods_named_as_properties(class: ClassLike, context: &mut Context<'_, '_, '_>) {
    let mut properties: Vec<&DirectVariable> = Vec::new();
    for member in class.members {
        match member {
            ClassLikeMember::Property(property) => properties.extend(property.variables()),
            ClassLikeMember::Method(method) => properties.extend(
                method
                    .parameter_list
                    .parameters
                    .iter()
                    .filter(|parameter| parameter.is_promoted_property())
                    .map(|parameter| &parameter.variable),
            ),
            _ => {}
        }
    }

    for member in class.members {
        let ClassLikeMember::Method(method) = member else {
            continue;
        };

        if let Some(property) = properties.iter().find(|property| property.name.eq_ignore_ascii_case(method.name.value))
        {
            context.report(
                Issue::error(format!(
                    "The class `{}` declares a method and a property named `{}`.",
                    BytesDisplay(class.name.value),
                    BytesDisplay(method.name.value),
                ))
                .with_annotation(Annotation::primary(method.name.span).with_message("The method is declared here."))
                .with_annotation(Annotation::secondary(property.span).with_message("The property is declared here."))
                .with_help("Rename one of them: `x.name(…)` calls the method, or the function the property holds."),
            );
        }
    }
}

/// Whether the slice has a class constant, with the refusal when it does not: an access modifier and one name.
fn is_slice_constant(constant: &ClassLikeConstant) -> Result<(), Box<Issue>> {
    let not_supported = |span: Span, message: &str, note: &str| {
        Box::new(
            Issue::error(message)
                .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
                .with_note(note),
        )
    };

    if constant.items.len() > 1 {
        Err(not_supported(
            constant.span(),
            "A constant declaring several names is not supported yet in PHP#.",
            supported(Place::ClassConstant),
        ))
    } else if !constant.modifiers.contains_visibility() {
        Err(not_supported(
            constant.first_item().name.span,
            "A constant without `public`, `protected` or `private` is not supported yet in PHP#.",
            "A PHP# member without an access modifier is `private`, while PHP makes it `public`.",
        ))
    } else {
        Ok(())
    }
}

/// Checks the accessors of a property, declared in the class body or on a constructor parameter: `get` once, and an
/// optional `set` once, which may take an access modifier narrower than the property's, as in C#. Each is `;`, an
/// expression body `=> expr;` or a block. `init`, an access modifier on `get` and a parameter list on `set` are not
/// supported yet.
fn check_accessors(
    modifiers: &Sequence<Modifier>,
    accessors: &PropertyHookList,
    name: Span,
    context: &mut Context<'_, '_, '_>,
) {
    let reach = modifiers.get_first_read_visibility().map_or(0, visibility_reach);
    let mut declared: Vec<&[u8]> = Vec::new();

    for accessor in &accessors.hooks {
        let name = accessor.name.value;
        if !is_slice_accessor(accessor) {
            report_not_supported(
                accessor.span(),
                "accessor",
                "A PHP# property's accessors are `get` and `set`, each written `;`, `=> expr;` or with a block body, and `set` may take `private` or `protected`.",
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
                .with_annotation(Annotation::primary(name).with_message("Declared here."))
                .with_help("Add `get;`, as in `public int views { get; set; }`."),
        );
    }
}

/// Whether the slice has an accessor: `get` or `set` without attributes, `&` or a parameter list, where only `set`
/// takes an access modifier, `private`, `protected` or `public`, which `check_accessors` compares with the property's.
fn is_slice_accessor(accessor: &PropertyHook) -> bool {
    let name = accessor.name.value;

    accessor.attribute_lists.is_empty()
        && accessor.ampersand.is_none()
        && accessor.parameter_list.is_none()
        && match accessor.modifiers.as_slice() {
            [] => name == b"get" || name == b"set",
            [Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)] => name == b"set",
            _ => false,
        }
}

/// Reports a property without storage that something sets: an initial value, or a constructor that receives it.
fn report_no_storage(property: &DirectVariable, span: Span, set_by: &str, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!(
            "The property `{}` has no storage for {set_by}: give it an auto accessor, such as `get;`, or use `field` in an accessor body.",
            BytesDisplay(property.name)
        ))
        .with_annotation(Annotation::primary(span).with_message("Set here."))
        .with_note("Its accessors compute every read and write, as PHP's virtual property does."),
    );
}

/// Checks the bodies of a property's accessors: the returns PHP's hooks allow, `field` in a lambda, and the property
/// read through `this` inside its own accessor.
fn check_accessor_bodies(accessors: &PropertyHookList, property: &[u8], context: &mut Context<'_, '_, '_>) {
    for accessor in accessors.hooks.iter().filter(|accessor| is_slice_accessor(accessor)) {
        if let PropertyHookBody::Concrete(PropertyHookConcreteBody::Block(block)) = &accessor.body {
            check_accessor_returns(accessor.name.value, block, context);
        }

        check_accessor_node(Node::PropertyHookBody(&accessor.body), property, false, context);
    }
}

/// php-src gives a `get` hook the property's type and a `set` hook `void`, so a `get` block returns a value and a
/// `set` block returns none. A lambda's returns are its own.
fn check_accessor_returns(accessor: &[u8], block: &Block, context: &mut Context<'_, '_, '_>) {
    for r#return in mago_syntax::utils::find_returns_in_block(block) {
        match (accessor, &r#return.value) {
            (b"get", None) => context.report(
                Issue::error("A `get` accessor must return a value.")
                    .with_annotation(Annotation::primary(r#return.span()).with_message("Returns no value."))
                    .with_note("PHP gives a `get` hook the property's type.")
                    .with_help("Return the property's value, such as `return field;`."),
            ),
            (b"set", Some(value)) => context.report(
                Issue::error("A `set` accessor must not return a value.")
                    .with_annotation(Annotation::primary(value.span()).with_message("Returned here."))
                    .with_note("PHP gives a `set` hook the return type `void`.")
                    .with_help("Write the value with `field = value;`, then `return;` if the body ends early."),
            ),
            _ => {}
        }
    }
}

/// A lambda runs as a function of its own, so `$this->name` there calls the accessor again, in PHP as in C#. In the
/// accessor itself PHP reads and writes the storage through `$this->name` and `$this?->name`, where C# calls the
/// accessor again, so the accessor writes `field`, and a lambda cannot.
fn check_accessor_node(node: Node<'_, '_>, property: &[u8], in_lambda: bool, context: &mut Context<'_, '_, '_>) {
    let in_lambda = in_lambda || matches!(node, Node::ArrowFunction(_) | Node::Closure(_));
    let member = match node {
        Node::PropertyAccess(access) => Some((access.object, &access.property)),
        Node::NullSafePropertyAccess(access) => Some((access.object, &access.property)),
        _ => None,
    };

    match node {
        Node::ConstantAccess(access) if in_lambda && context.names.binding(&access.name) == Some(Binding::Field) => {
            context.report(
                Issue::error(
                    "`field` cannot be used in a lambda: PHP would call the accessor again instead of reading the storage.",
                )
                .with_annotation(Annotation::primary(access.span()).with_message("Used here."))
                .with_help("Copy `field` into a local before the lambda, and use the local inside it."),
            );
        }
        _ if !in_lambda
            && let Some((object, selector)) = member
            && matches!(object.unparenthesized(), Expression::ConstantAccess(object) if context.names.binding(&object.name) == Some(Binding::This))
            && matches!(selector, ClassLikeMemberSelector::Identifier(member) if member.value == property) =>
        {
            let property = BytesDisplay(property);
            context.report(
                Issue::error(format!("Write `field` instead of `this.{property}` inside `{property}`'s own accessor."))
                    .with_annotation(Annotation::primary(node.span()).with_message("Written here."))
                    .with_note("PHP reads and writes the storage there, while C# would call the accessor again."),
            );
        }
        _ => {}
    }

    ensure_sufficient_stack(|| node.visit_children(|child| check_accessor_node(child, property, in_lambda, context)));
}

/// Reports each `this` in an initial value. A constant initial value is the member's default, and any other runs at
/// the start of the constructor in declaration order, before the members declared after it are set, so it cannot read
/// the object, as in C#. Returns whether it reported one.
fn report_this_in_initial_value(node: Node<'_, '_>, context: &mut Context<'_, '_, '_>) -> bool {
    let mut reported = false;
    if let Node::ConstantAccess(access) = node
        && context.names.binding(&access.name) == Some(Binding::This)
    {
        context.report(
            Issue::error(
                "An initial value cannot use `this`: it runs before the constructor body, while the object is built.",
            )
            .with_annotation(Annotation::primary(access.span()).with_message("Used here."))
            .with_help("Set the member in the constructor body instead."),
        );
        reported = true;
    }

    ensure_sufficient_stack(|| node.visit_children(|child| reported |= report_this_in_initial_value(child, context)));

    reported
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

/// The statements at file level of a PHP# file, and those of each of its namespaces, the ones the slice refuses too.
fn file_statements<'ast, 'arena>(program: &'ast Program<'arena>) -> impl Iterator<Item = &'ast Statement<'arena>> {
    program.statements.iter().flat_map(|statement| match statement {
        Statement::Namespace(namespace) => namespace.statements().as_slice(),
        statement => std::slice::from_ref(statement),
    })
}

/// Checks a ternary, and returns the place of its parts. PHP#'s ternary is `c ? a : b`, and PHP's `a ?: b` does not
/// exist, as spec section 21 says, so `?:` is reported between its two characters. A ternary as the condition of
/// another needs parentheses, as in PHP 8, so it is reported over that condition, which the walk then skips.
fn check_conditional(conditional: &Conditional, context: &mut Context<'_, '_, '_>) -> Place {
    if conditional.then.is_none() {
        context.report(
            Issue::error("PHP# has no `?:`: write `a ?? b` to replace null, or `c ? a : b` with a `bool` condition.")
                .with_annotation(
                    Annotation::primary(Span::between(conditional.question_mark, conditional.colon))
                        .with_message("Written here."),
                )
                .with_note("`?:` tests truthiness, so it replaces `\"\"` and `\"0\"` along with null."),
        );
    } else if let Expression::Conditional(nested @ Conditional { then: Some(_), .. }) = conditional.condition {
        context.report(
            Issue::error(
                "Unparenthesized `a ? b : c ? d : e` is not supported. Use either `(a ? b : c) ? d : e` or `a ? b : (c ? d : e)`.",
            )
            .with_annotation(Annotation::primary(nested.span()).with_message("Nested here.")),
        );

        return Place::RefusedPart(nested.span());
    }

    Place::Body
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

/// Whether every body of an `if`, `else` or loop is a block in braces. An `else` may also hold the next `if`.
fn has_braces(statement: &Statement) -> bool {
    let is_block = |statement: &Statement| matches!(statement, Statement::Block(_));

    match statement {
        Statement::If(If { body: IfBody::Statement(body), .. }) => {
            is_block(body.statement)
                && body
                    .else_clause
                    .as_ref()
                    .is_none_or(|clause| is_block(clause.statement) || matches!(clause.statement, Statement::If(_)))
        }
        Statement::While(While { body: WhileBody::Statement(body), .. })
        | Statement::For(For { body: ForBody::Statement(body), .. }) => is_block(body),
        Statement::ForOf(for_of) => is_block(for_of.body),
        Statement::DoWhile(do_while) => is_block(do_while.statement),
        _ => true,
    }
}

/// Checks the arms of a `match`, and returns whether its children can be checked. Spec section 21 needs a `default`
/// arm on a value that is not an enum, and section 20 lets a `match` on an enum leave it out when its arms cover every
/// case. Only the analyzer knows the value's type, so a `match` whose arms all name `Class.y` values or `null` may
/// leave out `default`, and the analyzer reports the cases it misses. A block arm is only in a `match` statement. The
/// engine takes one `default` arm, as PHP's `match` does.
fn check_pattern_match(pattern_match: &PatternMatch, is_expression: bool, context: &mut Context<'_, '_, '_>) -> bool {
    let mut defaults = pattern_match.arms.iter().filter(|arm| arm.is_default());
    match defaults.next() {
        None if !pattern_match.arms.iter().all(|arm| names_class_values(arm, context)) => {
            context.report(
                Issue::error("A `match` needs a `default` arm.")
                    .with_annotation(Annotation::primary(pattern_match.r#match.span).with_message("This `match` has none."))
                    .with_note("Spec section 21: only a `match` on an enum may leave out `default`, when its arms cover every case.")
                    .with_help("Add `default => …` as the last arm."),
            );

            return false;
        }
        None => {}
        Some(default) => {
            if let Some(again) = defaults.next() {
                context.report(
                    Issue::error("A `match` has one `default` arm.")
                        .with_annotation(Annotation::primary(again.span()).with_message("Written again here."))
                        .with_annotation(Annotation::secondary(default.span()).with_message("First written here.")),
                );

                return false;
            }
        }
    }

    let block = pattern_match.arms.iter().find_map(|arm| match arm.body() {
        PatternMatchArmBody::Block(block) if is_expression => Some(block),
        _ => None,
    });
    if let Some(block) = block {
        context.report(
            Issue::error("A block arm is only in a `match` statement: a `match` that gives a value gives an expression in each arm.")
                .with_annotation(Annotation::primary(block.span()).with_message("Block written here."))
                .with_help("Start the statement with `match`, or give a value in this arm."),
        );

        return false;
    }

    true
}

/// Whether an arm's pattern names only `Class.y` values or `null`, joined by `or`, as the arms of a `match` on an enum
/// do.
fn names_class_values(arm: &PatternMatchArm, context: &Context<'_, '_, '_>) -> bool {
    fn is_class_value(pattern: &Pattern, context: &Context<'_, '_, '_>) -> bool {
        match pattern {
            Pattern::Value(Expression::Access(Access::Property(access))) => {
                context.names.static_property_class(access).is_some()
            }
            Pattern::Value(Expression::Literal(Literal::Null(_))) => true,
            Pattern::Binary(binary) if !binary.is_and() => {
                is_class_value(binary.left, context) && is_class_value(binary.right, context)
            }
            Pattern::Parenthesized(parenthesized) => is_class_value(parenthesized.pattern, context),
            _ => false,
        }
    }

    match arm {
        PatternMatchArm::Pattern(arm) => is_class_value(arm.pattern, context),
        PatternMatchArm::Default(_) => true,
    }
}

/// Reports each variable a pattern declares under `or` or `not`, which C# refuses as error CS8780: the variable would
/// have no value where the pattern matches. `hidden` is whether an enclosing pattern already hides it.
fn report_hidden_pattern_variables(pattern: &Pattern, hidden: bool, context: &mut Context<'_, '_, '_>) {
    match pattern {
        Pattern::Type(TypePattern { variable: Some(variable), .. }) if hidden => context.report(
            Issue::error(format!(
                "`{}` is declared under `or` or `not`, where the pattern can match without a value for it.",
                BytesDisplay(variable.value)
            ))
            .with_annotation(Annotation::primary(variable.span).with_message("Declared here."))
            .with_help("Declare the variable outside `or` and `not`, or test the value again where you use it."),
        ),
        Pattern::Type(_) | Pattern::Value(_) | Pattern::Comparison(_) => {}
        Pattern::Not(not) => report_hidden_pattern_variables(not.pattern, true, context),
        Pattern::Binary(binary) => {
            let hidden = hidden || !binary.is_and();
            report_hidden_pattern_variables(binary.left, hidden, context);
            report_hidden_pattern_variables(binary.right, hidden, context);
        }
        Pattern::Parenthesized(parenthesized) => {
            report_hidden_pattern_variables(parenthesized.pattern, hidden, context)
        }
        Pattern::Properties(properties) => {
            for property in &properties.properties {
                report_hidden_pattern_variables(property.pattern, hidden, context);
            }
        }
    }
}

/// Whether the slice can write an expression: a local, a parameter, `field`, a member written `object.name`, or an
/// index of one of them, as in `counts["a"]`. A member written by its bare name passes, because
/// `check_constant_access` reports it with the name to write instead.
fn is_slice_target(target: &Expression, context: &Context<'_, '_, '_>) -> bool {
    match target {
        Expression::ConstantAccess(access) => {
            matches!(context.names.binding(&access.name), Some(Binding::Local(_) | Binding::Field | Binding::Member))
        }
        Expression::Access(Access::Property(_)) => true,
        Expression::ArrayAccess(access) => is_slice_target(access.array, context),
        _ => false,
    }
}

/// Whether the slice has a type: the built-in types of spec section 24, or a class or a type parameter written by its
/// short name, or a nullable type, or `List<T>` or `Map<TKey, TValue>` of spec section 12, `Class<T>` of section 25 or
/// a class with type arguments of section 11, none of them `void`, or a function type of spec section 14.1 without a
/// `void` parameter, whose inner types the walk checks next. The analyzer checks a generic class's arity.
fn is_slice_type(hint: &Hint) -> bool {
    match hint {
        Hint::Integer(_)
        | Hint::Float(_)
        | Hint::Bool(_)
        | Hint::String(_)
        | Hint::Void(_)
        | Hint::Identifier(Identifier::Local(_))
        | Hint::Nullable(_) => true,
        Hint::Mixed(any) => any.value == ANY,
        Hint::Generic(generic) => {
            let arguments = &generic.type_arguments.arguments;

            built_in_generic_arity(generic.name.value).is_none_or(|arity| arguments.len() == arity)
                && !arguments.iter().any(|argument| matches!(argument, Hint::Void(_)))
        }
        Hint::Function(function) => !function.parameters.iter().any(|parameter| matches!(parameter, Hint::Void(_))),
        _ => false,
    }
}

/// The note of a refusal of what needs a type argument while the code runs. G1 checks generics and erases them.
const ERASED_TYPE_ARGUMENTS: &str = "Type arguments do not reach the running program yet, so it cannot tell which type a type parameter or a generic type names.";

/// The number of type arguments a generic type takes when its name is PHP#'s own `List`, `Map` or `Class`, which name
/// no class.
fn built_in_generic_arity(name: &[u8]) -> Option<usize> {
    match name {
        b"List" | b"Class" => Some(1),
        b"Map" => Some(2),
        _ => None,
    }
}

/// Whether the binder bound a type name to a type parameter.
fn is_type_parameter(name: &impl HasPosition, context: &Context<'_, '_, '_>) -> bool {
    matches!(context.names.binding(name), Some(Binding::TypeParameter { .. }))
}

/// The class whose constant or static field, property or method names `name`, a type parameter the class declares.
/// `None` for any other name, a static method's own type parameter included, because the method declares it.
fn static_member_class<'ast, 'arena>(
    name: &Identifier<'_>,
    context: &Context<'_, 'ast, 'arena>,
) -> Option<ClassLike<'ast, 'arena>> {
    let Some(Binding::TypeParameter { declaration }) = context.names.binding(name) else {
        return None;
    };
    let class = enclosing_class(context.program, name.span())?;
    let names_it = class.members.iter().any(|member| {
        let (span, is_static) = match member {
            ClassLikeMember::Method(method) => (method.span(), method.modifiers.contains_static()),
            ClassLikeMember::Property(property) => (property.span(), property.modifiers().contains_static()),
            ClassLikeMember::Constant(constant) => (constant.span(), true),
            _ => return false,
        };

        is_static && span.contains(&name.span().start) && !span.contains(&declaration.start)
    });

    names_it.then_some(class)
}

/// Reports each type parameter of a list whose name is not `T`, or `T` and an uppercase letter, as C# names them, a
/// name the list declares twice, and `in` or `out` on a method's, because only a class or an interface declares
/// variance, spec section 11.1.
fn check_type_parameters(list: &TypeParameterList, place: Place, context: &mut Context<'_, '_, '_>) {
    for (index, parameter) in list.parameters.iter().enumerate() {
        let name = parameter.name.value;
        if !matches!(name, [b'T'] | [b'T', b'A'..=b'Z', ..]) {
            context.report(
                Issue::error("A type parameter's name starts with `T`, as in `TItem`.")
                    .with_annotation(Annotation::primary(parameter.name.span).with_message("Named here.")),
            );
        }

        if let Some(first) = list.parameters.iter().take(index).find(|earlier| earlier.name.value == name) {
            context.report(
                Issue::error(format!("This type parameter list declares `{}` twice.", BytesDisplay(name)))
                    .with_annotation(Annotation::primary(parameter.name.span).with_message("Declared again here."))
                    .with_annotation(Annotation::secondary(first.name.span).with_message("First declared here.")),
            );
        }

        if matches!(place, Place::Method | Place::Signature)
            && let Some(variance) = parameter.variance
        {
            context.report(
                Issue::error(format!(
                    "A method's type parameter takes no `{}`: only a class or an interface declares variance, so remove it.",
                    BytesDisplay(variance.value)
                ))
                .with_annotation(Annotation::primary(variance.span).with_message("Written here.")),
            );
        }
    }
}

/// Whether a type parameter's bound is a class or an interface, a generic class type, or several of them joined with
/// `&`, as spec section 11 writes it.
fn is_slice_bound(hint: &Hint, context: &Context<'_, '_, '_>) -> bool {
    match hint {
        Hint::Identifier(name @ Identifier::Local(_)) => !is_type_parameter(name, context),
        Hint::Generic(generic) => {
            built_in_generic_arity(generic.name.value).is_none() && !is_type_parameter(&generic.name, context)
        }
        Hint::Intersection(intersection) => {
            is_slice_bound(intersection.left, context) && is_slice_bound(intersection.right, context)
        }
        _ => false,
    }
}

/// Checks the type arguments a generic type's name takes: a `Map`'s key, spec section 12, `Class<T>`'s class, section
/// 25, and none on a type parameter. The analyzer checks a generic class's arity and bounds. Returns the place of the
/// type arguments, or none when the walk skips them.
fn check_generic(generic: &GenericHint, place: Place, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    if is_type_parameter(&generic.name, context) {
        context.report(
            Issue::error(format!(
                "A type parameter takes no type arguments: write `{}`.",
                BytesDisplay(generic.name.value)
            ))
            .with_annotation(Annotation::primary(generic.span()).with_message("Written here.")),
        );

        return None;
    }

    match (generic.name.value, generic.type_arguments.arguments.as_slice()) {
        // The analyzer refuses a named key type without an `int` or `string` backing value. A key type outside the
        // slice, nullable or not, is refused once, by the walk, as that type.
        (b"Map", [key, _])
            if !matches!(key, Hint::Integer(_) | Hint::String(_) | Hint::Identifier(_))
                && is_slice_type(match key {
                    Hint::Nullable(nullable) => nullable.hint,
                    key => key,
                }) =>
        {
            report_refusal(
                Issue::error("A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value.")
                    .with_annotation(Annotation::primary(key.span()).with_message("Key type written here.")),
                generic.span(),
                place,
                context,
            )
        }
        (b"Class", [class]) if !matches!(class, Hint::Identifier(Identifier::Local(_))) => {
            context.report(
                Issue::error(
                    "`Class`'s type argument is a class, an interface or a type parameter, as in `Class<Order>`.",
                )
                .with_annotation(Annotation::primary(class.span()).with_message("Written here.")),
            );

            None
        }
        _ => Some(place),
    }
}

/// Checks a union type. Each member in the slice is decided with the parts inside it as it is outside a union, so a
/// generic member takes the type arguments it takes alone. A type written twice is compared as the engine compares it:
/// a class by its full name, ignoring case.
fn check_union(union: &UnionHint, place: Place, context: &mut Context<'_, '_, '_>) {
    let mut members = Vec::new();
    union_members(union, &mut members);

    for (index, member) in members.iter().enumerate() {
        let is_slice_member = match member {
            Hint::Null(_) => {
                report_not_supported(
                    member.span(),
                    "union that holds null",
                    "PHP# writes a union that holds null in parentheses with `?` after it, as in `(int|string)?`.",
                    context,
                );

                false
            }
            Hint::Void(_) | Hint::Nullable(_) => false,
            Hint::Self_(keyword) => check_self_type(keyword, place, context),
            Hint::Static(keyword) => {
                report_php_static(keyword, context);

                false
            }
            _ if is_slice_type(member) => true,
            _ => {
                report_not_supported(member.span(), "type", supported(place), context);

                false
            }
        };

        if !is_slice_member {
            continue;
        }
        enter_tree(Node::Hint(member), place, context);
        let Some(first) = members[..index].iter().find(|earlier| is_same_type(earlier, member, context)) else {
            continue;
        };

        let written = BytesDisplay(context.get_code_snippet(*member));
        context.report(
            Issue::error(format!("Duplicate type `{written}` is redundant."))
                .with_annotation(Annotation::primary(member.span()).with_message("Written again here."))
                .with_annotation(Annotation::secondary(first.span()).with_message("First written here."))
                .with_help("Remove the second one, as PHP refuses a union that names a type twice."),
        );
    }
}

/// Decides a node and every node inside it at a place, as the checking walk does, for a node whose parent's check
/// decides it and so keeps the walk out of it.
fn enter_tree(node: Node<'_, '_>, place: Place, context: &mut Context<'_, '_, '_>) {
    if let Some(inner) = enter(node, place, context) {
        ensure_sufficient_stack(|| node.visit_children(|child| enter_tree(child, inner, context)));
    }
}

/// Whether a `self` keyword is PHP#'s `Self`, written exactly so. The lexer reads `Self` and PHP's `self` as one
/// keyword, and any other spelling is PHP's `self`.
fn is_sharp_self(keyword: &Keyword) -> bool {
    keyword.value == b"Self"
}

/// Checks `Self` or PHP's `self` written as a type at a place, and returns whether the slice has it. Spec section 25
/// makes `Self` a return type only, as in Swift.
fn check_self_type(keyword: &Keyword, place: Place, context: &mut Context<'_, '_, '_>) -> bool {
    let is_return_type = matches!(place, Place::Method | Place::Signature);
    if !is_sharp_self(keyword) {
        report_php_self(keyword, is_return_type, context);
    } else if !is_return_type {
        context.report(
            Issue::error("`Self` is only a return type in PHP#.")
                .with_annotation(Annotation::primary(keyword.span).with_message("Written here."))
                .with_note("`Self` is the class a static method is called on, which a method returns."),
        );
    }

    is_sharp_self(keyword) && is_return_type
}

/// Reports PHP's `self`, which spec section 25 removes: a class writes its own name, or `Self` where `self_fits`.
fn report_php_self(keyword: &Keyword, self_fits: bool, context: &mut Context<'_, '_, '_>) {
    let class = enclosing_class_like_name(context.program, keyword.span)
        .map_or(String::new(), |name| format!(", `{}`,", BytesDisplay(name)));
    let or_self = if self_fits { ", or `Self` for the class a static method is called on" } else { "" };

    context.report(
        Issue::error(format!(
            "PHP# has no `self`: write the class's own name{class} for the declaring class{or_self}."
        ))
        .with_annotation(Annotation::primary(keyword.span).with_message("Written here.")),
    );
}

/// Reports PHP's `static`, which spec section 25 writes `Self`.
fn report_php_static(keyword: &Keyword, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error("PHP# writes `Self` for PHP's `static`.")
            .with_annotation(Annotation::primary(keyword.span).with_message("Written here.")),
    );
}

/// Decides `new` on a class written by its short name, on `Self`, or on PHP's `self` or `static`. `new Self(…)` needs
/// the class's constructor marked `required`, spec section 25, because `Self` can be any subclass, and names no type
/// arguments, spec section 11, because `Self` is the class with its own type parameters. PHP's `self` and
/// `static` are reported at the keyword, so the arguments are still checked. `new` of a type parameter needs its type
/// argument while the code runs, which G1 erases.
fn check_instantiation(instantiation: &Instantiation, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    match instantiation.class {
        Expression::Self_(keyword) if !is_sharp_self(keyword) => {
            report_php_self(keyword, true, context);

            return Some(Place::Instantiation);
        }
        Expression::Static(keyword) => {
            report_php_static(keyword, context);

            return Some(Place::Instantiation);
        }
        Expression::Identifier(class) if is_type_parameter(class, context) => {
            report_not_supported(instantiation.span(), "`new` of a type parameter", ERASED_TYPE_ARGUMENTS, context);

            return None;
        }
        Expression::Self_(_) if let Some(type_arguments) = &instantiation.type_arguments => {
            context.report(
                Issue::error("`Self` already carries its class's type parameters.")
                    .with_annotation(Annotation::primary(type_arguments.span()).with_message("Written here."))
                    .with_help("Write `new Self(…)`."),
            );
        }
        _ => {}
    }

    if instantiation.argument_list.is_none() {
        report_not_supported(
            instantiation.span(),
            "`new` without arguments",
            "PHP# creates an object with `new Class(arguments)`, parentheses included.",
            context,
        );

        return None;
    }

    if matches!(instantiation.class, Expression::Self_(_)) && context.slice_places.contains(&Some(Place::Enum)) {
        context.report(
            Issue::error("An enum has no constructor: its cases are its only values.")
                .with_annotation(Annotation::primary(instantiation.span()).with_message("Created here.")),
        );

        return None;
    }

    if matches!(instantiation.class, Expression::Self_(_))
        && let Some(class) = enclosing_class(context.program, instantiation.span())
        && !has_required_constructor(&class)
    {
        let name = BytesDisplay(class.name.value);
        context.report(
            Issue::error(
                "`new Self(…)` needs a `required` constructor: `Self` can be any subclass, so every subclass must keep a constructor that `new Self(…)` can call.",
            )
            .with_annotation(Annotation::primary(instantiation.span()).with_message("Created here."))
            .with_help(format!("Declare the constructor of `{name}` `required`, as in `public required {name}(…)`.")),
        );

        return None;
    }

    Some(Place::Instantiation)
}

/// Whether a class's constructor is marked `required`.
fn has_required_constructor(class: &ClassLike) -> bool {
    class.members.iter().any(|member| {
        matches!(member, ClassLikeMember::Method(method)
            if php_method_name(method) == b"__construct"
                && method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Required(_))))
    })
}

/// Whether a parameter belongs to a class's constructor, the one method where an access modifier declares a property.
fn is_constructor_parameter(parameter: &FunctionLikeParameter, program: &Program) -> bool {
    enclosing_class(program, parameter.span()).is_some_and(|class| {
        class.members.iter().any(|member| {
            matches!(member, ClassLikeMember::Method(method)
                if php_method_name(method) == b"__construct"
                    && method.parameter_list.span().contains(&parameter.span().start))
        })
    })
}

/// The members of a union, in the order they are written. The parser nests a union to the right.
fn union_members<'ast, 'arena>(union: &'ast UnionHint<'arena>, members: &mut Vec<&'ast Hint<'arena>>) {
    for side in [union.left, union.right] {
        match side {
            Hint::Union(inner) => union_members(inner, members),
            member => members.push(member),
        }
    }
}

fn is_same_type(first: &Hint, second: &Hint, context: &Context<'_, '_, '_>) -> bool {
    match (first, second) {
        (Hint::Identifier(first), Hint::Identifier(second)) => {
            context.names.get(first).eq_ignore_ascii_case(context.names.get(second))
        }
        _ => std::mem::discriminant(first) == std::mem::discriminant(second),
    }
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
        | BinaryOperator::Exponentiation(_)
        | BinaryOperator::Equal(_)
        | BinaryOperator::NotEqual(_)
        | BinaryOperator::Identical(_)
        | BinaryOperator::NotIdentical(_)
        | BinaryOperator::LessThan(_)
        | BinaryOperator::LessThanOrEqual(_)
        | BinaryOperator::GreaterThan(_)
        | BinaryOperator::GreaterThanOrEqual(_)
        | BinaryOperator::And(_)
        | BinaryOperator::Or(_)
        | BinaryOperator::NullCoalesce(_) => true,
        BinaryOperator::BitwiseAnd(_)
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

/// Checks a cast at a place, reporting it at its operator when the slice does not have it. Spec section 24 keeps
/// `(int)`, `(float)` and `(string)`, which the slice has in a method body, and removes PHP's other casts and its cast
/// aliases. The refusal of `(array)` names an operand that is a local or a parameter, as the spec's `(array)row` does.
/// Every cast is named, so a new one does not compile until it is decided.
fn check_cast(unary_prefix: &UnaryPrefix, place: Place, context: &mut Context<'_, '_, '_>) {
    let operator = &unary_prefix.operator;
    let compare = "compare the value instead, as in `count > 0` or `flag == \"1\"`.";
    let parse_or_wrap;
    let (cast, instead) = match operator {
        UnaryPrefixOperator::IntCast(..) | UnaryPrefixOperator::FloatCast(..) | UnaryPrefixOperator::StringCast(..)
            if place == Place::Body =>
        {
            return;
        }
        UnaryPrefixOperator::IntCast(..)
        | UnaryPrefixOperator::FloatCast(..)
        | UnaryPrefixOperator::StringCast(..)
        | UnaryPrefixOperator::UnsetCast(..)
        | UnaryPrefixOperator::VoidCast(..) => {
            return report_not_supported(operator.span(), "operator", supported(place), context);
        }
        UnaryPrefixOperator::BoolCast(..) => ("(bool)", compare),
        UnaryPrefixOperator::BooleanCast(..) => ("(boolean)", compare),
        UnaryPrefixOperator::ArrayCast(..) => {
            let value = match unary_prefix.operand {
                Expression::ConstantAccess(ConstantAccess { name: name @ Identifier::Local(local) })
                    if matches!(context.names.binding(name), Some(Binding::Local(_))) =>
                {
                    local.value
                }
                _ => b"value".as_slice(),
            };
            let value = BytesDisplay(value);
            parse_or_wrap =
                format!("write a struct's `parse({value})` for an object, or `List.wrap({value})` for a value.");

            ("(array)", parse_or_wrap.as_str())
        }
        UnaryPrefixOperator::ObjectCast(..) => ("(object)", "write a `Map` literal, or a struct."),
        UnaryPrefixOperator::IntegerCast(..) => ("(integer)", "write `(int)`."),
        UnaryPrefixOperator::DoubleCast(..) => ("(double)", "write `(float)`."),
        UnaryPrefixOperator::RealCast(..) => ("(real)", "write `(float)`."),
        UnaryPrefixOperator::BinaryCast(..) => ("(binary)", "write `(string)`."),
        UnaryPrefixOperator::ErrorControl(_)
        | UnaryPrefixOperator::Reference(_)
        | UnaryPrefixOperator::BitwiseNot(_)
        | UnaryPrefixOperator::Not(_)
        | UnaryPrefixOperator::PreIncrement(_)
        | UnaryPrefixOperator::PreDecrement(_)
        | UnaryPrefixOperator::Plus(_)
        | UnaryPrefixOperator::Negation(_) => unreachable!("`enter` calls `check_cast` only for a cast"),
    };

    report_removed(
        operator.span(),
        cast.as_bytes(),
        instead,
        "Spec section 24 keeps `(int)`, `(float)` and `(string)` between numbers, and removes PHP's other casts and its cast aliases.",
        context,
    );
}

/// Reports a node's refusal, and returns the place of its children: none when the refusal's primary span covers
/// `skipped`, the node or the part of it that holds its code, so the walk skips the children, and `place` when the
/// refusal covers another part, such as a name or a modifier, so the walk still checks the rest.
fn report_refusal(issue: Issue, skipped: Span, place: Place, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    let covers = issue.annotations.iter().any(|annotation| {
        matches!(annotation.kind, AnnotationKind::Primary) && annotation.span.join(skipped) == annotation.span
    });
    context.report(issue);

    (!covers).then_some(place)
}

/// Reports a PHP form that PHP# removes, with what to write instead and why.
fn report_removed(span: Span, form: &[u8], instead: &str, note: &str, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!("PHP# has no `{}`: {instead}", BytesDisplay(form)))
            .with_annotation(Annotation::primary(span).with_message("Written here."))
            .with_note(note),
    );
}

/// Reports `die`, and `exit` with a string literal or a template, which spec section 8 removes.
fn report_die(span: Span, context: &mut Context<'_, '_, '_>) {
    report_removed(
        span,
        b"die",
        "write the message to STDERR, then `exit(1)`.",
        "`die(\"…\")` and `exit(\"…\")` print the message and exit with status 0, which reports success.",
        context,
    );
}

/// The span of the PHP superglobal an expression names, written bare as `_SERVER` or with `$`, through any index of
/// it, as in `_SESSION["user"]`. A bare name is the superglobal when it binds as a constant, or as a class before `.`
/// as in `_SERVER.x`, so a local named `_GET`, which `check_local_name` reports, is not.
fn superglobal_span(expression: &Expression, context: &Context<'_, '_, '_>) -> Option<Span> {
    match expression {
        Expression::ArrayAccess(access) => superglobal_span(access.array, context),
        Expression::ConstantAccess(ConstantAccess { name: name @ Identifier::Local(local) })
            if SUPERGLOBALS.contains(&local.value)
                && matches!(context.names.binding(name), Some(Binding::Constant | Binding::Class)) =>
        {
            Some(local.span)
        }
        Expression::Variable(Variable::Direct(direct))
            if direct.name.strip_prefix(b"$").is_some_and(|name| SUPERGLOBALS.contains(&name)) =>
        {
            Some(direct.span)
        }
        _ => None,
    }
}

/// The bare name an expression reads when it is written `__Something__`, the form of PHP's magic constants, which spec
/// section 27 removes. It binds as a constant, or as a class before `.` as in `__Foo__.bar`.
fn magic_name<'ast, 'arena>(
    expression: &'ast Expression<'arena>,
    context: &Context<'_, '_, '_>,
) -> Option<&'ast LocalIdentifier<'arena>> {
    match expression {
        Expression::ConstantAccess(ConstantAccess { name: name @ Identifier::Local(local) })
            if is_magic_name(local.value)
                && matches!(context.names.binding(name), Some(Binding::Constant | Binding::Class)) =>
        {
            Some(local)
        }
        _ => None,
    }
}

/// Whether a name is written `__Something__`, with at least one byte between the underscores.
fn is_magic_name(name: &[u8]) -> bool {
    name.len() > 4 && name.starts_with(b"__") && name.ends_with(b"__")
}

/// Reports a declared name written `__Something__`, which spec section 27 removes as it removes reading one.
fn check_declared_name(name: &[u8], span: Span, context: &mut Context<'_, '_, '_>) {
    if is_magic_name(name) {
        report_magic_constant(name, span, None, context);
    }
}

/// Reports a PHP superglobal, written with `$` or bare, which spec section 29 removes.
fn report_superglobal(span: Span, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error("PHP# has no superglobals; take a Request")
            .with_annotation(Annotation::primary(span).with_message("Written here."))
            .with_note(
                "Request data arrives as an object, such as a framework's `Request`, and the process environment as `Environment`.",
            ),
    );
}

/// Reports a PHP magic constant, or another `__Something__` name when `constant` is `None`, which spec section 27
/// replaces with `Position`. The error names it as written.
fn report_magic_constant(name: &[u8], span: Span, constant: Option<&MagicConstant>, context: &mut Context<'_, '_, '_>) {
    let instead = match constant {
        Some(MagicConstant::Directory(_)) => "write `Position.current().directory`.",
        Some(MagicConstant::File(_)) => "write `Position.current().file`.",
        Some(MagicConstant::Line(_)) => "write `Position.current().line`.",
        Some(MagicConstant::Function(_) | MagicConstant::Method(_)) => "write `Position.current().function`.",
        Some(MagicConstant::Class(_)) => "write `typeof(Class)` with the class's name.",
        Some(MagicConstant::Namespace(_)) => "write `Position.current().function`, which starts with the namespace.",
        Some(MagicConstant::Trait(_) | MagicConstant::Property(_)) | None => {
            "`Position.current()` gives the file, directory, line, column and function."
        }
    };

    report_removed(
        span,
        name,
        instead,
        "Spec section 27 removes PHP's magic constants and every other `__Something__` name: `Position` says where code sits in its source.",
        context,
    );
}

/// Whether the slice has an assignment operator. Every operator is named, so a new one does not compile until it
/// is decided.
const fn is_slice_assignment_operator(operator: &AssignmentOperator) -> bool {
    match operator {
        AssignmentOperator::Assign(_)
        | AssignmentOperator::Addition(_)
        | AssignmentOperator::Subtraction(_)
        | AssignmentOperator::Multiplication(_)
        | AssignmentOperator::Division(_)
        | AssignmentOperator::Exponentiation(_)
        | AssignmentOperator::Coalesce(_) => true,
        AssignmentOperator::Modulo(_)
        | AssignmentOperator::Concat(_)
        | AssignmentOperator::BitwiseAnd(_)
        | AssignmentOperator::BitwiseOr(_)
        | AssignmentOperator::BitwiseXor(_)
        | AssignmentOperator::LeftShift(_)
        | AssignmentOperator::RightShift(_) => false,
    }
}

fn report_unsupported(node: Node<'_, '_>, place: Place, context: &mut Context<'_, '_, '_>) {
    let construct = match node {
        Node::Statement(_) => "statement",
        Node::Expression(_) | Node::ConstantAccess(_) => "expression",
        Node::BinaryOperator(_) | Node::UnaryPrefixOperator(_) | Node::AssignmentOperator(_) => "operator",
        Node::Hint(_) | Node::NullableHint(_) => "type",
        Node::Pattern(_) => "pattern",
        Node::Modifier(_) => "modifier",
        Node::IfStatementBodyElseIfClause(_) => {
            return report_not_supported(node.span(), "`elseif`", "PHP# writes `else if`.", context);
        }
        Node::Extends(_) => "`extends` clause",
        Node::Implements(_) => "`implements` clause",
        Node::ClassLikeMember(_) => "class member",
        Node::ClassLikeMemberSelector(_) => "member name",
        _ => "construct",
    };

    report_not_supported(node.span(), construct, supported(place), context);
}

/// What the slice has at a place, as the note of a refusal there.
const fn supported(place: Place) -> &'static str {
    match place {
        Place::File => "At file level, PHP# supports `namespace`, `import`, `class`, `interface` and `enum`.",
        Place::Class => {
            "A PHP# class has attributes, an optional `public`, `abstract` or `final`, a name, optional type parameters as in `<out TItem : DatabaseEntity>`, an optional `: Base, Interface` header, constants, fields, properties and methods, with no other modifiers, `extends` or `implements`."
        }
        Place::Interface => {
            "A PHP# interface has an optional `public`, a name, optional type parameters as in `<in TItem>`, an optional `: Interface` header and methods, with no attributes, other modifiers, `extends`, constants or properties."
        }
        Place::Signature => {
            "A PHP# interface method has no modifier, optional type parameters, parameters, a return type of `int`, `float`, `bool`, `string`, `Any`, `void`, a class or a type parameter, a generic type as in `List<T>` or `PaginatedList<Order>`, or `Self`, each but `void` nullable as in `int?`, or a union of them but `void`, as in `int|string`, and no body."
        }
        Place::Enum => {
            "A PHP# enum has attributes, an optional `public`, a name, an optional `: string, Interface` header whose `int` or `string` comes first, constants, cases and methods, with no other modifiers or `implements`."
        }
        Place::Header => {
            "A PHP# header names each base class and interface by its short name, as in `: DatabaseEntity, Linkable`, and a generic one with its type arguments, as in `: PaginatedList<Order>`."
        }
        Place::TypeParameter => {
            "A PHP# type parameter is `in` or `out` on a class's or an interface's only, a name that starts with `T`, and an optional bound of classes and interfaces joined with `&`, as in `<out TItem : DatabaseEntity & Shareable>`."
        }
        Place::FieldOrProperty => {
            "A PHP# field is `private` or `protected`, and a property has the accessors `get` and an optional `set`, each `;`, `=> expr;` or a block. Both may be `static`, and have a type of `int`, `float`, `bool`, `string`, `Any`, a class or a type parameter, a generic type as in `List<T>` or `PaginatedList<Order>`, or `Function<R(P)>`, nullable as in `int?` or not, or a union of them as in `int|string`, a name, and an optional initial value."
        }
        Place::ClassConstant => {
            "A PHP# constant has `public`, `protected` or `private`, an optional type of `int`, `float`, `bool`, `string`, `Any` or a class, nullable as in `int?` or not, or a union of them as in `int|string`, one name, and a constant value."
        }
        Place::Method => {
            "A PHP# method takes `public`, `protected`, `private`, `static`, `abstract`, `virtual` and `override`, a constructor also `required`, optional type parameters as in `<TItem : DatabaseEntity>`, parameters, and a return type of `int`, `float`, `bool`, `string`, `Any`, `void`, a class or a type parameter, a generic type as in `List<T>` or `PaginatedList<Order>`, `Function<R(P)>` or `Self`, each but `void` nullable as in `int?`, or a union of them but `void`, as in `int|string`."
        }
        Place::Parameter => {
            "A PHP# parameter has a type of `int`, `float`, `bool`, `string`, `Any`, a class or a type parameter, a generic type as in `List<T>` or `PaginatedList<Order>`, or `Function<R(P)>`, nullable as in `int?` or not, or a union of them as in `int|string`, a name, and an optional default. The last parameter can be variadic, as in `int ...values`."
        }
        Place::Lambda => {
            "A PHP# lambda is a bare arrow after one name or parenthesized parameters, each with an optional type, and its body is an expression or a block, as in `(a, b) => a + b`. The last parameter can be variadic, as in `(int ...values) => count(values)`."
        }
        Place::Body
        | Place::Instantiation
        | Place::FunctionCall
        | Place::TryCatchClause
        | Place::SuperCall
        | Place::SelfCall
        | Place::RefusedPart(_) => {
            "In a method body, PHP# supports blocks, expression statements, `return`, `let` and `const` and typed locals, `if` with `else if` and `else`, `while`, `do … while`, `for`, `for … of`, `break` and `continue` without a level, and `try` with `catch` and `finally`, with literals, list and map literals, index reads, templates, parentheses, bare names, assignment, arithmetic, comparison and logical operators, `??`, the ternary `c ? a : b`, `(int)`, `(float)` and `(string)`, `++` and `--`, method calls, with optional type arguments as in `Json.decode<WebhookPayload>(body)`, and property reads written with `.` or `?.`, lambdas as in `x => x.id`, `new Class(...)` with optional type arguments as in `new PaginatedList<Order>(rows)` and, with a `required` constructor, `new Self(...)`, calls of global functions and of a local that holds a lambda, `super.method(...)` and `Self.method(...)`, each with positional, named and spread arguments as in `max(...prices)`, `throw`, `exit(code)`, `typeof(Class)`, `is`, `as` and `match`."
        }
        Place::Attribute => {
            "A PHP# attribute is a class name with optional positional and named arguments, as in `[Field(\"Name\", searchable: true)]`."
        }
        Place::Constant => {
            "A parameter default, an attribute argument, a constant's value or an enum case's value is a literal, a constant, `typeof(Class)`, a list or map literal, or arithmetic, comparison, logical and `??` operators on them."
        }
        Place::Pattern => {
            "A PHP# pattern is a type with an optional name, as in `int count`, a value, a comparison such as `< 10`, a properties pattern such as `{ total: > 0 }`, or patterns joined by `and`, `or` and `not`."
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

/// Checks a local's name, and reports an empty literal, or a `let` that starts as `null`, declared without a type,
/// which has no type to give the local. Swift refuses an empty collection literal or `nil` without a type for the same
/// reason.
#[inline]
pub fn check_local_declaration(local_declaration: &LocalDeclaration, context: &mut Context<'_, '_, '_>) {
    check_local_name(local_declaration.name.value, local_declaration.name.span, "local", context);

    if local_declaration.hint.is_some() {
        return;
    }

    let (start, hint, value) = match local_declaration.value {
        Expression::Array(literal) if literal.elements.is_empty() && literal.colon.is_some() => {
            ("An empty literal", "Map<TKey, TValue>", "[:]")
        }
        Expression::Array(literal) if literal.elements.is_empty() => ("An empty literal", "List<T>", "[]"),
        // A `const` never takes another value, so `null` is all of its type.
        Expression::Literal(Literal::Null(_)) if !local_declaration.is_const() => ("A null start", "T?", "null"),
        _ => return,
    };

    let keyword = if local_declaration.is_const() { "const " } else { "" };
    let name = BytesDisplay(local_declaration.name.value);

    context.report(
        Issue::error(format!("{start} needs a type: write `{keyword}{hint} {name} = {value}`.")).with_annotation(
            Annotation::primary(local_declaration.value.span()).with_message("Declared without a type."),
        ),
    );
}

#[inline]
pub fn check_for_of(for_of: &ForOf, context: &mut Context<'_, '_, '_>) {
    for name in for_of.target.names() {
        check_local_name(name.value, name.span, "loop variable", context);
    }
}

#[inline]
pub fn check_type_pattern(type_pattern: &TypePattern, context: &mut Context<'_, '_, '_>) {
    if let Some(variable) = &type_pattern.variable {
        check_local_name(variable.value, variable.span, "pattern variable", context);
    }
}

#[inline]
pub fn check_try_catch_clause(try_catch_clause: &TryCatchClause, context: &mut Context<'_, '_, '_>) {
    if let Some(variable) = &try_catch_clause.variable
        && !variable.name.starts_with(b"$")
    {
        check_local_name(variable.name, variable.span, "catch variable", context);
    }
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
            BindingError::OutOfScope {
                name,
                local: Local { declaration, kind: LocalKind::Pattern { test, negated } },
            } => {
                let name_text = BytesDisplay(context.get_code_snippet(name));
                let test_text = BytesDisplay(context.get_code_snippet(test));
                let holds = if negated { "false" } else { "true" };

                Issue::error(format!("`{name_text}` exists only where `{test_text}` is {holds}."))
                    .with_annotation(Annotation::primary(name).with_message("Used here."))
                    .with_annotation(Annotation::secondary(declaration).with_message("Declared here."))
                    .with_help(format!(
                        "A pattern variable holds a value only where its pattern matches. Test the value again here, or declare a local for `{name_text}` before the test."
                    ))
            }
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

/// Checks the name of a PHP# class or enum against the names the engine reserves, beyond the keywords the PHP checks
/// reject, and against the `__Something__` names spec section 27 removes.
#[inline]
pub fn check_class_name(class_name: &LocalIdentifier, context: &mut Context<'_, '_, '_>) {
    let is_keyword = RESERVED_KEYWORDS
        .iter()
        .chain(&SOFT_RESERVED_KEYWORDS_MINUS_SYMBOL_ALLOWED)
        .any(|keyword| keyword.eq_ignore_ascii_case(class_name.value));

    if is_reserved_class_name(class_name.value) && !is_keyword {
        let name = BytesDisplay(class_name.value);

        context.report(
            Issue::error(format!("Cannot use `{name}` as a class name: it is reserved."))
                .with_annotation(Annotation::primary(class_name.span).with_message("Class declared here."))
                .with_note("PHP# reserves this name for a type."),
        );
    } else {
        check_declared_name(class_name.value, class_name.span, context);
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
                    "Cannot import `{full_name}` as `{short_name}`: PHP# reserves `{short_name}` for a type."
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
    } else if function.eq_ignore_ascii_case(b"assert")
        && let [Argument::Positional(argument)] = function_call.argument_list.arguments.as_slice()
        && argument.ellipsis.is_some()
    {
        context.report(
            Issue::error("Cannot use positional argument after argument unpacking.")
                .with_annotation(
                    Annotation::primary(argument.span()).with_message("The only argument of `assert` is a spread."),
                )
                .with_note("PHP compiles `assert` with one argument by adding its text as a positional description.")
                .with_help("Pass the condition without `...`, or pass the description by name after the spread."),
        );
    }
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
/// member name spans of the accesses it checks go into the context's `slice_members`, because each access has its own
/// member name and an access's own span grows with the chain before it.
///
/// A chain rooted at a class reaches its constants, enum cases and static members, and one rooted at a class value,
/// `typeof(X)` or a local holding one, reaches those of the class it holds. One file cannot tell a full name,
/// `App.Status`, from a class and its member, `Status.Active`, so the analyzer reports a full name.
fn check_member_access(object: &Expression, member: &ClassLikeMemberSelector, context: &mut Context<'_, '_, '_>) {
    if !context.slice_members.insert(member.span()) {
        return;
    }

    let mut root = object;
    while let Expression::Access(Access::Property(property)) = root
        && let ClassLikeMemberSelector::Identifier(name) = &property.property
    {
        context.slice_members.insert(name.span);
        root = property.object;
    }
}

/// A class or an enum a PHP# file declares. The slice's class rules treat both alike.
#[derive(Clone, Copy)]
struct ClassLike<'ast, 'arena> {
    name: &'ast LocalIdentifier<'arena>,
    span: Span,
    members: &'ast Sequence<'arena, ClassLikeMember<'arena>>,
}

/// The classes, enums and imports a PHP# file declares at its top level and in its first namespace, in source order.
fn declarations<'ast, 'arena>(
    program: &'ast Program<'arena>,
) -> (Vec<ClassLike<'ast, 'arena>>, Vec<&'ast UseItem<'arena>>) {
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

fn collect_declarations<'ast, 'arena>(
    statement: &'ast Statement<'arena>,
    classes: &mut Vec<ClassLike<'ast, 'arena>>,
    imports: &mut Vec<&'ast UseItem<'arena>>,
) {
    match statement {
        Statement::Class(class) => {
            classes.push(ClassLike { name: &class.name, span: class.span(), members: &class.members });
        }
        Statement::Enum(r#enum) => {
            classes.push(ClassLike { name: &r#enum.name, span: r#enum.span(), members: &r#enum.members });
        }
        Statement::Use(Use { items: UseItems::Sequence(sequence), .. }) => imports.extend(sequence.items.iter()),
        _ => {}
    }
}

/// Checks the name of a PHP# local or parameter, which runs as the PHP variable of the same name, and a
/// `__Something__` name, which spec section 27 removes.
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
    } else {
        check_declared_name(name, span, context);
    }
}

/// PHP# reserves PHP's type names and `Any`, its name for `mixed`.
fn is_reserved_class_name(name: &[u8]) -> bool {
    RESERVED_CLASS_NAMES.iter().chain(&[ANY]).any(|reserved| reserved.eq_ignore_ascii_case(name))
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

/// Checks the object of `object?.member`. A class is never null, so `?.` after a class is an error that names `.`. A
/// superglobal or a `__Something__` name binds as a class there too, and the walk refuses it at the object.
fn check_null_safe_object(
    access: Span,
    object: &Expression,
    member: &ClassLikeMemberSelector,
    context: &mut Context<'_, '_, '_>,
) -> Option<Place> {
    if let Expression::ConstantAccess(class) = object
        && context.names.binding(&class.name) == Some(Binding::Class)
        && superglobal_span(object, context).is_none()
        && magic_name(object, context).is_none()
        && let ClassLikeMemberSelector::Identifier(member) = member
    {
        let class = BytesDisplay(class.name.value());

        context.report(
            Issue::error(format!(
                "`{class}` is a class, which is never null: write `{class}.{}`.",
                BytesDisplay(member.value)
            ))
            .with_annotation(Annotation::primary(access).with_message("Null-safe access written here.")),
        );

        return None;
    }

    Some(Place::Body)
}

fn report_bare_member(span: Span, name: &[u8], context: &mut Context<'_, '_, '_>) {
    let is_case =
        |member: &ClassLikeMember| matches!(member, ClassLikeMember::EnumCase(case) if case.item.name().value == name);
    let class = enclosing_class(context.program, span);
    let message = if let Some(r#enum) = class.filter(|class| class.members.iter().any(is_case)) {
        format!(
            "Write `{}.{}`: an enum case is reached through its enum's name.",
            BytesDisplay(r#enum.name.value),
            BytesDisplay(name)
        )
    } else if let Some(class) = class.filter(|class| is_static_member(*class, name)) {
        format!(
            "Write `{}.{}`: a static member is reached through its class name.",
            BytesDisplay(class.name.value),
            BytesDisplay(name)
        )
    } else {
        let (name, call) = match enclosing_class_method(context.program, span, name) {
            Some(method) => (BytesDisplay(method.name.value), "()"),
            None => (BytesDisplay(name), ""),
        };

        match enclosing_static_method_class(context.program, span) {
            Some(class) => format!(
                "Write `{}.{name}{call}`: a static method reaches the members of its class through the class name.",
                BytesDisplay(class.name.value)
            ),
            None => format!("Write `this.{name}{call}`: members of the same object are always written with `this.`."),
        }
    };

    context.report(Issue::error(message).with_annotation(Annotation::primary(span).with_message("Used here.")));
}

/// The class of the static method whose body holds `span`, which has no `this`.
fn enclosing_static_method_class<'ast, 'arena>(
    program: &'ast Program<'arena>,
    span: Span,
) -> Option<ClassLike<'ast, 'arena>> {
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

/// Refuses a lambda that captures a loop variable that code changes. Spec section 3 gives each loop pass its own
/// variable, and a lambda captures the variable itself, so the engine would capture a changing one by reference, and
/// every pass's lambda would share the one PHP variable the loop reuses.
fn check_lambda_captures(lambda: Node<'_, '_>, context: &mut Context<'_, '_, '_>) {
    let captures = context.names.captures(&lambda.span()).to_vec();
    for (_, local) in captures {
        if !context.names.is_written(&local) || !is_loop_variable(context.program, &local) {
            continue;
        }

        let uses = lambda.filter_map(|node| match node {
            Node::ConstantAccess(access) if context.names.binding(&access.name) == Some(Binding::Local(local)) => {
                Some(access.span())
            }
            _ => None,
        });
        report_not_supported(
            uses.first().copied().unwrap_or_else(|| lambda.span()),
            "capture of a loop variable that changes",
            "Each loop pass has its own loop variable, which the engine cannot give a lambda yet when code changes it. Copy it into a `const` in the loop body, and capture that.",
            context,
        );
    }
}

/// Whether `local` is the counter a `for` declares or a variable a `for … of` declares.
fn is_loop_variable(program: &Program, local: &Local) -> bool {
    let declares = |node: &Node<'_, '_>| match node {
        Node::For(r#for) => {
            r#for.declaration.as_ref().is_some_and(|declaration| declaration.name.span == local.declaration)
        }
        Node::ForOf(for_of) => for_of.target.names().into_iter().any(|name| name.span == local.declaration),
        _ => false,
    };

    !Node::Program(program).filter_map(|node| declares(node).then_some(())).is_empty()
}

/// Whether `name` is a constant, or a static field or property, of `class`.
fn is_static_member(class: ClassLike, name: &[u8]) -> bool {
    is_static_property(class, name)
        || class.members.iter().any(|member| {
            matches!(member, ClassLikeMember::Constant(constant)
                if constant.items.iter().any(|item| item.name.value == name))
        })
}

/// Whether `name` is a static field or property of `class`.
fn is_static_property(class: ClassLike, name: &[u8]) -> bool {
    class.members.iter().any(|member| {
        matches!(member, ClassLikeMember::Property(property)
            if property.modifiers().contains_static()
                && property.variables().iter().any(|variable| variable.name == name))
    })
}

/// Reports `Class.name` in a constant expression when this file declares `name` as a static field or property of
/// `Class`, because PHP cannot read a static property there.
fn report_static_member_in_constant(
    class: &ConstantAccess,
    member: &LocalIdentifier,
    span: Span,
    context: &mut Context<'_, '_, '_>,
) {
    let declared = declarations(context.program).0.into_iter().any(|declared| {
        declared.name.value.eq_ignore_ascii_case(class.name.value()) && is_static_property(declared, member.value)
    });
    if !declared {
        return;
    }

    context.report(
        Issue::error(format!(
            "`{}.{}` is a static member, which a constant value cannot read.",
            BytesDisplay(class.name.value()),
            BytesDisplay(member.value)
        ))
        .with_annotation(Annotation::primary(span).with_message("Read here."))
        .with_note("A default, a constant's value and a constant initial value read constants and enum cases only."),
    );
}

/// The class or enum of a PHP# file whose body holds `span`, in any of the file's namespaces, because the walk checks
/// the classes of a namespace it refuses too.
fn enclosing_class<'ast, 'arena>(program: &'ast Program<'arena>, span: Span) -> Option<ClassLike<'ast, 'arena>> {
    let mut classes = Vec::new();
    for statement in file_statements(program) {
        collect_declarations(statement, &mut classes, &mut Vec::new());
    }

    classes.into_iter().find(|class| class.span.contains(&span.start))
}

/// The name of the class, interface or enum of a PHP# file whose body holds `span`, in any of the file's namespaces.
fn enclosing_class_like_name<'arena>(program: &Program<'arena>, span: Span) -> Option<&'arena [u8]> {
    file_statements(program).find_map(|statement| match statement {
        Statement::Class(class) if class.span().contains(&span.start) => Some(class.name.value),
        Statement::Interface(interface) if interface.span().contains(&span.start) => Some(interface.name.value),
        Statement::Enum(r#enum) if r#enum.span().contains(&span.start) => Some(r#enum.name.value),
        _ => None,
    })
}
