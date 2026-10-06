use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
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
    Enum,
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
            DeclarationKind::Enum
        } else {
            DeclarationKind::Class
        };

        Declaration { kind }
    }

    /// The declaration `member` of the fully qualified class name `class` resolves to when code reads it: a constant
    /// or enum case first, then a property, then a method, which the read takes as a first-class callable.
    pub(crate) fn member_declaration(&self, class: &[u8], member: &[u8]) -> Declaration {
        let kind = if self.codebase.get_enum_case(class, member).is_some() {
            DeclarationKind::EnumCase
        } else if self.codebase.class_constant_exists(class, member) {
            DeclarationKind::Constant
        } else if let Some(kind) = self.property_kind(class, member) {
            kind
        } else if let Some(kind) = self.method_kind(class, member) {
            kind
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
    /// holding a function, as spec section 14 calls one. The receiver's type names one class.
    pub(crate) fn call_target(&self, call: &Expression) -> Declaration {
        let (object, method) = match call {
            Expression::Call(Call::Method(call)) => (call.object, &call.method),
            Expression::Call(Call::NullSafeMethod(call)) => (call.object, &call.method),
            _ => unreachable!("only a method call has a typed call target yet"),
        };
        let ClassLikeMemberSelector::Identifier(method) = method else {
            unreachable!("check_slice refuses the method name `{method}`");
        };
        let class = single_class(self.expression_type(object))
            .unwrap_or_else(|| unreachable!("the lowering asks only for a receiver whose type names one class"));

        let kind = self.method_kind(class, method.value).or_else(|| self.property_kind(class, method.value));

        Declaration {
            kind: kind.unwrap_or_else(|| {
                unreachable!(
                    "the checker refuses `{}.{}()`, which names no method or property",
                    String::from_utf8_lossy(class),
                    String::from_utf8_lossy(method.value)
                )
            }),
        }
    }

    /// A declared property, or else one a PHP class's `__get` serves, tagged with `@property` or not.
    fn property_kind(&self, class: &[u8], property: &[u8]) -> Option<DeclarationKind> {
        match self.codebase.get_declaring_property(class, &[b"$", property].concat()) {
            Some(property) if property.flags.is_static() => Some(DeclarationKind::StaticProperty),
            Some(_) => Some(DeclarationKind::Property),
            None => self.codebase.method_exists(class, b"__get").then_some(DeclarationKind::Property),
        }
    }

    /// A declared method, or else one a PHP class's `__call` serves, tagged with `@method` or not.
    fn method_kind(&self, class: &[u8], method: &[u8]) -> Option<DeclarationKind> {
        match self.codebase.get_declaring_method(class, method).and_then(|method| method.method_metadata.as_ref()) {
            Some(method) if method.is_static => Some(DeclarationKind::StaticMethod),
            Some(_) => Some(DeclarationKind::Method),
            None => self.codebase.method_exists(class, b"__call").then_some(DeclarationKind::Method),
        }
    }
}

/// The fully qualified name of the one class `r#type` names, leaving out `null`, or none when it names no class or
/// several.
pub(crate) fn single_class(r#type: &TUnion) -> Option<&[u8]> {
    let mut classes = r#type.types.iter().filter(|atomic| !atomic.is_null()).map(|atomic| match atomic {
        TAtomic::Object(TObject::Named(object)) => Some(object.name.as_bytes()),
        TAtomic::Object(TObject::Enum(object)) => Some(object.name.as_bytes()),
        _ => None,
    });
    let class = classes.next()??;

    classes.all(|other| other == Some(class)).then_some(class)
}
