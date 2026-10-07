use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;

/// The checker's types for one file. The lowering reads a type only through these queries, so `--assert-types` can
/// turn each answer it used into a runtime guard.
pub struct Types<'analysis> {
    artifacts: &'analysis AnalysisArtifacts,
    codebase: &'analysis CodebaseMetadata,
}

/// The declaration a class or member name resolves to, as the checker found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Declaration {
    pub(crate) kind: DeclarationKind,
}

/// What a declaration is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeclarationKind {
    Class,
    Interface,
    /// `backed` is whether each case has a backing value, which a `Map` keyed by the enum holds for the case.
    Enum {
        backed: bool,
    },
    Constant,
    EnumCase,
    StaticProperty,
    Property,
    StaticMethod,
    Method,
}

impl<'analysis> Types<'analysis> {
    pub(crate) fn new(artifacts: &'analysis AnalysisArtifacts, codebase: &'analysis CodebaseMetadata) -> Self {
        Self { artifacts, codebase }
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
            DeclarationKind::Enum { backed: metadata.enum_type.is_some() }
        } else {
            DeclarationKind::Class
        };

        Declaration { kind }
    }

    /// The declaration `member` of the fully qualified class name `class` resolves to when code reads it: an enum case,
    /// a constant, a property, then a method, which the read takes as a first-class callable. Only when the class
    /// declares none of them is it a property its `__get` serves.
    pub(crate) fn member_declaration(&self, class: &[u8], member: &[u8]) -> Declaration {
        let kind = if self.codebase.get_enum_case(class, member).is_some() {
            DeclarationKind::EnumCase
        } else if self.codebase.class_constant_exists(class, member) {
            DeclarationKind::Constant
        } else if let Some(kind) = self.property_kind(class, member) {
            kind
        } else if let Some(kind) = self.method_kind(class, member) {
            kind
        } else if self.codebase.method_exists(class, b"__get") {
            DeclarationKind::Property
        } else {
            unreachable!(
                "the checker refuses `{}.{}`, which names no member",
                String::from_utf8_lossy(class),
                String::from_utf8_lossy(member)
            )
        };

        Declaration { kind }
    }

    /// The declaration the method call `call`, null-safe or not, runs: the receiver's method, or else its property
    /// holding a function, as spec section 14 calls one. Only when the class declares neither is it a method its
    /// `__call` serves. The receiver's type names one class.
    pub(crate) fn call_target(&self, call: &Expression) -> Declaration {
        let (object, method) = match call {
            Expression::Call(Call::Method(call)) => (call.object, &call.method),
            Expression::Call(Call::NullSafeMethod(call)) => (call.object, &call.method),
            _ => unreachable!("only a method call has a typed call target yet"),
        };
        let ClassLikeMemberSelector::Identifier(method) = method else {
            unreachable!("check_slice refuses the method name `{method}`");
        };
        let classes = receiver_classes(self.expression_type(object))
            .unwrap_or_else(|| unreachable!("the lowering asks only for a receiver whose type names classes"));

        Declaration { kind: agreed_kind(classes.into_iter().map(|class| self.call_kind(class, method.value))) }
    }

    /// The kind of what `class`'s call of `method` runs: its method, its property holding a function, or else a method
    /// its `__call` serves.
    fn call_kind(&self, class: &[u8], method: &[u8]) -> DeclarationKind {
        self.method_kind(class, method)
            .or_else(|| self.property_kind(class, method))
            .or_else(|| self.codebase.method_exists(class, b"__call").then_some(DeclarationKind::Method))
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

    /// The kind of the property `class` declares by that name, if any.
    fn property_kind(&self, class: &[u8], property: &[u8]) -> Option<DeclarationKind> {
        let property = self.codebase.get_declaring_property(class, &[b"$", property].concat())?;

        Some(if property.flags.is_static() { DeclarationKind::StaticProperty } else { DeclarationKind::Property })
    }

    /// The kind of the method `class` declares by that name, if any.
    fn method_kind(&self, class: &[u8], method: &[u8]) -> Option<DeclarationKind> {
        let method = self.codebase.get_declaring_method(class, method)?.method_metadata.as_ref()?;

        Some(if method.is_static { DeclarationKind::StaticMethod } else { DeclarationKind::Method })
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

/// The kind of member every class a receiver can be declares, which the checker requires to be one kind.
pub(crate) fn agreed_kind(mut kinds: impl Iterator<Item = DeclarationKind>) -> DeclarationKind {
    let kind = kinds.next().unwrap_or_else(|| unreachable!("a receiver's type names at least one class"));
    if kinds.any(|other| other != kind) {
        unreachable!("the checker refuses a member whose kind differs across the receiver's classes");
    }

    kind
}
