use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_sole_backed_enum;
use mago_codex::ttype::union::TUnion;
use mago_names::ResolvedNames;
use mago_span::HasSpan;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
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

    /// The fully qualified name of the one backed enum every value of `r#type` but `null` is a case of, as
    /// [`get_sole_backed_enum`] finds it.
    pub(crate) fn backed_enum(&self, r#type: &TUnion) -> Option<&'analysis [u8]> {
        get_sole_backed_enum(r#type, self.codebase).map(|backed_enum| backed_enum.original_name.as_bytes())
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
