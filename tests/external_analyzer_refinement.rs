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

#[Refined]
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
        if refined { "#[Refined]" } else { "" }
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

fn factory(returned: &str) -> String {
    format!(
        r"<?php

namespace Proof;

#[Refined]
final class Factory
{{
    public function __construct(public Box $special, public Box $plain) {{}}

    public function make(): Box
    {{
        return $this->{returned};
    }}
}}
"
    )
}

const CONSUMER: &str = r"<?php

namespace Proof;

final class Consumer
{
    public function unwrap(Factory $factory): Other
    {
        return $factory->make()->get();
    }
}
";

fn factory_database(returned: &str) -> (Database<'static>, FileId) {
    let mut database = Database::new(common::database_configuration("/refinement-proof", vec![Cow::Borrowed(b".")]));
    database.add(File::new(Cow::Borrowed(b"src/Box.php"), FileType::Host, None, Cow::Borrowed(BOX.as_bytes())));
    let factory =
        File::new(Cow::Borrowed(b"src/Factory.php"), FileType::Host, None, Cow::Owned(factory(returned).into_bytes()));
    let factory_id = factory.id;
    database.add(factory);
    database.add(File::new(
        Cow::Borrowed(b"src/Consumer.php"),
        FileType::Host,
        None,
        Cow::Borrowed(CONSUMER.as_bytes()),
    ));

    (database, factory_id)
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
        Default::default,
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
fn a_refined_file_entering_the_node_analysis_files_keeps_its_refinement_issues() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the refinement coverage test") {
        return;
    }

    let (mut refined, holder_id) = database(true);
    let expected =
        holder(true).replace("#[Refined]", "// @mago-expect analysis:declaration-refinement-proof/marker\n#[Refined]");
    refined.update(holder_id, Cow::Owned(expected.into_bytes()));
    let mut incremental = service(&refined, registry(repository));
    incremental.set_node_analysis_files(Some([FileId::new(b"src/Box.php")].into_iter().collect()));
    incremental.analyze().expect("scoped analysis should succeed");

    incremental.set_node_analysis_files(None);
    let covered = codes(&incremental.analyze_incremental(Some(&[])).expect("covering analysis").issues);
    let fresh = codes(&service(&refined, registry(repository)).analyze().expect("fresh analysis").issues);
    assert!(!fresh.contains_key("unfulfilled-expect"), "the pragma fulfils the refinement issue: {fresh:#?}");
    assert_eq!(covered, fresh, "a covered file answers like a fresh analysis");
}

#[test]
fn a_refinement_issue_survives_when_an_edit_elsewhere_analyzes_its_file_again() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the refinement cascade test") {
        return;
    }

    let (mut refined, _) = database(true);
    let mut incremental = service(&refined, registry(repository));
    incremental.analyze().expect("refined analysis should succeed");

    let box_id = FileId::new(b"src/Box.php");
    let nullable = BOX.replace("public function get(): object", "public function get(): ?object");
    refined.update(box_id, Cow::Owned(nullable.into_bytes()));
    incremental.update_database(refined.read_only());
    let edited = codes(&incremental.analyze_incremental(Some(&[box_id])).expect("incremental analysis").issues);
    let fresh = codes(&service(&refined, registry(repository)).analyze().expect("fresh analysis").issues);
    assert!(edited.contains_key("declaration-refinement-proof/marker"), "{edited:#?}");
    assert_eq!(edited, fresh, "a signature edit elsewhere answers like a fresh analysis");
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

#[test]
fn a_return_taken_from_the_body_reaches_callers_and_follows_body_edits() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the body return test") {
        return;
    }

    let (special, factory_id) = factory_database("special");
    let mut incremental = service(&special, registry(repository));
    let first = codes(&incremental.analyze().expect("body return analysis should succeed").issues);
    assert!(
        first
            .get("invalid-return-statement")
            .is_some_and(|messages| messages.iter().any(|m| m.contains("Proof\\Special"))),
        "the caller reads the return the body applied: {first:#?}"
    );

    let (plain, _) = factory_database("plain");
    incremental.update_database(plain.read_only());
    let edited = codes(&incremental.analyze_incremental(Some(&[factory_id])).expect("incremental analysis").issues);
    let fresh = codes(&service(&plain, registry(repository)).analyze().expect("fresh analysis").issues);
    assert!(
        !fresh.values().flatten().any(|message| message.contains("Proof\\Special")),
        "the edited body no longer applies the narrower class: {fresh:#?}"
    );
    assert_eq!(edited, fresh, "incremental analysis after a body edit matches a fresh analysis");
}
