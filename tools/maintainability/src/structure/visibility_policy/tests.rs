use super::*;
use crate::structure::classify::FileMeasurement;
use crate::structure::syntax::{ConcreteStoreCounts, ConcreteStoreSignatureSites, ConcreteStoreSites};

#[test]
fn exact_component_counts_pass_and_growth_fails() {
    let policy = policy(&[component("config", budget(2, 1), budget(2, 1)), component("embedding", budget(1, 0), budget(1, 0))]);
    let paths = components(&[("src/config.rs", "config"), ("src/embedding.rs", "embedding")]);
    let exact = inventory(&[file("src/config.rs", 2, 1), file("src/embedding.rs", 1, 0)]);
    policy.compare_current(&exact, &paths).expect("exact current counts");
    policy.compare_baseline(&exact, &paths).expect("exact baseline counts");

    let growth = inventory(&[file("src/config.rs", 3, 1), file("src/embedding.rs", 1, 0)]);
    assert!(policy.compare_current(&growth, &paths).unwrap_err().to_string().contains("production-count mismatch"));

    let unrecorded_reduction = inventory(&[file("src/config.rs", 1, 1), file("src/embedding.rs", 1, 0)]);
    assert!(
        policy
            .compare_current(&unrecorded_reduction, &paths)
            .unwrap_err()
            .to_string()
            .contains("production-count mismatch")
    );
}

#[test]
fn zero_count_components_are_required_and_cannot_gain_visibility() {
    let policy = policy(&[component("config", budget(0, 0), budget(0, 0))]);
    let paths = components(&[("src/config.rs", "config")]);
    policy.compare_current(&inventory(&[file("src/config.rs", 0, 0)]), &paths).expect("closed zero count");
    assert!(
        policy
            .compare_current(&inventory(&[file("src/config.rs", 0, 1)]), &paths)
            .unwrap_err()
            .to_string()
            .contains("production-count mismatch")
    );

    let extra = components(&[("src/config.rs", "config"), ("src/engine.rs", "engine-application")]);
    assert!(
        policy
            .compare_current(&inventory(&[file("src/config.rs", 0, 0), file("src/engine.rs", 0, 0)]), &extra)
            .unwrap_err()
            .to_string()
            .contains("component set mismatch")
    );
}

#[test]
fn evolution_requires_exact_new_exception_delta() {
    let previous = policy(&[component("config", budget(1, 0), budget(1, 0))]);
    let mut grown = previous.clone();
    grown.components[0].current.pub_crate = 2;
    grown.exceptions.push(cross_component_exception(
        "phase0.config-cross-component",
        "config",
        "https://github.com/gearboxlogic/localhold/issues/500",
        1,
    ));
    grown.validate().expect("reviewed growth policy");
    grown.compare_policy(&previous).expect("matching new exception");

    let mut wrong_delta = grown.clone();
    wrong_delta.exceptions[0].delta = 2;
    assert!(wrong_delta.compare_policy(&previous).unwrap_err().to_string().contains("exactly match"));

    let mut unused = previous.clone();
    unused.exceptions.push(cross_component_exception(
        "phase0.config-unused",
        "config",
        "https://github.com/gearboxlogic/localhold/issues/501",
        1,
    ));
    assert!(unused.compare_policy(&previous).unwrap_err().to_string().contains("exactly match"));
}

#[test]
fn old_exceptions_cannot_authorize_visibility_resurrection() {
    let mut approved = policy(&[component("config", budget(1, 0), budget(2, 0))]);
    approved.exceptions.push(cross_component_exception(
        "phase0.config-cross-component",
        "config",
        "https://github.com/gearboxlogic/localhold/issues/500",
        1,
    ));
    approved.validate().expect("approved exception");

    let mut reduced = approved.clone();
    reduced.components[0].current.pub_crate = 1;
    reduced.compare_policy(&approved).expect("visibility reduction");

    let mut resurrected = reduced.clone();
    resurrected.components[0].current.pub_crate = 2;
    resurrected.validate().expect("historical ceiling still parses");
    assert!(resurrected.compare_policy(&reduced).unwrap_err().to_string().contains("exactly match"));
}

#[test]
fn old_subtree_exceptions_cannot_fund_resurrection_with_a_new_exception() {
    let mut previous = policy(&[component("config", budget(0, 0), budget(0, 0))]);
    previous.exceptions.push(subtree_exception("phase0.config-a", "config", "src/config/a", 1));
    previous.validate().expect("historical subtree exception");

    let mut current = previous.clone();
    current.components[0].current.pub_super = 1;
    current.exceptions.push(subtree_exception("phase0.config-b", "config", "src/config/b", 1));
    current.validate().expect("new subtree exception");
    current.compare_policy(&previous).expect("component total matches the new exception");

    let paths = components(&[("src/config/a/mod.rs", "config"), ("src/config/b/mod.rs", "config")]);
    let previous_inventory = inventory(&[file("src/config/a/mod.rs", 0, 0), file("src/config/b/mod.rs", 0, 0)]);
    let resurrected = inventory(&[file("src/config/a/mod.rs", 0, 1), file("src/config/b/mod.rs", 0, 0)]);
    assert!(
        current
            .compare_scope_evolution((&resurrected, &paths), (&previous_inventory, &paths), &previous)
            .unwrap_err()
            .to_string()
            .contains("newly appended subtree exception")
    );

    let reviewed = inventory(&[file("src/config/a/mod.rs", 0, 0), file("src/config/b/mod.rs", 0, 1)]);
    current
        .compare_scope_evolution((&reviewed, &paths), (&previous_inventory, &paths), &previous)
        .expect("growth stays in the newly reviewed subtree");
}

#[test]
fn exception_evidence_is_append_only_and_component_baselines_are_immutable() {
    let mut previous = policy(&[component("config", budget(0, 0), budget(1, 0))]);
    previous.exceptions.push(cross_component_exception(
        "phase0.config-cross-component",
        "config",
        "https://github.com/gearboxlogic/localhold/issues/500",
        1,
    ));

    let mut rewritten = previous.clone();
    rewritten.exceptions[0].rationale = "Different rationale".to_owned();
    assert!(rewritten.compare_policy(&previous).unwrap_err().to_string().contains("append-only"));

    let mut rebased = previous.clone();
    rebased.components[0].baseline.pub_crate = 1;
    assert!(rebased.compare_policy(&previous).unwrap_err().to_string().contains("baselines are immutable"));
}

#[test]
fn pub_crate_growth_requires_distinct_cross_component_review() {
    let mut umbrella = policy(&[component("config", budget(0, 0), budget(1, 0))]);
    umbrella
        .exceptions
        .push(cross_component_exception("phase0.config-cross-component", "config", PHASE_ZERO_ISSUE, 1));
    assert!(umbrella.validate().unwrap_err().to_string().contains("distinct architectural issue"));

    let mut narrowed = umbrella.clone();
    narrowed.exceptions[0].issue = "https://github.com/gearboxlogic/localhold/issues/500".to_owned();
    narrowed.exceptions[0].scope = VisibilityScope::ComponentSubtree;
    narrowed.exceptions[0].subtree = Some("src/config".to_owned());
    assert!(narrowed.validate().unwrap_err().to_string().contains("cross-component scope"));
}

#[test]
fn pub_super_growth_is_confined_to_the_reviewed_component_subtree() {
    let mut policy = policy(&[component("config", budget(0, 0), budget(0, 1))]);
    policy.exceptions.push(subtree_exception("phase0.config-subtree", "config", "src/config/approved", 1));
    policy.validate().expect("subtree exception");

    let paths = components(&[("src/config/approved/mod.rs", "config"), ("src/config/outside.rs", "config")]);
    let baseline = inventory(&[file("src/config/approved/mod.rs", 0, 0), file("src/config/outside.rs", 0, 0)]);
    let inside = inventory(&[file("src/config/approved/mod.rs", 0, 1), file("src/config/outside.rs", 0, 0)]);
    policy.compare_exception_scopes(&inside, &paths, &baseline, &paths).expect("growth inside approved subtree");

    let outside = inventory(&[file("src/config/approved/mod.rs", 0, 0), file("src/config/outside.rs", 0, 1)]);
    assert!(
        policy
            .compare_exception_scopes(&outside, &paths, &baseline, &paths)
            .unwrap_err()
            .to_string()
            .contains("escaped its approved")
    );
}

#[test]
fn later_subtree_exception_cannot_fund_growth_in_an_earlier_subtree() {
    let mut policy = policy(&[component("config", budget(0, 0), budget(0, 2))]);
    policy.exceptions.push(subtree_exception("phase0.config-a", "config", "src/config/a", 1));
    policy.exceptions.push(subtree_exception("phase0.config-b", "config", "src/config/b", 1));
    policy.validate().expect("two disjoint subtree exceptions");

    let paths = components(&[("src/config/a/mod.rs", "config"), ("src/config/b/mod.rs", "config")]);
    let baseline = inventory(&[file("src/config/a/mod.rs", 0, 0), file("src/config/b/mod.rs", 0, 0)]);
    let exact = inventory(&[file("src/config/a/mod.rs", 0, 1), file("src/config/b/mod.rs", 0, 1)]);
    policy
        .compare_exception_scopes(&exact, &paths, &baseline, &paths)
        .expect("each subtree uses only its own reviewed delta");

    let shifted = inventory(&[file("src/config/a/mod.rs", 0, 2), file("src/config/b/mod.rs", 0, 0)]);
    assert!(
        policy
            .compare_exception_scopes(&shifted, &paths, &baseline, &paths)
            .unwrap_err()
            .to_string()
            .contains("reviewed subtree ceiling")
    );
}

#[test]
fn distinct_pub_super_exception_subtrees_cannot_overlap() {
    let mut policy = policy(&[component("config", budget(0, 0), budget(0, 2))]);
    policy.exceptions.push(subtree_exception("phase0.config-parent", "config", "src/config", 1));
    policy.exceptions.push(subtree_exception("phase0.config-child", "config", "src/config/nested", 1));
    assert!(policy.validate().unwrap_err().to_string().contains("must not overlap"));
}

#[test]
fn malformed_subtrees_and_evidence_fail_closed() {
    let mut policy = policy(&[component("config", budget(0, 0), budget(0, 1))]);
    policy.exceptions.push(subtree_exception("phase0.config-subtree", "config", "src/config/../store", 1));
    assert!(policy.validate().unwrap_err().to_string().contains("normalized relative path"));

    policy.exceptions[0].subtree = Some("src/config".to_owned());
    policy.exceptions[0].pull_request = "not-a-pull-request".to_owned();
    assert!(policy.validate().unwrap_err().to_string().contains("must link to this repository"));
}

#[test]
fn initial_policy_has_exact_baseline_counts_and_no_exceptions() {
    let policy = policy(&[component("config", budget(1, 1), budget(1, 1))]);
    policy.validate_initial_policy().expect("closed initial policy");

    let mut reduced = policy.clone();
    reduced.components[0].current.pub_crate = 0;
    assert!(reduced.validate_initial_policy().unwrap_err().to_string().contains("exact baseline counts"));

    let mut exception = policy;
    exception.exceptions.push(subtree_exception("phase0.config-subtree", "config", "src/config", 1));
    assert!(exception.validate_initial_policy().unwrap_err().to_string().contains("no exceptions"));
}

fn policy(components: &[ComponentVisibility]) -> VisibilityPolicy {
    VisibilityPolicy {
        schema_version: CURRENT_SCHEMA_VERSION,
        baseline_commit: "b05f7a43345b39d40b456fb9ed46d479c4bf26e0".to_owned(),
        components: components.to_vec(),
        exceptions: Vec::new(),
    }
}

fn component(component: &str, baseline: VisibilityBudget, current: VisibilityBudget) -> ComponentVisibility {
    ComponentVisibility {
        component: component.to_owned(),
        baseline,
        current,
    }
}

const fn budget(pub_crate: usize, pub_super: usize) -> VisibilityBudget {
    VisibilityBudget { pub_crate, pub_super }
}

fn cross_component_exception(id: &str, component: &str, issue: &str, delta: usize) -> VisibilityException {
    let mut exception = exception(id, component, VisibilityKind::PubCrate);
    exception.delta = delta;
    exception.issue = issue.to_owned();
    exception
}

fn subtree_exception(id: &str, component: &str, subtree: &str, delta: usize) -> VisibilityException {
    let mut exception = exception(id, component, VisibilityKind::PubSuper);
    exception.delta = delta;
    exception.subtree = Some(subtree.to_owned());
    exception
}

fn exception(id: &str, component: &str, kind: VisibilityKind) -> VisibilityException {
    VisibilityException {
        id: id.to_owned(),
        component: component.to_owned(),
        kind,
        delta: 1,
        scope: match kind {
            VisibilityKind::PubCrate => VisibilityScope::CrossComponent,
            VisibilityKind::PubSuper => VisibilityScope::ComponentSubtree,
        },
        subtree: None,
        owner: "maintainers".to_owned(),
        issue: PHASE_ZERO_ISSUE.to_owned(),
        pull_request: "https://github.com/gearboxlogic/localhold/pull/500".to_owned(),
        rationale: "Reviewed architectural visibility growth".to_owned(),
        review_phase: "Phase 1 boundary restoration".to_owned(),
    }
}

fn inventory(files: &[FileMeasurement]) -> Inventory {
    Inventory { files: files.to_vec() }
}

fn file(path: &str, pub_crate: usize, pub_super: usize) -> FileMeasurement {
    FileMeasurement {
        path: path.to_owned(),
        physical_lines: 1,
        production_lines: 1,
        test_lines: 0,
        production_targets: vec!["crate".to_owned()],
        production_module: Vec::new(),
        production_internal_imports: Vec::new(),
        production_public_reexports: Vec::new(),
        production_type_declarations: Vec::new(),
        production_concrete_stores: ConcreteStoreCounts::default(),
        production_public_concrete_store_structs: ConcreteStoreSites::default(),
        production_concrete_store_sites: ConcreteStoreSites::default(),
        production_generic_default_store_sites: ConcreteStoreSites::default(),
        production_signature_store_sites: ConcreteStoreSignatureSites::default(),
        production_store_binding_sites: ConcreteStoreSites::default(),
        production_visibilities: VisibilityCounts { pub_crate, pub_super },
    }
}

fn components(entries: &[(&'static str, &'static str)]) -> BTreeMap<&'static str, &'static str> {
    entries.iter().copied().collect()
}
