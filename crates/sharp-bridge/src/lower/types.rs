use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::metadata::CodebaseMetadata;

/// The checker's types for one file. The lowering reads a type only through these queries, so `--assert-types` can
/// turn each answer it used into a runtime guard.
pub struct Types<'analysis> {
    #[expect(dead_code, reason = "the first expression type query in piece 3 reads it")]
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
    StaticMethod,
}

impl<'analysis> Types<'analysis> {
    pub(crate) fn new(artifacts: &'analysis AnalysisArtifacts, codebase: &'analysis CodebaseMetadata) -> Self {
        Self { artifacts, codebase }
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

    /// The declaration `member` of the fully qualified class name `class` resolves to, found as the engine finds
    /// `Class::member`: a constant or enum case first, then a static property, then a static method.
    pub(crate) fn member_declaration(&self, class: &[u8], member: &[u8]) -> Declaration {
        let kind = if self.codebase.get_enum_case(class, member).is_some() {
            DeclarationKind::EnumCase
        } else if self.codebase.class_constant_exists(class, member) {
            DeclarationKind::Constant
        } else if self
            .codebase
            .get_declaring_property(class, &[b"$", member].concat())
            .is_some_and(|property| property.flags.is_static())
        {
            DeclarationKind::StaticProperty
        } else if self
            .codebase
            .get_declaring_method(class, member)
            .and_then(|method| method.method_metadata.as_ref())
            .is_some_and(|method| method.is_static)
        {
            DeclarationKind::StaticMethod
        } else {
            unreachable!(
                "the checker refuses `{}.{}`, which names no constant, case, static property or static method",
                String::from_utf8_lossy(class),
                String::from_utf8_lossy(member)
            )
        };

        Declaration { kind }
    }
}
