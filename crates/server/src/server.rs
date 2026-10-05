//! The single-workspace [`Server`]: the transport-agnostic core that owns one
//! workspace's file database and analysis service, and answers queries against them.

use foldhash::HashSet;

use mago_analyzer::analysis_result::AnalysisResult;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::file::FileId;
use mago_database::membership::WorkspaceMatcher;
use mago_orchestrator::error::OrchestratorError;
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
    scope: Option<WorkspaceMatcher>,
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

        Self { database, service, scope: None }
    }

    /// Build a server for one workspace that resumes from the state [`Server::encode`] returned,
    /// so its next pass is incremental.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when `state` does not decode or the extension workers refuse it.
    pub fn restore(
        database: Database<'static>,
        prelude: fn() -> (CodebaseMetadata, SymbolReferences),
        settings: Settings,
        state: &[u8],
    ) -> Result<Self, ServerError> {
        let mut server = Self::new(database, prelude, settings);
        server.service.restore(state)?;

        Ok(server)
    }

    /// Encode the state of the last pass for [`Server::restore`].
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] before the first pass, or when encoding fails.
    pub fn encode(&self) -> Result<Vec<u8>, ServerError> {
        Ok(self.service.encode()?)
    }

    /// Build the codebase and run the before-analysis hooks, then analyze no
    /// file: every analysis reports only the issues those hooks raise.
    #[must_use]
    pub fn scan_only(mut self) -> Self {
        self.service = self.service.scan_only();
        self
    }

    /// Analyze the whole workspace, but report only the issues in files `scope` contains, plus the
    /// issues that name no file. An issue is reported in its primary annotation's file, else in its
    /// first annotation's. The node-analysis hooks report only in the file they inspect, so they run
    /// only in the files `scope` contains.
    #[must_use]
    pub fn scoped_to(mut self, scope: WorkspaceMatcher) -> Self {
        self.scope = Some(scope);
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
        self.pass(IncrementalAnalysisService::analyze)
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
        if self.service.is_initialized() {
            self.pass(|service| service.analyze_incremental(Some(changed)))
        } else {
            self.pass(IncrementalAnalysisService::analyze)
        }
    }

    /// The issues of the last pass reported in files `scope` contains, plus the issues that name no
    /// file, or nothing before the first pass.
    ///
    /// A server that is not [scoped](Server::scoped_to) runs the node-analysis hooks in every
    /// file, so it answers any scope.
    #[must_use]
    pub fn issues_in(&self, scope: &WorkspaceMatcher) -> IssueCollection {
        let files = self.files_in(scope);

        self.service
            .last_issues()
            .unwrap_or_default()
            .into_iter()
            .filter(|issue| reported_file(issue).is_none_or(|file_id| files.contains(&file_id)))
            .collect()
    }

    /// Run one analysis pass with the node-analysis hooks only in the scope's files, and report
    /// only their issues. A failed pass resets the service.
    fn pass(
        &mut self,
        analyze: impl FnOnce(&mut IncrementalAnalysisService) -> Result<AnalysisResult, OrchestratorError>,
    ) -> Result<AnalysisResult, ServerError> {
        let files = self.scope.as_ref().map(|scope| self.files_in(scope));
        self.service.set_node_analysis_files(files.clone());

        analyze(&mut self.service)
            .inspect_err(|_| self.service.reset())
            .map(|result| reported_in(result, files.as_ref()))
            .map_err(ServerError::from)
    }

    /// The workspace files whose path `scope` contains.
    fn files_in(&self, scope: &WorkspaceMatcher) -> HashSet<FileId> {
        self.database
            .files()
            .filter(|file| file.path.as_deref().is_some_and(|path| scope.contains(path)))
            .map(|file| file.id)
            .collect()
    }
}

/// `result` with only the issues reported in `files`, plus the issues that name no file, or every
/// issue when `files` is `None`.
fn reported_in(mut result: AnalysisResult, files: Option<&HashSet<FileId>>) -> AnalysisResult {
    if let Some(files) = files {
        result.issues = std::mem::take(&mut result.issues)
            .into_iter()
            .filter(|issue| reported_file(issue).is_none_or(|file_id| files.contains(&file_id)))
            .collect();
    }

    result
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
    fn a_scoped_server_reports_the_issues_whose_primary_annotation_lies_in_its_scope() {
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
        let scoped = two_file_server().0.scoped_to(scope).analyze().expect("scoped analysis").issues;

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
    fn a_restored_server_answers_an_edit_like_a_fresh_analysis() {
        let (mut warm, a, _) = two_file_server();
        warm.analyze().expect("analysis");
        let state = warm.encode().expect("the state encodes");

        let (fresh, _, _) = two_file_server();
        let settings = Settings {
            parser: ParserSettings::default(),
            analyzer: AnalyzerSettings::default(),
            plugin_registry: Arc::new(PluginRegistry::with_library_providers()),
            use_progress_bars: false,
        };
        let mut restored =
            Server::restore(fresh.database().clone(), Default::default, settings, &state).expect("the state restores");
        let edited = Cow::Borrowed("<?php\nfunction a(): string { return 'a'; }\n".as_bytes());
        restored.database_mut().update(a, edited.clone());
        let restored_issues = restored.analyze_incremental(&[a]).expect("incremental analysis").issues;

        let (mut cold, _, _) = two_file_server();
        cold.database_mut().update(a, edited);
        let cold_issues = cold.analyze_incremental(&[]).expect("analysis from scratch").issues;

        assert!(!cold_issues.is_empty());
        assert_eq!(restored_issues, cold_issues);
    }

    #[test]
    fn issues_in_answers_any_scope_from_one_unscoped_pass() {
        let (mut server, a, b) = two_file_server();
        let all = server.analyze().expect("analysis").issues;
        let scope = |path: &'static [u8]| {
            WorkspaceMatcher::from_configuration(&DatabaseConfiguration::new(
                Path::new("/scope"),
                vec![path],
                vec![],
                vec![],
                vec![b"php"],
            ))
            .expect("the scope compiles")
        };

        for (file, path) in [(a, b"src/a.php".as_slice()), (b, b"src/b.php".as_slice())] {
            let expected = all.iter().filter(|issue| reported_file(issue).is_none_or(|id| id == file));
            assert_eq!(server.issues_in(&scope(path)).iter().collect::<Vec<_>>(), expected.collect::<Vec<_>>());
        }
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
