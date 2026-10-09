use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::TType;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_sole_backed_enum;
use mago_codex::ttype::union::TUnion;
use mago_names::ResolvedNames;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::dialect::Dialect;
use mago_word::Word;
use mago_word::word;

use super::inline::InlineForm;
use super::inline::InlineForms;
use super::inline::key;

/// The checker's types for one file. The lowering reads a type only through these queries, so `--assert-types` can
/// turn each answer it used into a runtime guard.
pub struct Types<'analysis> {
    /// The file's resolved names, which name the class of a `Class.m()` call.
    names: ResolvedNames<'analysis>,
    artifacts: &'analysis AnalysisArtifacts,
    codebase: &'analysis CodebaseMetadata,
    inline_forms: &'analysis InlineForms,
}

/// The declaration a class or member name resolves to, as the checker found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Declaration {
    pub(crate) kind: DeclarationKind,
    /// The class that declares it: a class itself, or the class an inherited member is declared in.
    pub(crate) class: Word,
    /// The member's name, or a class's own name.
    pub(crate) name: Word,
    /// Whether code outside the class may use it. A class is public.
    pub(crate) public: bool,
}

/// What a declaration is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeclarationKind {
    Class,
    Interface,
    Enum,
    Constant,
    EnumCase,
    StaticProperty,
    /// `typed` is whether the property's root declaration, the one at the bottom of its chain of overrides, has a type.
    /// PHP refuses a type on an override of an untyped property, so every override above an untyped root runs untyped.
    Property {
        typed: bool,
    },
    StaticMethod,
    /// `overridable` is whether a subclass can declare its own body, which keeps a call from being inlined.
    Method {
        overridable: bool,
    },
}

impl<'analysis> Types<'analysis> {
    pub(crate) fn new(
        names: ResolvedNames<'analysis>,
        artifacts: &'analysis AnalysisArtifacts,
        codebase: &'analysis CodebaseMetadata,
        inline_forms: &'analysis InlineForms,
    ) -> Self {
        Self { names, artifacts, codebase, inline_forms }
    }

    pub(crate) fn names(&self) -> &ResolvedNames<'analysis> {
        &self.names
    }

    /// The type the analysis gave `expression`.
    pub(crate) fn expression_type(&self, expression: &Expression) -> &'analysis TUnion {
        self.artifacts.get_expression_type(expression).unwrap_or_else(|| {
            unreachable!(
                "the analysis types every expression of a file the checker accepted, not {:?}",
                expression.span()
            )
        })
    }

    /// The declaration the fully qualified class name `class` resolves to.
    pub(crate) fn class_declaration(&self, class: &[u8]) -> Declaration {
        let metadata = self.codebase.get_class_like(class).unwrap_or_else(|| {
            unreachable!("the checker refuses the unknown class `{}`", String::from_utf8_lossy(class))
        });

        let kind = if metadata.kind.is_interface() {
            DeclarationKind::Interface
        } else if metadata.kind.is_enum() {
            DeclarationKind::Enum
        } else {
            DeclarationKind::Class
        };

        Declaration { kind, class: metadata.original_name, name: metadata.original_name, public: true }
    }

    /// The bounds of the fully qualified class name `class` when it is a PHP# generic class, whose objects carry their
    /// type arguments: the [`type_text`] of each type parameter's bound in declaration order, joined by `, `.
    pub(crate) fn bounds(&self, class: &[u8]) -> Option<String> {
        let metadata = self.codebase.get_class_like(class)?;
        if !metadata.flags.is_sharp() || metadata.template_types.is_empty() {
            return None;
        }

        let bounds: Vec<String> =
            metadata.template_types.values().map(|template| type_text(&template.constraint, self.codebase)).collect();

        Some(bounds.join(", "))
    }

    /// The type arguments the header of the fully qualified class name `class` gives each generic parent and
    /// interface, as a type text list of those classes with their type arguments, each type parameter of `class`
    /// written as `$` and its index, and sorted by their text: `App.PaginatedList<App.Order>` for
    /// `OrderPage : PaginatedList<Order>`, and `App.Base<List<$0>>` for `Sub<T> : Base<List<T>>`. None when the header
    /// gives none.
    pub(crate) fn header(&self, class: &[u8]) -> Option<String> {
        let metadata = self.codebase.get_class_like(class)?;
        let mut ancestors: Vec<String> = metadata
            .template_extended_offsets
            .iter()
            .map(|(ancestor, arguments)| {
                let arguments: Vec<String> =
                    arguments.iter().map(|argument| text(argument, self.codebase, Parameter::Index)).collect();

                format!("{}<{}>", class_text(*ancestor, self.codebase), arguments.join(", "))
            })
            .collect();
        ancestors.sort_unstable();

        (!ancestors.is_empty()).then(|| ancestors.join(", "))
    }

    /// The type arguments the `new` at `span` gives the fully qualified class name `class`, as the checker found them,
    /// when it is a PHP# generic class: the type text of each in declaration order, joined by `, `. A type parameter of
    /// the class the `new` is in is written as `$` and its index, which the engine replaces with `this`'s type argument
    /// at that index, and one of the method it is in as `#` and its index, which the engine replaces with the method's
    /// own type argument. A lambda captures its method's type arguments, so a `new` in it writes them the same way.
    pub(crate) fn type_arguments(&self, class: &[u8], span: Span) -> Option<String> {
        self.bounds(class)?;
        let arguments =
            self.artifacts.inferred_type_arguments.get(&(span.start.offset, span.end.offset)).unwrap_or_else(|| {
                unreachable!("the analysis records the type arguments of every generic `new`, not {span:?}")
            });

        Some(self.argument_texts(arguments))
    }

    /// The type arguments the generic method call at `span` gives the method, as [`Self::type_arguments`] writes those
    /// of a `new`. None when the method declares no type parameter, or PHP declares it, writing its type parameters in
    /// docblocks the engine never reads.
    /// `class` is the class the call is written in, whose method `Self.m()` and whose parent's `super.m()` call.
    pub(crate) fn call_type_arguments(&self, call: &Expression, class: &[u8]) -> Option<String> {
        let span = call.span();
        let arguments = self.artifacts.inferred_type_arguments.get(&(span.start.offset, span.end.offset))?;
        let object = match call {
            Expression::Call(Call::Method(call)) => call.object,
            Expression::Call(Call::NullSafeMethod(call)) => call.object,
            _ => unreachable!("only a method call takes type arguments, not {span:?}"),
        };
        let callee = match object {
            Expression::Self_(_) => word(class),
            Expression::Parent(_) => self
                .codebase
                .get_class_like(class)
                .and_then(|class| class.direct_parent_class)
                .unwrap_or_else(|| unreachable!("the checker refuses `super` in a class without a parent")),
            _ => self.call_target(call).class,
        };
        if !self.codebase.get_class_like(callee.as_bytes()).is_some_and(|callee| callee.flags.is_sharp()) {
            return None;
        }

        Some(self.argument_texts(arguments))
    }

    fn argument_texts(&self, arguments: &[TUnion]) -> String {
        arguments.iter().map(|argument| text(argument, self.codebase, Parameter::Index)).collect::<Vec<_>>().join(", ")
    }

    /// The metadata of the method `method` of the fully qualified class name `class` that the engine reads on a call:
    /// the bounds of its own type parameters, which a call from plain PHP gives it, and a type text list with one entry
    /// per parameter that a call from plain PHP checks its argument against. An entry is the parameter's type when part
    /// of it is a class with type arguments or a type parameter, and `Any?` otherwise. Each half is None when the method
    /// declares no type parameter, or no parameter needs a check.
    pub(crate) fn method_metadata(&self, class: &[u8], method: &[u8]) -> (Option<String>, Option<String>) {
        let Some(metadata) = self.codebase.get_method(class, method) else {
            return (None, None);
        };

        let bounds: Vec<String> = metadata
            .template_types
            .values()
            .map(|template| text(&template.constraint, self.codebase, Parameter::Bound))
            .collect();
        let checked = |r#type: &TUnion| {
            r#type.types.iter().any(|atomic| match atomic {
                TAtomic::Object(TObject::Named(object)) => {
                    object.get_type_parameters().is_some_and(|arguments| !arguments.is_empty())
                }
                TAtomic::GenericParameter(_) => true,
                _ => false,
            })
        };
        let parameters: Vec<Option<String>> = metadata
            .parameters
            .iter()
            .map(|parameter| {
                parameter
                    .type_metadata
                    .as_ref()
                    .map(|r#type| &r#type.type_union)
                    .filter(|r#type| checked(r#type))
                    .map(|r#type| text(r#type, self.codebase, Parameter::Index))
            })
            .collect();

        (
            (!bounds.is_empty()).then(|| bounds.join(", ")),
            parameters.iter().any(Option::is_some).then(|| {
                parameters
                    .into_iter()
                    .map(|parameter| parameter.unwrap_or_else(|| "Any?".to_owned()))
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
        )
    }

    /// The fully qualified name of the one backed enum every value of `r#type` but `null` is a case of, as
    /// [`get_sole_backed_enum`] finds it.
    pub(crate) fn backed_enum(&self, r#type: &TUnion) -> Option<&'analysis [u8]> {
        get_sole_backed_enum(r#type, self.codebase, Dialect::Sharp)
            .map(|backed_enum| backed_enum.original_name.as_bytes())
    }

    /// The declaration `member` of the fully qualified class name `class` resolves to when code reads it: an enum case,
    /// a constant, a property, then a method, which the read takes as a first-class callable. Only when the class
    /// declares none of them is it a property its `__get` serves. A call is [`Self::call_target`]'s.
    pub(crate) fn member_declaration(&self, class: &[u8], member: &[u8]) -> Declaration {
        let declared = |kind, public| Declaration { kind, class: word(class), name: word(member), public };

        if self.codebase.get_enum_case(class, member).is_some() {
            declared(DeclarationKind::EnumCase, true)
        } else if let Some(constant) = self.codebase.get_class_constant(class, member) {
            declared(DeclarationKind::Constant, constant.visibility.is_public())
        } else if let Some(declaration) = self.property_declaration(class, member) {
            declaration
        } else if let Some(declaration) = self.method_declaration(class, member) {
            declaration
        } else if self.codebase.method_exists(class, b"__get") {
            declared(DeclarationKind::Property { typed: false }, true)
        } else {
            unreachable!(
                "the checker refuses `{}.{}`, which names no member",
                String::from_utf8_lossy(class),
                String::from_utf8_lossy(member)
            )
        }
    }

    /// The declaration the method call `call`, null-safe or not, runs, as PHP finds it. `Class.m()` and a class value's
    /// `type.m()` run the class's method, or else a static method its `__callStatic` serves. `object.m()` runs the
    /// receiver's method, or else its property holding a function, as spec section 14 calls one, or else a method its
    /// `__call` serves. Every class the receiver can be has the same kind of member, and `class` is the first's.
    /// `Self.m()` and `super.m()` lower to `static::` and `parent::`, and no caller asks their target.
    pub(crate) fn call_target(&self, call: &Expression) -> Declaration {
        let (object, method, class) = match call {
            Expression::Call(Call::Method(call)) => (call.object, &call.method, self.names.static_call_class(call)),
            Expression::Call(Call::NullSafeMethod(call)) => (call.object, &call.method, None),
            _ => unreachable!("only a method call has a typed call target yet"),
        };
        let ClassLikeMemberSelector::Identifier(method) = method else {
            unreachable!("check_slice refuses the method name `{method}`");
        };
        if let Some(class) = class {
            return self.static_call_declaration(self.names.get(&class.name), method.value);
        }
        if matches!(object, Expression::Self_(_) | Expression::Parent(_)) {
            unreachable!(
                "`Self.m()` and `super.m()` lower to `static::` and `parent::`, and no caller asks their target"
            );
        }

        let r#type = self.expression_type(object);
        let declarations: Vec<Declaration> = if let Some(classes) = class_value_classes(r#type) {
            classes.iter().map(|class| self.static_call_declaration(class, method.value)).collect()
        } else {
            receiver_classes(r#type)
                .unwrap_or_else(|| unreachable!("the lowering asks only for a receiver whose type names classes"))
                .iter()
                .map(|class| self.call_declaration(class, method.value))
                .collect()
        };
        agreed_kind(declarations.iter().map(|declaration| declaration.kind));

        declarations[0]
    }

    /// The full name of the constant the read `constant` reaches, as the analysis found it: the constant of that name
    /// in the file's namespace, or else the global one.
    pub(crate) fn constant_target(&self, constant: &ConstantAccess) -> Word {
        self.codebase
            .get_constant_or_global(self.names.get(constant), constant.name.value())
            .unwrap_or_else(|| unreachable!("the checker refuses the undefined constant `{}`", constant.name))
            .name
    }

    /// The inline form of the standard library method `declaration` names, if it has one.
    pub(crate) fn inline_form(&self, declaration: &Declaration) -> Option<&'analysis InlineForm> {
        if !matches!(declaration.kind, DeclarationKind::Method { .. } | DeclarationKind::StaticMethod) {
            return None;
        }

        self.inline_forms.get(&key(declaration.class.as_bytes(), declaration.name.as_bytes()))
    }

    /// What a static call of `class`'s `method` runs: its method, or else a static method its `__callStatic` serves.
    fn static_call_declaration(&self, class: &[u8], method: &[u8]) -> Declaration {
        let served = || Declaration {
            kind: DeclarationKind::StaticMethod,
            class: word(class),
            name: word(method),
            public: true,
        };

        self.method_declaration(class, method)
            .or_else(|| self.codebase.method_exists(class, b"__callStatic").then(served))
            .unwrap_or_else(|| {
                unreachable!(
                    "the checker refuses `{}.{}()`, which names no method",
                    String::from_utf8_lossy(class),
                    String::from_utf8_lossy(method)
                )
            })
    }

    /// What `class`'s call of `method` runs: its method, its property holding a function, or else a method its
    /// `__call` serves.
    fn call_declaration(&self, class: &[u8], method: &[u8]) -> Declaration {
        let called = || Declaration {
            kind: DeclarationKind::Method { overridable: true },
            class: word(class),
            name: word(method),
            public: true,
        };

        self.method_declaration(class, method)
            .or_else(|| self.property_declaration(class, method))
            .or_else(|| self.codebase.method_exists(class, b"__call").then(called))
            .unwrap_or_else(|| {
                unreachable!(
                    "the checker refuses `{}.{}()`, which names no method or property",
                    String::from_utf8_lossy(class),
                    String::from_utf8_lossy(method)
                )
            })
    }

    /// The type of the keys a value of `r#type` holds, or none when part of it is no `Map` or `List`.
    pub(crate) fn map_key_type(&self, r#type: &TUnion) -> Option<TUnion> {
        let mut key_type = None;
        for atomic in r#type.types.iter() {
            let TAtomic::Array(array) = atomic else {
                return None;
            };
            let (key, _) = get_array_parameters(array, self.codebase);
            key_type = Some(add_optional_union_type(key, key_type.as_ref(), self.codebase));
        }

        key_type
    }

    /// The property `class` declares or inherits by the name `property`, if any.
    fn property_declaration(&self, class: &[u8], property: &[u8]) -> Option<Declaration> {
        let variable = [b"$", property].concat();
        let metadata = self.codebase.get_declaring_property(class, &variable)?;
        let kind = if metadata.flags.is_static() {
            DeclarationKind::StaticProperty
        } else {
            DeclarationKind::Property {
                typed: self.root_property(class, &variable).type_declaration_metadata.is_some(),
            }
        };

        Some(Declaration {
            kind,
            class: self.codebase.get_declaring_property_class(class, &variable).unwrap_or_else(|| word(class)),
            name: word(property),
            public: metadata.read_visibility.is_public(),
        })
    }

    /// The root declaration of the property `variable` that `class` declares or inherits: the one in the class whose
    /// parent does not declare it.
    fn root_property(&self, class: &[u8], variable: &[u8]) -> &'analysis PropertyMetadata {
        let declaring = |class: &[u8]| self.codebase.get_declaring_property_class(class, variable);
        let mut root =
            declaring(class).unwrap_or_else(|| unreachable!("`{}` declares or inherits the property", word(class)));
        while let Some(parent) = self
            .codebase
            .get_class_like(root.as_bytes())
            .and_then(|metadata| metadata.direct_parent_class)
            .and_then(|parent| declaring(parent.as_bytes()))
        {
            root = parent;
        }

        self.codebase
            .get_declaring_property(root.as_bytes(), variable)
            .unwrap_or_else(|| unreachable!("`{root}` declares the property"))
    }

    /// The method `class` declares or inherits by the name `method`, if any.
    fn method_declaration(&self, class: &[u8], method: &[u8]) -> Option<Declaration> {
        let metadata = self.codebase.get_declaring_method(class, method)?.method_metadata.as_ref()?;
        let kind = if metadata.is_static {
            DeclarationKind::StaticMethod
        } else {
            DeclarationKind::Method { overridable: !metadata.is_final }
        };
        let declaring =
            self.codebase.get_declaring_method_identifier(&MethodIdentifier::new(word(class), word(method)));

        Some(Declaration {
            kind,
            class: declaring.get_class_name(),
            name: word(method),
            public: metadata.visibility.is_public(),
        })
    }
}

/// The fully qualified names of the classes a value of `r#type` can be, leaving out `null`, or none when part of the
/// type is no class, such as a collection.
pub(crate) fn receiver_classes(r#type: &TUnion) -> Option<Vec<&[u8]>> {
    let classes: Vec<&[u8]> = r#type
        .types
        .iter()
        .filter(|atomic| !atomic.is_null())
        .map(|atomic| match atomic {
            TAtomic::Object(TObject::Named(object)) => Some(object.name.as_bytes()),
            TAtomic::Object(TObject::Enum(object)) => Some(object.name.as_bytes()),
            _ => None,
        })
        .collect::<Option<_>>()?;

    (!classes.is_empty()).then_some(classes)
}

/// The fully qualified names of the classes a class value of `r#type` holds, leaving out `null`, or none when part of
/// the type is no class value. A `Class<T>` value holds `T` or a subclass, which has the same kind of member.
pub(crate) fn class_value_classes(r#type: &TUnion) -> Option<Vec<&[u8]>> {
    let classes: Vec<&[u8]> = r#type
        .types
        .iter()
        .filter(|atomic| !atomic.is_null())
        .map(|atomic| match atomic {
            TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::Literal { value })) => Some(value.as_bytes()),
            TAtomic::Scalar(TScalar::ClassLikeString(
                TClassLikeString::Generic { constraint, .. } | TClassLikeString::OfType { constraint, .. },
            )) => match constraint.as_ref() {
                TAtomic::Object(TObject::Named(object)) => Some(object.name.as_bytes()),
                TAtomic::Object(TObject::Enum(object)) => Some(object.name.as_bytes()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Option<_>>()?;

    (!classes.is_empty()).then_some(classes)
}

/// The kind of member every class a receiver can be declares, which the checker requires to be one kind. Its details,
/// such as whether a property is typed, may differ between the classes.
pub(crate) fn agreed_kind(mut kinds: impl Iterator<Item = DeclarationKind>) -> DeclarationKind {
    let kind = kinds.next().unwrap_or_else(|| unreachable!("a receiver's type names at least one class"));
    if kinds.any(|other| std::mem::discriminant(&other) != std::mem::discriminant(&kind)) {
        unreachable!("the checker refuses a member whose kind differs across the receiver's classes");
    }

    kind
}

/// How a type text writes a type parameter.
#[derive(Clone, Copy)]
enum Parameter {
    /// By its name.
    Name,
    /// A class's as `$` and its index among its class's type parameters, `$0` for the first, and a method's as `#` and
    /// its index among the method's.
    Index,
    /// A class's as [`Parameter::Index`] writes it, and a method's as its bound, which a call from plain PHP gives it.
    Bound,
}

/// `r#type` in the one PHP# spelling the engine parses, task 090's type text: a class by its full dotted name as it is
/// declared, the built-in types, `List<T>`, `Map<K, V>`, `Iterable<T>`, `Class<T>`, `Function<R(P1, P2)>`, a type
/// parameter by its name, `T?`, `(A|B)?`, `A & B`, and an intersection in parentheses inside a union or a nullable
/// type, `(A & B)|C` and `(A & B)?`. Union and intersection members are sorted by their text, and a space comes only
/// after a comma and around `&`.
pub(crate) fn type_text(r#type: &TUnion, codebase: &CodebaseMetadata) -> String {
    text(r#type, codebase, Parameter::Name)
}

/// [`type_text`], with each type parameter of a class written as `parameter` says.
fn text(r#type: &TUnion, codebase: &CodebaseMetadata, parameter: Parameter) -> String {
    if let Some(TAtomic::Mixed(mixed)) = r#type.types.iter().find(|atomic| atomic.is_mixed()) {
        return if mixed.is_non_null() { "Any" } else { "Any?" }.to_owned();
    }

    let mut members: Vec<(String, bool)> = r#type
        .types
        .iter()
        .filter(|atomic| !atomic.is_null())
        .map(|atomic| intersection_text(atomic, codebase, parameter))
        .collect();
    members.sort_unstable();
    members.dedup();
    let grouped =
        |(text, intersection): &(String, bool)| if *intersection { format!("({text})") } else { text.clone() };
    let union = || members.iter().map(grouped).collect::<Vec<_>>().join("|");

    match (r#type.has_null(), members.as_slice()) {
        (false, [(member, _)]) => member.clone(),
        (false, _) => union(),
        (true, []) => "null".to_owned(),
        (true, [member]) => format!("{}?", grouped(member)),
        (true, _) => format!("({})?", union()),
    }
}

/// `atomic` and each type it intersects, `A & B` with its members sorted by their text, and whether it intersects any.
fn intersection_text(atomic: &TAtomic, codebase: &CodebaseMetadata, parameter: Parameter) -> (String, bool) {
    let Some(intersected) = atomic.get_intersection_types().filter(|intersected| !intersected.is_empty()) else {
        return (atomic_text(atomic, codebase, parameter), false);
    };

    let mut members: Vec<String> =
        std::iter::once(atomic).chain(intersected).map(|member| atomic_text(member, codebase, parameter)).collect();
    members.sort_unstable();
    members.dedup();

    (members.join(" & "), true)
}

fn atomic_text(atomic: &TAtomic, codebase: &CodebaseMetadata, parameter: Parameter) -> String {
    let type_text = |r#type: &TUnion| text(r#type, codebase, parameter);
    let list = |types: &mut dyn Iterator<Item = &TUnion>| types.map(type_text).collect::<Vec<_>>().join(", ");
    let parameter_text =
        |name: Word, defining_entity: GenericParent, bound: &dyn Fn() -> String| match (parameter, defining_entity) {
            (Parameter::Name, _) => name.to_string(),
            (Parameter::Index | Parameter::Bound, GenericParent::ClassLike(class)) => {
                let index = codebase
                    .get_class_like(class.as_bytes())
                    .and_then(|class| class.template_types.get_index_of(&name))
                    .unwrap_or_else(|| unreachable!("a class declares each of its type parameters, not `{name}`"));

                format!("${index}")
            }
            (Parameter::Index, GenericParent::FunctionLike((class, method))) => {
                let index = codebase
                    .get_method(class.as_bytes(), method.as_bytes())
                    .and_then(|method| method.template_types.get_index_of(&name))
                    .unwrap_or_else(|| unreachable!("a method declares each of its type parameters, not `{name}`"));

                format!("#{index}")
            }
            (Parameter::Bound, GenericParent::FunctionLike(_)) => bound(),
        };

    match atomic {
        TAtomic::Scalar(TScalar::Integer(_)) => "int".to_owned(),
        TAtomic::Scalar(TScalar::Float(_)) => "float".to_owned(),
        TAtomic::Scalar(TScalar::Bool(_)) => "bool".to_owned(),
        TAtomic::Scalar(TScalar::String(_)) => "string".to_owned(),
        TAtomic::Void => "void".to_owned(),
        TAtomic::Object(TObject::Any) => "Object".to_owned(),
        TAtomic::Object(TObject::Enum(object)) => class_text(object.name, codebase),
        TAtomic::Object(TObject::Named(object)) => match object.get_type_parameters() {
            Some(arguments) if !arguments.is_empty() => {
                format!("{}<{}>", class_text(object.name, codebase), list(&mut arguments.iter()))
            }
            _ => class_text(object.name, codebase),
        },
        TAtomic::Array(TArray::List(array)) => format!("List<{}>", type_text(&array.element_type)),
        TAtomic::Array(TArray::Keyed(array)) => {
            let (key, value) = array.parameters.as_ref().unwrap_or_else(|| {
                unreachable!("PHP# writes a Map with its key and value types, not `{}`", atomic.get_id())
            });

            format!("Map<{}>", list(&mut [key.as_ref(), value.as_ref()].into_iter()))
        }
        TAtomic::Iterable(iterable) => format!("Iterable<{}>", type_text(iterable.get_value_type())),
        TAtomic::GenericParameter(generic) => {
            parameter_text(generic.parameter_name, generic.defining_entity, &|| type_text(&generic.constraint))
        }
        TAtomic::Scalar(TScalar::ClassLikeString(class_value)) => {
            let class = match class_value {
                TClassLikeString::Literal { value } => class_text(*value, codebase),
                TClassLikeString::OfType { constraint, .. } => atomic_text(constraint, codebase, parameter),
                TClassLikeString::Generic { parameter_name, defining_entity, constraint, .. } => {
                    parameter_text(*parameter_name, *defining_entity, &|| atomic_text(constraint, codebase, parameter))
                }
                TClassLikeString::Any { .. } => "Object".to_owned(),
            };

            format!("Class<{class}>")
        }
        TAtomic::Callable(TCallable::Signature(signature)) => {
            let written = |r#type: Option<&TUnion>| r#type.map_or_else(|| "Any?".to_owned(), type_text);
            let parameters: Vec<String> =
                signature.get_parameters().iter().map(|parameter| written(parameter.get_type_signature())).collect();

            format!("Function<{}({})>", written(signature.get_return_type()), parameters.join(", "))
        }
        _ => unreachable!("the checker refuses a type PHP# can't write, not `{}`", atomic.get_id()),
    }
}

/// The full dotted name of the class `name`, as it is declared.
fn class_text(name: Word, codebase: &CodebaseMetadata) -> String {
    let declared = codebase.get_class_like(name.as_bytes()).map_or(name, |class| class.original_name);

    declared.as_str_lossy().trim_start_matches('\\').replace('\\', ".")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mago_codex::metadata::CodebaseMetadata;
    use mago_codex::misc::GenericParent;
    use mago_codex::ttype::TType;
    use mago_codex::ttype::atomic::TAtomic;
    use mago_codex::ttype::atomic::callable::TCallable;
    use mago_codex::ttype::atomic::callable::TCallableSignature;
    use mago_codex::ttype::atomic::callable::parameter::TCallableParameter;
    use mago_codex::ttype::atomic::generic::TGenericParameter;
    use mago_codex::ttype::atomic::iterable::TIterable;
    use mago_codex::ttype::atomic::mixed::TMixed;
    use mago_codex::ttype::atomic::object::TObject;
    use mago_codex::ttype::atomic::object::named::TNamedObject;
    use mago_codex::ttype::atomic::scalar::TScalar;
    use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
    use mago_codex::ttype::get_bool;
    use mago_codex::ttype::get_float;
    use mago_codex::ttype::get_int;
    use mago_codex::ttype::get_keyed_array;
    use mago_codex::ttype::get_list;
    use mago_codex::ttype::get_mixed;
    use mago_codex::ttype::get_object;
    use mago_codex::ttype::get_string;
    use mago_codex::ttype::get_void;
    use mago_codex::ttype::union::TUnion;
    use mago_codex::ttype::wrap_atomic;
    use mago_word::word;

    use super::type_text;

    fn text(r#type: &TUnion) -> String {
        type_text(r#type, &CodebaseMetadata::default())
    }

    fn class(name: &str, arguments: Vec<TUnion>) -> TUnion {
        let arguments = (!arguments.is_empty()).then_some(arguments);

        wrap_atomic(TAtomic::Object(TObject::Named(TNamedObject::new_with_type_parameters(word(name), arguments))))
    }

    fn union(members: &[TUnion]) -> TUnion {
        TUnion::from_vec(members.iter().flat_map(|member| member.types.iter().cloned()).collect())
    }

    fn function(return_type: TUnion, parameters: &[TUnion]) -> TUnion {
        let parameters = parameters
            .iter()
            .map(|parameter| TCallableParameter::new(Some(Arc::new(parameter.clone())), false, false, false))
            .collect();

        wrap_atomic(TAtomic::Callable(TCallable::Signature(
            TCallableSignature::new(false, true)
                .with_parameters(parameters)
                .with_return_type(Some(Arc::new(return_type))),
        )))
    }

    #[test]
    fn a_class_is_its_full_dotted_name_and_the_built_in_types_are_their_names() {
        assert_eq!(text(&class("App\\Order", vec![])), "App.Order");
        assert_eq!(text(&class("Order", vec![])), "Order");
        assert_eq!(text(&get_int()), "int");
        assert_eq!(text(&get_float()), "float");
        assert_eq!(text(&get_bool()), "bool");
        assert_eq!(text(&get_string()), "string");
        assert_eq!(text(&get_void()), "void");
        assert_eq!(text(&get_object()), "Object");
        assert_eq!(text(&wrap_atomic(TAtomic::Mixed(TMixed::new().with_is_non_null(true)))), "Any");
        assert_eq!(text(&get_mixed()), "Any?");
    }

    #[test]
    fn a_generic_type_writes_its_arguments_nested_with_a_space_after_each_comma() {
        let order = class("App\\Order", vec![]);
        let page = class("App\\PaginatedList", vec![get_keyed_array(get_string(), get_list(order.clone()))]);

        assert_eq!(text(&page), "App.PaginatedList<Map<string, List<App.Order>>>");
        assert_eq!(text(&class("App\\Pair", vec![order.clone(), get_int()])), "App.Pair<App.Order, int>");
        assert_eq!(text(&wrap_atomic(TAtomic::Iterable(TIterable::of_value(Arc::new(order))))), "Iterable<App.Order>");
    }

    #[test]
    fn a_nullable_type_ends_in_a_question_mark_and_a_nullable_union_is_in_parentheses() {
        assert_eq!(text(&get_int().as_nullable()), "int?");
        assert_eq!(text(&class("App\\Order", vec![]).as_nullable()), "App.Order?");
        assert_eq!(text(&union(&[get_string(), get_int()]).as_nullable()), "(int|string)?");
        assert_eq!(text(&get_list(get_int().as_nullable())), "List<int?>");
    }

    #[test]
    fn union_members_are_sorted_by_their_text() {
        let union = union(&[get_string(), class("App\\Zone", vec![]), get_int(), class("App\\Area", vec![])]);

        assert_eq!(text(&union), "App.Area|App.Zone|int|string");
    }

    #[test]
    fn an_intersection_joins_its_members_sorted_and_stands_in_parentheses_in_a_union_or_a_nullable_type() {
        let mut shared = TAtomic::Object(TObject::Named(TNamedObject::new(word("App\\Shareable"))));
        shared.add_intersection_type(TAtomic::Object(TObject::Named(TNamedObject::new(word("App\\DatabaseEntity")))));
        let shared = wrap_atomic(shared);

        assert_eq!(text(&shared), "App.DatabaseEntity & App.Shareable");
        assert_eq!(text(&get_list(shared.clone())), "List<App.DatabaseEntity & App.Shareable>");
        assert_eq!(text(&union(&[get_int(), shared.clone()])), "(App.DatabaseEntity & App.Shareable)|int");
        assert_eq!(
            text(&union(&[class("App\\Zone", vec![]), shared.clone()])),
            "(App.DatabaseEntity & App.Shareable)|App.Zone"
        );
        assert_eq!(text(&shared.clone().as_nullable()), "(App.DatabaseEntity & App.Shareable)?");
        assert_eq!(text(&union(&[get_int(), shared]).as_nullable()), "((App.DatabaseEntity & App.Shareable)|int)?");
    }

    #[test]
    fn a_function_type_writes_its_return_type_then_its_parameter_types() {
        let order = class("App\\Order", vec![]);

        assert_eq!(text(&function(get_bool(), &[order, get_int()])), "Function<bool(App.Order, int)>");
        assert_eq!(text(&function(get_void(), &[])), "Function<void()>");
        assert_eq!(
            text(&function(get_list(get_int()).as_nullable(), &[get_string().as_nullable()])),
            "Function<List<int>?(string?)>"
        );
    }

    #[test]
    fn a_type_parameter_is_its_name_and_a_class_value_names_its_class() {
        let parameter = TGenericParameter::new(
            word("TItem"),
            Arc::new(get_mixed()),
            GenericParent::ClassLike(word("App\\PaginatedList")),
        );
        let class_value = TClassLikeString::literal(word("App\\Order"));

        assert_eq!(text(&wrap_atomic(TAtomic::GenericParameter(parameter))), "TItem");
        assert_eq!(text(&wrap_atomic(TAtomic::Scalar(TScalar::ClassLikeString(class_value)))), "Class<App.Order>");
    }
}
