use super::*;
use crate::structure::classify::FileMeasurement;
use crate::structure::syntax::{ConcreteStoreCounts, ConcreteStoreSignatureSites, ConcreteStoreSites, VisibilityCounts};

type ImportFixture<'a> = (&'a str, &'a [&'a str]);

#[test]
fn exact_http_transport_exception_passes_but_a_second_import_fails() {
    let policy = policy();
    policy
        .compare_current(&inventory(&[(HTTP_TRANSPORT_PATH, &["crate::server::LocalHoldServer"])]))
        .expect("exact baseline exemption passes");

    let error = policy
        .compare_current(&inventory(&[(
            HTTP_TRANSPORT_PATH,
            &["crate::server::LocalHoldServer", "crate::server::params::RememberResponse"],
        )]))
        .unwrap_err();
    assert!(error.to_string().contains("restricted import mismatch"));
}

#[test]
fn classifier_exemptions_must_arrive_empty_and_policy_never_filters_by_path() {
    let policy = policy();
    let accepted = inventory(&[
        (HTTP_TRANSPORT_PATH, &["crate::server::LocalHoldServer"]),
        ("src/server/mod.rs", &[]),
        ("src/ui/mod.rs", &[]),
        ("src/main.rs", &[]),
    ]);
    policy.compare_current(&accepted).expect("classifier exemptions produce no observed imports");

    let forbidden = inventory(&[
        (HTTP_TRANSPORT_PATH, &["crate::server::LocalHoldServer"]),
        ("src/bin/worker.rs", &["crate::server::LocalHoldServer"]),
    ]);
    assert!(policy.compare_current(&forbidden).unwrap_err().to_string().contains("src/bin/worker.rs"));
}

#[test]
fn baseline_uses_only_baseline_exceptions() {
    let mut policy = policy();
    policy
        .exceptions
        .push(exception("phase4.http-transport-params", "crate::server::params::RememberResponse", false));
    policy
        .compare_baseline(&inventory(&[(HTTP_TRANSPORT_PATH, &["crate::server::LocalHoldServer"])]))
        .expect("later reviewed exceptions do not rewrite recovery baseline");
    policy
        .compare_current(&inventory(&[(
            HTTP_TRANSPORT_PATH,
            &["crate::server::LocalHoldServer", "crate::server::params::RememberResponse"],
        )]))
        .expect("current inventory requires every reviewed exception");
}

#[test]
fn retirement_allows_removal_and_prevents_resurrection() {
    let mut policy = policy();
    policy.retirements.push(retirement("phase4.retire-http-transport", "phase0.http-transport-server"));
    policy.compare_current(&inventory(&[])).expect("retired import is absent");

    let error = policy
        .compare_current(&inventory(&[(HTTP_TRANSPORT_PATH, &["crate::server::LocalHoldServer"])]))
        .unwrap_err();
    assert!(error.to_string().contains("restricted import mismatch"));
    policy
        .compare_baseline(&inventory(&[(HTTP_TRANSPORT_PATH, &["crate::server::LocalHoldServer"])]))
        .expect("retirement does not rewrite recovery evidence");
}

#[test]
fn architecture_and_structure_baselines_must_match() {
    let policy = policy();
    policy.require_baseline_commit("b05f7a43345b39d40b456fb9ed46d479c4bf26e0").expect("matching baseline");
    assert!(
        policy
            .require_baseline_commit("77af0885525a5e3be81a7631d5d41e1809c6587d")
            .unwrap_err()
            .to_string()
            .contains("same baseline commit")
    );
}

#[test]
fn exception_evidence_is_append_only_and_new_entries_are_not_baseline() {
    let previous = policy();
    let mut current = previous.clone();
    current
        .exceptions
        .push(exception("phase4.http-transport-params", "crate::server::params::RememberResponse", false));
    current.compare_policy(&previous).expect("new reviewed non-baseline exception");

    let mut rewritten = current.clone();
    rewritten.exceptions[0].rationale = "rewritten".to_owned();
    assert!(rewritten.compare_policy(&previous).unwrap_err().to_string().contains("append-only"));

    let mut false_baseline = current;
    false_baseline.exceptions[1].baseline = true;
    assert!(false_baseline.compare_policy(&previous).unwrap_err().to_string().contains("cannot be marked as baseline"));

    let mut retired = previous.clone();
    retired.retirements.push(retirement("phase4.retire-http-transport", "phase0.http-transport-server"));
    retired.compare_policy(&previous).expect("later retirement");

    let mut rewritten_retirement = retired.clone();
    rewritten_retirement.retirements[0].rationale = "rewritten".to_owned();
    assert!(rewritten_retirement.compare_policy(&retired).unwrap_err().to_string().contains("append-only"));

    let mut add_and_retire = previous.clone();
    add_and_retire
        .exceptions
        .push(exception("phase4.http-transport-params", "crate::server::params::RememberResponse", false));
    add_and_retire.retirements.push(retirement("phase4.retire-params", "phase4.http-transport-params"));
    assert!(add_and_retire.compare_policy(&previous).unwrap_err().to_string().contains("added and retired"));
}

#[test]
fn exception_scope_target_and_evidence_are_validated() {
    let mut wrong_source = policy();
    wrong_source.exceptions[0].source = "src/engine.rs".to_owned();
    assert!(wrong_source.validate().unwrap_err().to_string().contains("only to src/http_transport.rs"));

    let mut glob = policy();
    glob.exceptions[0].target = "crate::server::*".to_owned();
    assert!(glob.validate().unwrap_err().to_string().contains("normalized explicit"));

    let mut invalid_identifier = policy();
    invalid_identifier.exceptions[0].target = "crate::server::7invalid".to_owned();
    assert!(invalid_identifier.validate().unwrap_err().to_string().contains("normalized explicit"));

    let mut missing_issue = policy();
    missing_issue.exceptions[0].issue.clear();
    assert!(missing_issue.validate().unwrap_err().to_string().contains("issue must not be empty"));

    let mut unknown_retirement = policy();
    unknown_retirement.retirements.push(retirement("phase4.retire-missing", "missing"));
    assert!(unknown_retirement.validate().unwrap_err().to_string().contains("unknown exception"));

    let mut initial_nonbaseline = policy();
    initial_nonbaseline.exceptions[0].baseline = false;
    assert!(
        initial_nonbaseline
            .validate_initial_policy()
            .unwrap_err()
            .to_string()
            .contains("only active recovery-baseline")
    );
}

fn policy() -> ImportPolicy {
    ImportPolicy {
        schema_version: 1,
        baseline_commit: "b05f7a43345b39d40b456fb9ed46d479c4bf26e0".to_owned(),
        exceptions: vec![exception("phase0.http-transport-server", "crate::server::LocalHoldServer", true)],
        retirements: Vec::new(),
    }
}

fn retirement(id: &str, exception_id: &str) -> ImportRetirement {
    ImportRetirement {
        id: id.to_owned(),
        exception_id: exception_id.to_owned(),
        owner: "maintainers".to_owned(),
        issue: "https://github.com/gearboxlogic/localhold/issues/124".to_owned(),
        pull_request: "https://github.com/gearboxlogic/localhold/pull/131".to_owned(),
        rationale: "The transport no longer imports the MCP server service".to_owned(),
    }
}

fn exception(id: &str, target: &str, baseline: bool) -> ImportException {
    ImportException {
        id: id.to_owned(),
        source: HTTP_TRANSPORT_PATH.to_owned(),
        target: target.to_owned(),
        baseline,
        owner: "maintainers".to_owned(),
        issue: "https://github.com/gearboxlogic/localhold/issues/124".to_owned(),
        pull_request: "https://github.com/gearboxlogic/localhold/pull/131".to_owned(),
        rationale: "HTTP router construction currently requires the MCP server service".to_owned(),
        re_review_phase: "Phase 4 protocol/application boundary restoration".to_owned(),
    }
}

fn inventory(files: &[ImportFixture<'_>]) -> Inventory {
    Inventory {
        files: files
            .iter()
            .map(|(path, imports)| FileMeasurement {
                path: (*path).to_owned(),
                physical_lines: 1,
                production_lines: 1,
                test_lines: 0,
                production_targets: vec!["crate".to_owned()],
                production_module: Vec::new(),
                production_internal_imports: imports.iter().map(|target| (*target).to_owned()).collect(),
                production_public_reexports: Vec::new(),
                production_type_declarations: Vec::new(),
                production_concrete_stores: ConcreteStoreCounts::default(),
                production_public_concrete_store_structs: ConcreteStoreSites::default(),
                production_concrete_store_sites: ConcreteStoreSites::default(),
                production_generic_default_store_sites: ConcreteStoreSites::default(),
                production_signature_store_sites: ConcreteStoreSignatureSites::default(),
                production_store_binding_sites: ConcreteStoreSites::default(),
                production_visibilities: VisibilityCounts::default(),
            })
            .collect(),
    }
}
