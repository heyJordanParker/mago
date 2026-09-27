#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_database::Database;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;
use mago_orchestrator::service::incremental_analysis::IncrementalAnalysisService;
use mago_php_version::PHPVersion;
use mago_reporting::IssueCollection;
use mago_syntax::settings::ParserSettings;

mod common;

const BOX: &str = r"<?php

namespace Proof;

class Spec {}

final class Special extends Spec {}

final class Other {}

// refined
final class Box
{
    public function __construct(public object $item) {}

    public function get(): object
    {
        return $this->item;
    }
}
";

fn holder(refined: bool) -> String {
    format!(
        r"<?php

namespace Proof;

{}
class Holder
{{
    public ?Box $excess = null;

    public ?Box $outside = null;

    public function wrong(Box $box): Other
    {{
        return $box->get();
    }}

    public function broad(Box $box): Box
    {{
        return $box;
    }}

    public function subtype(): object
    {{
        return new Special();
    }}
}}

final class Heir extends Holder
{{
    public function broad(Box $box): Box
    {{
        return $box;
    }}
}}
",
        if refined { "// refined" } else { "// plain" }
    )
}

fn database(refined: bool) -> (Database<'static>, FileId) {
    let mut database = Database::new(common::database_configuration("/refinement-proof", vec![Cow::Borrowed(b".")]));
    database.add(File::new(Cow::Borrowed(b"src/Box.php"), FileType::Host, None, Cow::Borrowed(BOX.as_bytes())));
    let holder =
        File::new(Cow::Borrowed(b"src/Holder.php"), FileType::Host, None, Cow::Owned(holder(refined).into_bytes()));
    let holder_id = holder.id;
    database.add(holder);

    (database, holder_id)
}

fn registry(repository: &Path) -> Arc<PluginRegistry> {
    let command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-refinement-worker.php"))
        .with_current_directory(repository);
    let pool = WorkerPool::spawn(command, NonZeroUsize::new(2).expect("two workers"), WorkerPoolOptions::default())
        .expect("refinement worker pool should start");
    let analyzer = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)
        .expect("refinement analyzer should initialize");
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));
    Arc::new(registry)
}

fn service(database: &Database<'_>, registry: Arc<PluginRegistry>) -> IncrementalAnalysisService {
    let mut settings = Settings::new(PHPVersion::PHP85);
    settings.find_unused_definitions = false;
    settings.find_unused_expressions = false;
    IncrementalAnalysisService::new(
        database.read_only(),
        CodebaseMetadata::new(),
        SymbolReferences::new(),
        settings,
        ParserSettings::default(),
        registry,
    )
}

fn codes(issues: &IssueCollection) -> BTreeMap<String, Vec<String>> {
    let mut codes = BTreeMap::<String, Vec<String>>::new();
    for issue in issues.iter() {
        codes.entry(issue.code.clone().unwrap_or_default()).or_default().push(issue.message.clone());
    }
    codes
}

#[test]
fn declaration_refinements_reach_declarations_bodies_and_incremental_removal() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the declaration refinement test") {
        return;
    }

    let (refined, holder_id) = database(true);
    let mut incremental = service(&refined, registry(repository));
    let first = codes(&incremental.analyze().expect("refined analysis should succeed").issues);

    assert!(first.contains_key("excess-template-parameter"), "arity is checked on a refined property: {first:#?}");
    assert!(
        first.contains_key("template-constraint-violation"),
        "bounds are checked on a refined property: {first:#?}"
    );
    assert!(
        first.contains_key("declaration-refinement-proof/marker"),
        "an adapter issue is reported under its plugin: {first:#?}"
    );
    assert!(
        first
            .get("invalid-return-statement")
            .is_some_and(|messages| messages.iter().any(|m| m.contains("Proof\\Spec"))),
        "the method body reads the refined parameter through the refined class template: {first:#?}"
    );
    let broader_returns = first.get("less-specific-return-statement").cloned().unwrap_or_default();
    assert!(
        broader_returns.iter().any(|message| message.contains("Holder::broad")),
        "a broader generic value returned from a refined narrower declaration is refused: {first:#?}"
    );
    assert!(
        !first.values().flatten().any(|message| message.contains("Holder::subtype")),
        "a subtype of a refined return is still accepted: {first:#?}"
    );
    assert!(
        broader_returns.iter().any(|message| message.contains("Heir::broad")),
        "an override inheriting a refined return is held to that contract: {first:#?}"
    );

    let (plain, _) = database(false);
    incremental.update_database(plain.read_only());
    let removed = codes(&incremental.analyze_incremental(Some(&[holder_id])).expect("incremental analysis").issues);
    for code in ["excess-template-parameter", "template-constraint-violation", "declaration-refinement-proof/marker"] {
        assert!(!removed.contains_key(code), "removing the refinement removes `{code}`: {removed:#?}");
    }

    let fresh = codes(&service(&plain, registry(repository)).analyze().expect("fresh analysis").issues);
    assert_eq!(removed, fresh, "incremental analysis after removal matches a fresh analysis");
}
