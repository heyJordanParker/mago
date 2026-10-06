use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::metadata::CodebaseMetadata;

/// The checker's types for one file. The lowering reads a type only through these queries, so `--assert-types` can
/// turn each answer it used into a runtime guard.
#[expect(dead_code, reason = "the first typed site in piece 3 reads it")]
pub struct Types<'analysis> {
    artifacts: &'analysis AnalysisArtifacts,
    codebase: &'analysis CodebaseMetadata,
}

impl<'analysis> Types<'analysis> {
    pub(crate) fn new(artifacts: &'analysis AnalysisArtifacts, codebase: &'analysis CodebaseMetadata) -> Self {
        Self { artifacts, codebase }
    }
}
