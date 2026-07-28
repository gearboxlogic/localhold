use std::collections::BTreeSet;

use crate::structure::classify::Inventory;
use crate::structure::manifest::model::{FeatureFreezeStatus, HotspotKind, HotspotStatus, PathEvolutionKind, SplitAllowanceStatus, StructureManifest};

use super::support::{component, evolution, hotspot_manifest, inventory, split_allowance};

struct Scenario {
    previous: StructureManifest,
    previous_files: Inventory,
    current: StructureManifest,
    current_files: Inventory,
}

#[test]
fn first_hotspot_split_can_use_exact_three_percent_overhead() {
    let scenario = approved_production_split();
    compare(&scenario.current, &scenario.previous, &scenario.previous_files, &scenario.current_files).expect("first split accepts exact physical and production maxima");

    let mut excessive = scenario.current.clone();
    excessive.split_allowances[0].approved_physical_lines = 31;
    excessive.split_allowances[0].current_physical_lines = 31;
    let excessive_files = inventory(&[("src/hot/first.rs", 516, 412), ("src/hot/second.rs", 515, 412)]);
    assert!(
        excessive
            .compare_policy(&scenario.previous, &scenario.previous_files, &excessive_files)
            .unwrap_err()
            .to_string()
            .contains("exceeds 3%")
    );

    let mut inexact = scenario.current;
    inexact.split_allowances[0].approved_physical_lines = 29;
    inexact.split_allowances[0].current_physical_lines = 29;
    assert!(
        inexact
            .compare_policy(&scenario.previous, &scenario.previous_files, &scenario.current_files)
            .unwrap_err()
            .to_string()
            .contains("exact measured split overhead")
    );
}

#[test]
fn retained_source_uses_only_lines_moved_out_as_the_percentage_base() {
    let mut previous = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot.rs"], 1_000, 800);
    let previous_files = inventory(&[("src/hot.rs", 1_000, 800)]);
    let mut current = previous.clone();
    current.components[0].paths = strings(&["src/hot.rs", "src/hot/extracted.rs"]);
    current.hotspots[0].status = HotspotStatus::Resolved;
    current.hotspots[0].successors = strings(&["src/hot.rs", "src/hot/extracted.rs"]);
    current
        .path_evolutions
        .push(evolution("split.hot", PathEvolutionKind::Split, &["src/hot.rs"], &["src/hot.rs", "src/hot/extracted.rs"]));
    current.split_allowances.push(split_allowance("component.hot", "split.hot", 12, 9));
    let current_files = inventory(&[("src/hot.rs", 600, 480), ("src/hot/extracted.rs", 412, 329)]);
    compare(&current, &previous, &previous_files, &current_files).expect("3% of the 400/320 moved lines passes");

    previous.hotspots[0].physical_ceiling = 1_000;
    current.split_allowances[0].approved_physical_lines = 13;
    current.split_allowances[0].current_physical_lines = 13;
    let excessive_files = inventory(&[("src/hot.rs", 600, 480), ("src/hot/extracted.rs", 413, 329)]);
    assert!(
        current
            .compare_policy(&previous, &previous_files, &excessive_files)
            .unwrap_err()
            .to_string()
            .contains("exceeds 3%")
    );
}

#[test]
fn test_hotspot_split_has_no_production_allowance() {
    let previous = hotspot_manifest(HotspotKind::Test, HotspotStatus::Active, &["tests/hot.rs"], 1_100, 0);
    let previous_files = inventory(&[("tests/hot.rs", 1_100, 0)]);
    let mut current = previous.clone();
    current.components[0].paths = strings(&["tests/hot/first.rs", "tests/hot/second.rs"]);
    current.hotspots[0].status = HotspotStatus::Resolved;
    current.hotspots[0].successors = strings(&["tests/hot/first.rs", "tests/hot/second.rs"]);
    current.path_evolutions.push(evolution(
        "split.hot",
        PathEvolutionKind::Split,
        &["tests/hot.rs"],
        &["tests/hot/first.rs", "tests/hot/second.rs"],
    ));
    current.split_allowances.push(split_allowance("component.hot", "split.hot", 33, 0));
    let current_files = inventory(&[("tests/hot/first.rs", 567, 0), ("tests/hot/second.rs", 566, 0)]);

    compare(&current, &previous, &previous_files, &current_files).expect("test-only split uses physical overhead only");
}

#[test]
fn split_growth_requires_complete_future_dated_evidence() {
    let scenario = approved_production_split();

    let mut missing = scenario.current.clone();
    missing.split_allowances.clear();
    assert!(
        missing
            .compare_policy(&scenario.previous, &scenario.previous_files, &scenario.current_files)
            .unwrap_err()
            .to_string()
            .contains("cannot increase physical or production counts")
    );

    let mut due = scenario.current.clone();
    due.split_allowances[0].due_phase = 0;
    assert!(
        due.compare_policy(&scenario.previous, &scenario.previous_files, &scenario.current_files)
            .unwrap_err()
            .to_string()
            .contains("future due phase")
    );

    let mut missing_owner = scenario.current;
    missing_owner.split_allowances[0].owner.clear();
    assert!(missing_owner.validate_current().unwrap_err().to_string().contains("owner must not be empty"));
}

#[test]
fn later_successor_touch_strictly_reduces_each_outstanding_class() {
    let scenario = approved_production_split();
    let mut reduced = scenario.current.clone();
    reduced.split_allowances[0].current_physical_lines = 29;
    reduced.split_allowances[0].current_production_lines = 23;
    let reduced_files = inventory(&[("src/hot/first.rs", 514, 411), ("src/hot/second.rs", 515, 412)]);
    compare(&reduced, &scenario.current, &scenario.current_files, &reduced_files).expect("touch reduces both outstanding classes");

    let mut partial = scenario.current.clone();
    partial.split_allowances[0].current_physical_lines = 29;
    let partial_files = inventory(&[("src/hot/first.rs", 514, 412), ("src/hot/second.rs", 515, 412)]);
    assert!(
        partial
            .compare_policy(&scenario.current, &scenario.current_files, &partial_files)
            .unwrap_err()
            .to_string()
            .contains("strictly reduce every outstanding")
    );

    let touched = BTreeSet::from(["src/hot/first.rs".to_owned()]);
    assert!(
        scenario
            .current
            .compare_policy_with_touched(&scenario.current, &scenario.current_files, &scenario.current_files, &touched)
            .unwrap_err()
            .to_string()
            .contains("strictly reduce every outstanding")
    );
}

#[test]
fn overdue_untouched_debt_survives_but_any_touch_must_reduce() {
    let scenario = approved_production_split();
    let mut overdue = scenario.current.clone();
    overdue.program_phase = 1;
    overdue
        .compare_policy(&scenario.current, &scenario.current_files, &scenario.current_files)
        .expect("unrelated work does not force an overdue successor rewrite");

    let touched = BTreeSet::from(["src/hot/second.rs".to_owned()]);
    assert!(
        overdue
            .compare_policy_with_touched(&scenario.current, &scenario.current_files, &scenario.current_files, &touched)
            .unwrap_err()
            .to_string()
            .contains("overdue split allowance")
    );
}

#[test]
fn repayment_resolves_once_and_cannot_be_renewed() {
    let scenario = approved_production_split();
    let mut repaid = scenario.current.clone();
    repaid.split_allowances[0].status = SplitAllowanceStatus::Resolved;
    repaid.split_allowances[0].current_physical_lines = 0;
    repaid.split_allowances[0].current_production_lines = 0;
    let repaid_files = inventory(&[("src/hot/first.rs", 500, 400), ("src/hot/second.rs", 500, 400)]);
    compare(&repaid, &scenario.current, &scenario.current_files, &repaid_files).expect("full repayment resolves the allowance");

    let mut reactivated = repaid.clone();
    reactivated.split_allowances[0].status = SplitAllowanceStatus::Active;
    assert!(
        reactivated
            .compare_policy(&repaid, &repaid_files, &repaid_files)
            .unwrap_err()
            .to_string()
            .contains("resolved split allowance")
    );

    let mut renewed = repaid;
    let mut replacement = split_allowance("component.hot", "split.hot", 1, 1);
    replacement.id = "component.hot.replacement-overhead".to_owned();
    renewed.split_allowances.push(replacement);
    assert!(renewed.validate_current().unwrap_err().to_string().contains("more than one split allowance"));
}

#[test]
fn later_successor_resplit_cannot_claim_the_first_split_allowance() {
    let previous = previously_split_hotspot();
    let previous_files = inventory(&[("src/hot/first.rs", 900, 700), ("src/hot/second.rs", 100, 100)]);
    let mut current = previous.clone();
    current.components[0].paths = strings(&["src/hot/a.rs", "src/hot/b.rs", "src/hot/second.rs"]);
    current.hotspots[0].status = HotspotStatus::Resolved;
    current.hotspots[0].successors = strings(&["src/hot/a.rs", "src/hot/b.rs", "src/hot/second.rs"]);
    current.path_evolutions.push(evolution(
        "split.hot-again",
        PathEvolutionKind::Split,
        &["src/hot/first.rs"],
        &["src/hot/a.rs", "src/hot/b.rs"],
    ));
    current.split_allowances.push(split_allowance("component.hot", "split.hot-again", 27, 21));
    let current_files = inventory(&[("src/hot/a.rs", 464, 361), ("src/hot/b.rs", 463, 360), ("src/hot/second.rs", 100, 100)]);

    assert!(
        current
            .compare_policy(&previous, &previous_files, &current_files)
            .unwrap_err()
            .to_string()
            .contains("already used its one structural split allowance")
    );
}

#[test]
fn active_allowance_cannot_transfer_or_rewrite_approval() {
    let scenario = approved_production_split();
    let mut transferred = scenario.current.clone();
    transferred.components[0].paths.pop();
    transferred.components.push(component("other", "src/other.rs", "src/hot/second.rs", 412));
    assert!(transferred.validate_current().unwrap_err().to_string().contains("cannot be transferred"));

    let mut rewritten = scenario.current.clone();
    rewritten.split_allowances[0].recovery_issue = "https://example.invalid/issues/different".to_owned();
    assert!(
        rewritten
            .compare_policy(&scenario.current, &scenario.current_files, &scenario.current_files)
            .unwrap_err()
            .to_string()
            .contains("approval evidence is immutable")
    );
}

#[test]
fn allowance_capacity_cannot_fund_unrelated_component_growth() {
    let scenario = approved_production_split();
    let mut previous = scenario.current;
    previous.components[0].baseline_paths.push("src/other.rs".to_owned());
    previous.components[0].paths.push("src/other.rs".to_owned());
    previous.components[0].baseline_production_lines = 890;
    previous.components[0].production_ceiling = 890;
    let previous_files = inventory(&[("src/hot/first.rs", 515, 412), ("src/hot/second.rs", 515, 412), ("src/other.rs", 80, 80)]);
    previous.compare_current(&previous_files).expect("unused component allowance capacity is valid");

    let current = previous.clone();
    let current_files = inventory(&[("src/hot/first.rs", 515, 412), ("src/hot/second.rs", 515, 412), ("src/other.rs", 81, 81)]);
    current
        .compare_current(&current_files)
        .expect("canonical plus allowance capacity alone permits the observed total");
    assert!(
        current
            .compare_policy(&previous, &previous_files, &current_files)
            .unwrap_err()
            .to_string()
            .contains("cannot increase beyond governed transfer and split overhead")
    );
}

#[test]
fn invalid_allowance_states_and_ledger_removal_are_rejected() {
    let scenario = approved_production_split();

    let mut impossible_classification = scenario.current.clone();
    impossible_classification.split_allowances[0].approved_physical_lines = 23;
    assert!(
        impossible_classification
            .validate_current()
            .unwrap_err()
            .to_string()
            .contains("production overhead cannot exceed physical")
    );

    let mut empty_active = scenario.current.clone();
    empty_active.split_allowances[0].current_physical_lines = 0;
    empty_active.split_allowances[0].current_production_lines = 0;
    assert!(empty_active.validate_current().unwrap_err().to_string().contains("must be resolved"));

    let mut unresolved = scenario.current.clone();
    unresolved.split_allowances[0].status = SplitAllowanceStatus::Resolved;
    assert!(unresolved.validate_current().unwrap_err().to_string().contains("must have zero remaining overhead"));

    let mut removed = scenario.current.clone();
    removed.split_allowances.clear();
    assert!(
        removed
            .compare_policy(&scenario.current, &scenario.current_files, &scenario.current_files)
            .unwrap_err()
            .to_string()
            .contains("append-only")
    );
}

#[test]
fn feature_freeze_exit_requires_full_repayment_and_is_irreversible() {
    let scenario = approved_production_split();
    let mut premature = scenario.current.clone();
    premature.feature_freeze = FeatureFreezeStatus::Exited;
    assert!(
        premature
            .validate_current()
            .unwrap_err()
            .to_string()
            .contains("cannot exit with outstanding split overhead")
    );

    let mut exited = scenario.current.clone();
    exited.feature_freeze = FeatureFreezeStatus::Exited;
    exited.split_allowances[0].status = SplitAllowanceStatus::Resolved;
    exited.split_allowances[0].current_physical_lines = 0;
    exited.split_allowances[0].current_production_lines = 0;
    let repaid_files = inventory(&[("src/hot/first.rs", 500, 400), ("src/hot/second.rs", 500, 400)]);
    compare(&exited, &scenario.current, &scenario.current_files, &repaid_files).expect("fully repaid allowance permits freeze exit");

    let mut reentered = exited.clone();
    reentered.feature_freeze = FeatureFreezeStatus::Active;
    assert!(
        reentered
            .compare_policy(&exited, &repaid_files, &repaid_files)
            .unwrap_err()
            .to_string()
            .contains("irreversible")
    );
}

fn approved_production_split() -> Scenario {
    let previous = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot.rs"], 1_000, 800);
    let previous_files = inventory(&[("src/hot.rs", 1_000, 800)]);
    let mut current = previous.clone();
    current.components[0].paths = strings(&["src/hot/first.rs", "src/hot/second.rs"]);
    current.hotspots[0].status = HotspotStatus::Resolved;
    current.hotspots[0].successors = strings(&["src/hot/first.rs", "src/hot/second.rs"]);
    current.path_evolutions.push(evolution(
        "split.hot",
        PathEvolutionKind::Split,
        &["src/hot.rs"],
        &["src/hot/first.rs", "src/hot/second.rs"],
    ));
    current.split_allowances.push(split_allowance("component.hot", "split.hot", 30, 24));
    let current_files = inventory(&[("src/hot/first.rs", 515, 412), ("src/hot/second.rs", 515, 412)]);
    Scenario {
        previous,
        previous_files,
        current,
        current_files,
    }
}

fn previously_split_hotspot() -> StructureManifest {
    let mut manifest = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot/first.rs", "src/hot/second.rs"], 1_000, 800);
    manifest.path_evolutions.push(evolution(
        "split.hot",
        PathEvolutionKind::Split,
        &["src/hot.rs"],
        &["src/hot/first.rs", "src/hot/second.rs"],
    ));
    manifest
}

fn compare(current: &StructureManifest, previous: &StructureManifest, previous_files: &Inventory, current_files: &Inventory) -> anyhow::Result<()> {
    previous.validate_previous()?;
    current.validate_current()?;
    previous.compare_current(previous_files)?;
    current.compare_current(current_files)?;
    current.compare_policy(previous, previous_files, current_files)
}

fn strings(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}
