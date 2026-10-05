//! The single-workspace [`Server`]: the transport-agnostic core that owns one
//! workspace's file database and analysis service, and answers queries against them.

use foldhash::HashMap;

use mago_analyzer::analysis_result::AnalysisResult;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::file::FileId;
use mago_database::membership::WorkspaceMatcher;
use mago_orchestrator::service::incremental_analysis::IncrementalAnalysisService;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;

use crate::error::ServerError;
use crate::settings::Settings;

/// A transport-agnostic backend for a single workspace.
///
/// Owns the workspace's [`Database`] and an [`IncrementalAnalysisService`]. It
/// performs no I/O: callers supply file contents as in-memory buffers and the
/// server answers queries in terms of [`FileId`]s.
pub struct Server {
    database: Database<'static>,
    service: IncrementalAnalysisService,
}

impl Server {
    /// Build a server for one workspace from an already-loaded file database,
    /// the function that decodes the prelude, and resolved [`Settings`].
    ///
    /// Construction performs no analysis; call [`Server::analyze`] for the
    /// initial pass. Each analysis from scratch decodes the prelude afresh.
    #[must_use]
    pub fn new(
        database: Database<'static>,
        prelude: fn() -> (CodebaseMetadata, SymbolReferences),
        settings: Settings,
    ) -> Self {
        let Settings { parser, analyzer, plugin_registry, use_progress_bars } = settings;

        let service = IncrementalAnalysisService::new(database.read_only(), prelude, analyzer, parser, plugin_registry)
            .with_progress_bars(use_progress_bars);

        Self { database, service }
    }

    /// Build the codebase and run the before-analysis hooks, then analyze no
    /// file: every analysis reports only the issues those hooks raise.
    #[must_use]
    pub fn scan_only(mut self) -> Self {
        self.service = self.service.scan_only();
        self
    }

    /// Borrow the workspace file database.
    #[must_use]
    pub fn database(&self) -> &Database<'static> {
        &self.database
    }

    /// Mutably borrow the workspace file database (to add / update / delete
    /// files as the workspace changes).
    pub fn database_mut(&mut self) -> &mut Database<'static> {
        &mut self.database
    }

    /// Run a full analysis pass over the workspace.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] if the underlying analysis fails. The failed
    /// pass leaves no state behind: the next pass analyzes the whole workspace.
    pub fn analyze(&mut self) -> Result<AnalysisResult, ServerError> {
        self.service.analyze().inspect_err(|_| self.service.reset()).map_err(ServerError::from)
    }

    /// Refresh the analysis service's view of the database, then re-analyze the
    /// `changed` files, or the whole workspace when no pass has succeeded yet.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] if the underlying analysis fails. The failed
    /// pass leaves no state behind: the next pass analyzes the whole workspace.
    pub fn analyze_incremental(&mut self, changed: &[FileId]) -> Result<AnalysisResult, ServerError> {
        self.service.update_database(self.database.read_only());
        let result = if self.service.is_initialized() {
            self.service.analyze_incremental(Some(changed))
        } else {
            self.service.analyze()
        };

        result.inspect_err(|_| self.service.reset()).map_err(ServerError::from)
    }

    /// The issues of the most recent analysis pass reported in a file `scope` contains, plus the
    /// issues that name no file. An issue is reported in its primary annotation's file, else in its
    /// first annotation's.
    #[must_use]
    pub fn issues_in(&self, scope: &WorkspaceMatcher) -> IssueCollection {
        let mut in_scope = HashMap::<FileId, bool>::default();

        self.service
            .last_issues()
            .unwrap_or_default()
            .into_iter()
            .filter(|issue| {
                let Some(file_id) = reported_file(issue) else {
                    return true;
                };

                *in_scope.entry(file_id).or_insert_with(|| {
                    self.database
                        .get_ref(&file_id)
                        .ok()
                        .and_then(|file| file.path.as_deref())
                        .is_some_and(|path| scope.contains(path))
                })
            })
            .collect()
    }
}

/// The file `issue` is reported in: its primary annotation's file, else its first annotation's, or
/// `None` when it names no file.
fn reported_file(issue: &Issue) -> Option<FileId> {
    issue
        .annotations
        .iter()
        .find(|annotation| annotation.kind.is_primary())
        .or_else(|| issue.annotations.first())
        .map(|annotation| annotation.span.file_id)
        .filter(|file_id| !file_id.is_zero())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::borrow::Cow;
    use std::path::Path;
    use std::sync::Arc;

    use mago_analyzer::plugin::PluginRegistry;
    use mago_analyzer::settings::Settings as AnalyzerSettings;
    use mago_database::DatabaseConfiguration;
    use mago_database::file::File;
    use mago_database::file::FileType;
    use mago_reporting::Annotation;
    use mago_syntax::settings::ParserSettings;

    use super::*;

    fn host(name: &'static str, contents: &'static str) -> File {
        File::new(
            Cow::Borrowed(name.as_bytes()),
            FileType::Host,
            Some(Path::new("/scope").join(name)),
            Cow::Borrowed(contents.as_bytes()),
        )
    }

    /// A server over two host files, `src/a.php` and `src/b.php`, that each report an issue.
    fn two_file_server() -> (Server, FileId, FileId) {
        let mut database =
            Database::new(DatabaseConfiguration::new(Path::new("/scope"), vec![], vec![], vec![], vec![]));
        let a = database.add(host("src/a.php", "<?php\nfunction a(): int { return 'a'; }\n"));
        let b = database.add(host("src/b.php", "<?php\nfunction b(): int { return 'b'; }\n"));
        let settings = Settings {
            parser: ParserSettings::default(),
            analyzer: AnalyzerSettings::default(),
            plugin_registry: Arc::new(PluginRegistry::with_library_providers()),
            use_progress_bars: false,
        };

        (Server::new(database, Default::default, settings), a, b)
    }

    #[test]
    fn the_issues_in_a_scope_are_those_whose_primary_annotation_lies_in_it() {
        let (mut server, _, b) = two_file_server();
        let all = server.analyze().expect("analysis").issues;

        let scope = WorkspaceMatcher::from_configuration(&DatabaseConfiguration::new(
            Path::new("/scope"),
            vec![b"src/b.php"],
            vec![],
            vec![],
            vec![b"php"],
        ))
        .expect("the scope compiles");
        let scoped = server.issues_in(&scope);

        let primary_file = |issue: &Issue| {
            issue
                .annotations
                .iter()
                .find(|annotation| annotation.kind.is_primary())
                .map(|annotation| annotation.span.file_id)
        };
        assert!(all.iter().any(|issue| primary_file(issue).is_some_and(|file| file != b)), "{all:#?}");
        assert!(!scoped.is_empty());
        assert_eq!(
            scoped.iter().collect::<Vec<_>>(),
            all.iter().filter(|issue| primary_file(issue) == Some(b)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_issue_is_reported_in_its_primary_file_else_its_first_annotated_file() {
        let (mut server, a, b) = two_file_server();
        let issues = server.analyze().expect("analysis").issues;
        let span_in = |file_id: FileId| {
            issues
                .iter()
                .flat_map(|issue| &issue.annotations)
                .find(|annotation| annotation.span.file_id == file_id)
                .expect("an issue in the file")
                .span
        };

        let primary =
            Issue::error("x").with_annotations([Annotation::secondary(span_in(a)), Annotation::primary(span_in(b))]);
        let secondary_only =
            Issue::error("x").with_annotations([Annotation::secondary(span_in(a)), Annotation::secondary(span_in(b))]);

        assert_eq!(reported_file(&primary), Some(b));
        assert_eq!(reported_file(&secondary_only), Some(a));
        assert_eq!(reported_file(&Issue::error("x")), None);
    }
}
