use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::metadata::CodebaseMetadata;

/// The checker's types for one file. The lowering reads a type only through these queries, so `--assert-types` can
/// turn each answer it used into a runtime guard.
pub struct Types<'analysis> {
    #[expect(dead_code, reason = "the first expression type query in piece 3 reads it")]
    artifacts: &'analysis AnalysisArtifacts,
    codebase: &'analysis CodebaseMetadata,
}

/// What a declaration the lowering names is, as the checker found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeclarationKind {
    Class,
    Interface,
    Enum,
}

impl<'analysis> Types<'analysis> {
    pub(crate) fn new(artifacts: &'analysis AnalysisArtifacts, codebase: &'analysis CodebaseMetadata) -> Self {
        Self { artifacts, codebase }
    }

    /// The kind of the class-like named `class`, a fully qualified name.
    pub(crate) fn declaration_kind(&self, class: &[u8]) -> DeclarationKind {
        let metadata = self.codebase.get_class_like(class).unwrap_or_else(|| {
            unreachable!("the checker refuses the unknown class `{}`", String::from_utf8_lossy(class))
        });

        if metadata.kind.is_interface() {
            DeclarationKind::Interface
        } else if metadata.kind.is_enum() {
            DeclarationKind::Enum
        } else {
            DeclarationKind::Class
        }
    }
}
