//! The single-workspace [`Server`]: the transport-agnostic core that owns one
//! workspace's file database, analysis service, and per-file derived-data
//! caches, and answers queries against them.

use std::sync::Arc;

use foldhash::HashMap;
use xxhash_rust::xxh3;

use mago_analyzer::analysis_result::AnalysisResult;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_database::membership::WorkspaceMatcher;
use mago_orchestrator::service::incremental_analysis::IncrementalAnalysisService;
use mago_reporting::IssueCollection;

use crate::error::ServerError;
use crate::file_analysis;
use crate::file_analysis::FileAnalysis;
use crate::linter::LinterContext;
use crate::settings::Settings;

/// A transport-agnostic backend for a single workspace.
///
/// Owns the workspace's [`Database`], an [`IncrementalAnalysisService`], a
/// [`LinterContext`], and the per-file analysis cache. It performs no I/O:
/// callers supply file contents as in-memory buffers and the server answers
/// queries in terms of [`FileId`]s.
pub struct Server {
    database: Database<'static>,
    service: IncrementalAnalysisService,
    linter: LinterContext,
    /// Per-file parse + resolve + lint results, keyed by content hash.
    file_analyses: HashMap<FileId, (u64, Arc<FileAnalysis>)>,
}

impl Server {
    /// Build a server for one workspace from an already-loaded file database,
    /// decoded codebase metadata, and resolved [`Settings`].
    ///
    /// Construction performs no analysis; call [`Server::analyze`] for the
    /// initial pass.
    #[must_use]
    pub fn new(
        database: Database<'static>,
        metadata: CodebaseMetadata,
        symbol_references: SymbolReferences,
        settings: Settings,
    ) -> Self {
        let Settings { parser, analyzer, linter, plugin_registry, use_progress_bars } = settings;

        let service = IncrementalAnalysisService::new(
            database.read_only(),
            metadata,
            symbol_references,
            analyzer,
            parser,
            plugin_registry,
        )
        .with_progress_bars(use_progress_bars);

        let linter = LinterContext::new(linter, parser);

        Self { database, service, linter, file_analyses: HashMap::default() }
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

    /// Borrow the populated codebase metadata (symbol table).
    #[must_use]
    pub fn codebase(&self) -> &CodebaseMetadata {
        self.service.codebase()
    }

    /// The issues from the most recent analysis pass, if any.
    #[must_use]
    pub fn last_issues(&self) -> Option<IssueCollection> {
        self.service.last_issues()
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

    /// The issues of the most recent analysis pass whose primary annotation lies in a file
    /// `scope` contains, plus the issues that name no file.
    #[must_use]
    pub fn issues_in(&self, scope: &WorkspaceMatcher) -> IssueCollection {
        let mut in_scope = HashMap::<FileId, bool>::default();

        self.service
            .last_issues()
            .unwrap_or_default()
            .into_iter()
            .filter(|issue| {
                let Some(file_id) = issue
                    .annotations
                    .iter()
                    .find(|annotation| annotation.kind.is_primary())
                    .map(|annotation| annotation.span.file_id)
                    .filter(|file_id| !file_id.is_zero())
                else {
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

    /// Return the [`FileAnalysis`] for `file_id`, building it on a cache miss.
    /// One parse + resolve per content hash.
    pub fn file_analysis_for(&mut self, file_id: FileId) -> Option<Arc<FileAnalysis>> {
        let file = self.database.get(&file_id).ok()?;
        let hash = xxh3::xxh3_64(&file.contents);

        if let Some((cached_hash, analysis)) = self.file_analyses.get(&file_id)
            && *cached_hash == hash
        {
            return Some(Arc::clone(analysis));
        }

        let analysis = Arc::new(file_analysis::build(&file, &self.linter));
        self.file_analyses.insert(file_id, (hash, Arc::clone(&analysis)));
        Some(analysis)
    }

    /// Build (or rebuild) the analysis for every changed host file.
    pub fn refresh_analyses(&mut self, file_ids: &[FileId]) {
        for &file_id in file_ids {
            let Ok(file) = self.database.get(&file_id) else {
                self.file_analyses.remove(&file_id);
                continue;
            };

            if file.file_type != FileType::Host {
                continue;
            }

            let hash = xxh3::xxh3_64(&file.contents);
            let analysis = Arc::new(file_analysis::build(&file, &self.linter));
            self.file_analyses.insert(file_id, (hash, analysis));
        }
    }

    /// Build analyses for every host file in the database.
    pub fn refresh_all_host_analyses(&mut self) {
        let host_ids: Vec<FileId> =
            self.database.files().filter(|f| matches!(f.file_type, FileType::Host)).map(|f| f.id).collect();

        self.refresh_analyses(&host_ids);
    }

    /// Iterate the cached lint issues across every analyzed file.
    pub fn lint_issues(&self) -> impl Iterator<Item = &IssueCollection> + '_ {
        self.file_analyses.values().map(|(_, analysis)| &analysis.lint_issues)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::borrow::Cow;
    use std::path::Path;

    use mago_analyzer::plugin::PluginRegistry;
    use mago_analyzer::settings::Settings as AnalyzerSettings;
    use mago_database::DatabaseConfiguration;
    use mago_database::file::File;
    use mago_linter::settings::Settings as LinterSettings;
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

    #[test]
    fn the_issues_in_a_scope_are_those_whose_primary_annotation_lies_in_it() {
        let mut database =
            Database::new(DatabaseConfiguration::new(Path::new("/scope"), vec![], vec![], vec![], vec![]));
        database.add(host("src/a.php", "<?php\nfunction a(): int { return 'a'; }\n"));
        database.add(host("src/b.php", "<?php\nfunction b(): int { return 'b'; }\n"));
        let settings = Settings {
            parser: ParserSettings::default(),
            analyzer: AnalyzerSettings::default(),
            linter: LinterSettings::default(),
            plugin_registry: Arc::new(PluginRegistry::with_library_providers()),
            use_progress_bars: false,
        };
        let mut server = Server::new(database, CodebaseMetadata::new(), SymbolReferences::new(), settings);
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

        let b = FileId::new(b"src/b.php");
        let primary_file = |issue: &mago_reporting::Issue| {
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

    fn server(contents: &'static str) -> (Server, FileId) {
        let mut database =
            Database::new(DatabaseConfiguration::new(Path::new("/lint"), vec![], vec![], vec![], vec![]));
        let id = database.add(File::new(
            Cow::Borrowed(b"src/a.php"),
            FileType::Host,
            None,
            Cow::Borrowed(contents.as_bytes()),
        ));
        let settings = Settings {
            parser: ParserSettings::default(),
            analyzer: AnalyzerSettings::default(),
            linter: LinterSettings::default(),
            plugin_registry: Arc::new(PluginRegistry::with_library_providers()),
            use_progress_bars: false,
        };

        (Server::new(database.into_static(), CodebaseMetadata::new(), SymbolReferences::new(), settings), id)
    }

    #[test]
    fn a_file_analysis_follows_the_file_contents() {
        let (mut server, id) = server("<?php\n");
        let first = server.file_analysis_for(id).expect("the file is in the database");
        assert_eq!(first.lint_issues.len(), 1);
        assert!(Arc::ptr_eq(&first, &server.file_analysis_for(id).expect("cached")));

        server.database_mut().update(id, Cow::Borrowed(b"<?php\n\ndeclare(strict_types=1);\n"));
        server.refresh_analyses(&[id]);
        let messages =
            server.lint_issues().flat_map(IssueCollection::iter).map(|issue| &issue.message).collect::<Vec<_>>();
        assert_eq!(messages, ["Redundant file with no executable code or declarations."]);
    }
}
