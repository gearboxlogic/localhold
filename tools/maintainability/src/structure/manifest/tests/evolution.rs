use crate::structure::classify::Inventory;
use crate::structure::manifest::model::{HotspotKind, HotspotStatus, PathEvolutionKind, StructureManifest};

use super::support::{component, evolution, hotspot_manifest, inventory, ordinary_manifest, transfer};

#[test]
fn rename_requires_exact_measured_counts() {
    let previous = ordinary_manifest("src/lib.rs", 10, 10);
    let previous_files = inventory(&[("src/lib.rs", 12, 10)]);
    let mut current = previous.clone();
    current.components[0].paths = vec!["src/renamed.rs".to_owned()];
    current
        .path_evolutions
        .push(evolution("rename.lib", PathEvolutionKind::Rename, &["src/lib.rs"], &["src/renamed.rs"]));
    let exact = inventory(&[("src/renamed.rs", 12, 10)]);
    compare(&current, &previous, &previous_files, &exact).expect("exact rename passes");

    let changed = inventory(&[("src/renamed.rs", 11, 10)]);
    assert!(
        current
            .compare_policy(&previous, &previous_files, &changed)
            .unwrap_err()
            .to_string()
            .contains("preserve physical and production counts exactly")
    );
}

#[test]
fn governed_split_preserves_component_and_hotspot_debt() {
    let previous = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot.rs"], 900, 700);
    let previous_files = inventory(&[("src/hot.rs", 900, 700)]);
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
    let current_files = inventory(&[("src/hot/first.rs", 450, 350), ("src/hot/second.rs", 450, 350)]);

    compare(&current, &previous, &previous_files, &current_files).expect("unchanged split is governed without resetting debt");
}

#[test]
fn split_rejects_undeclared_growth_and_incomplete_mapping() {
    let previous = ordinary_manifest("src/lib.rs", 10, 10);
    let previous_files = inventory(&[("src/lib.rs", 10, 10)]);
    let mut current = previous.clone();
    current.components[0].paths = strings(&["src/first.rs", "src/second.rs"]);
    current.components[0].production_ceiling = 11;
    current
        .path_evolutions
        .push(evolution("split.lib", PathEvolutionKind::Split, &["src/lib.rs"], &["src/first.rs", "src/second.rs"]));
    let grown = inventory(&[("src/first.rs", 5, 5), ("src/second.rs", 6, 6)]);
    assert!(
        current
            .compare_policy(&previous, &previous_files, &grown)
            .unwrap_err()
            .to_string()
            .contains("cannot increase")
    );

    current.components[0].production_ceiling = 10;
    current.path_evolutions[0].successors.pop();
    let unchanged = inventory(&[("src/first.rs", 5, 5), ("src/second.rs", 5, 5)]);
    assert!(
        current
            .compare_policy(&previous, &previous_files, &unchanged)
            .unwrap_err()
            .to_string()
            .contains("do not exactly match")
    );
}

#[test]
fn test_extraction_preserves_production_and_requires_new_test_successor() {
    let previous = ordinary_manifest("src/lib.rs", 70, 70);
    let previous_files = inventory(&[("src/lib.rs", 100, 70)]);
    let mut current = previous.clone();
    current.components[0].paths = strings(&["src/lib.rs", "src/lib/tests.rs"]);
    current.path_evolutions.push(evolution(
        "extract.lib-tests",
        PathEvolutionKind::TestExtraction,
        &["src/lib.rs"],
        &["src/lib.rs", "src/lib/tests.rs"],
    ));
    let current_files = inventory(&[("src/lib.rs", 80, 70), ("src/lib/tests.rs", 20, 0)]);
    compare(&current, &previous, &previous_files, &current_files).expect("test extraction preserves the production ceiling");

    let no_test_successor = inventory(&[("src/lib.rs", 80, 70), ("src/lib/tests.rs", 20, 1)]);
    assert!(
        current
            .compare_policy(&previous, &previous_files, &no_test_successor)
            .unwrap_err()
            .to_string()
            .contains("preserve production")
    );

    current.path_evolutions[0].kind = PathEvolutionKind::Split;
    assert!(
        current
            .compare_policy(&previous, &previous_files, &current_files)
            .unwrap_err()
            .to_string()
            .contains("must be declared as test-extraction")
    );
}

#[test]
fn path_lineage_rejects_duplicate_source_existing_merge_and_resurrection() {
    let previous = ordinary_manifest("src/lib.rs", 10, 10);
    let previous_files = inventory(&[("src/lib.rs", 10, 10)]);
    let current_files = inventory(&[("src/first.rs", 5, 5), ("src/second.rs", 5, 5)]);

    let mut duplicate = previous.clone();
    duplicate.components[0].paths = strings(&["src/first.rs", "src/second.rs"]);
    duplicate.path_evolutions = vec![
        evolution("split.first", PathEvolutionKind::Split, &["src/lib.rs"], &["src/first.rs", "src/second.rs"]),
        evolution("split.second", PathEvolutionKind::Rename, &["src/lib.rs"], &["src/first.rs"]),
    ];
    assert!(
        duplicate
            .compare_policy(&previous, &previous_files, &current_files)
            .unwrap_err()
            .to_string()
            .contains("consumed more than once")
    );

    let mut existing = previous.clone();
    existing.components[0].paths.push("src/existing.rs".to_owned());
    existing.components[0].production_ceiling = 15;
    let previous_existing = inventory(&[("src/lib.rs", 10, 10), ("src/existing.rs", 5, 5)]);
    let mut merged = existing.clone();
    merged
        .path_evolutions
        .push(evolution("split.merge", PathEvolutionKind::Split, &["src/lib.rs"], &["src/lib.rs", "src/existing.rs"]));
    assert!(
        merged
            .compare_policy(&existing, &previous_existing, &previous_existing)
            .unwrap_err()
            .to_string()
            .contains("cannot merge")
    );

    let mut historical = previous;
    historical.components[0].paths = vec!["src/current.rs".to_owned()];
    historical
        .path_evolutions
        .push(evolution("rename.old", PathEvolutionKind::Rename, &["src/lib.rs"], &["src/current.rs"]));
    let historical_files = inventory(&[("src/current.rs", 10, 10)]);
    let mut resurrected = historical.clone();
    resurrected.components[0].paths = vec!["src/lib.rs".to_owned()];
    resurrected
        .path_evolutions
        .push(evolution("rename.back", PathEvolutionKind::Rename, &["src/current.rs"], &["src/lib.rs"]));
    assert!(
        resurrected.validate_current().unwrap_err().to_string().contains("round trip")
            || resurrected
                .compare_policy(&historical, &historical_files, &previous_files)
                .unwrap_err()
                .to_string()
                .contains("resurrect")
    );
}

#[test]
fn cross_component_transfer_is_exact_zero_sum_and_follows_hotspot_lineage() {
    let (previous, previous_files, mut current, current_files) = transfer_fixture();
    compare(&current, &previous, &previous_files, &current_files).expect("exact classified production transfer passes");

    current.component_transfers[0].production_lines = 39;
    assert!(
        current
            .compare_policy(&previous, &previous_files, &current_files)
            .unwrap_err()
            .to_string()
            .contains("amount mismatch")
    );
}

#[test]
fn same_path_component_reassignment_uses_exact_transfer_lineage() {
    let mut previous = ordinary_manifest("src/shared.rs", 100, 100);
    previous.components[0].id = "source".to_owned();
    previous.components[0].paths = strings(&["src/source.rs", "src/shared.rs"]);
    previous.components.push(component("destination", "src/destination.rs", "src/destination.rs", 10));
    let previous_files = inventory(&[("src/source.rs", 60, 60), ("src/shared.rs", 40, 40), ("src/destination.rs", 10, 10)]);

    let mut current = previous.clone();
    current.components[0].paths = vec!["src/source.rs".to_owned()];
    current.components[0].production_ceiling = 60;
    current.components[1].paths = strings(&["src/destination.rs", "src/shared.rs"]);
    current.components[1].production_ceiling = 50;
    current
        .path_evolutions
        .push(evolution("move.shared-ownership", PathEvolutionKind::Rename, &["src/shared.rs"], &["src/shared.rs"]));
    current.component_transfers.push(transfer(
        "transfer.shared-ownership",
        ("source", "destination"),
        40,
        &["src/shared.rs"],
        "move.shared-ownership",
    ));
    let current_files = previous_files.clone();

    compare(&current, &previous, &previous_files, &current_files).expect("same-path responsibility move is governed by exact transfer evidence");

    let mut test_only = current;
    test_only.component_transfers.clear();
    let test_files = inventory(&[("src/source.rs", 60, 60), ("src/shared.rs", 40, 0), ("src/destination.rs", 10, 10)]);
    assert!(
        test_only
            .compare_policy(&previous, &previous_files, &test_files)
            .unwrap_err()
            .to_string()
            .contains("must rename the path")
    );
}

#[test]
fn cross_component_transfer_rejects_missing_duplicate_and_net_growth() {
    let (previous, previous_files, mut current, current_files) = transfer_fixture();
    current.component_transfers.clear();
    assert!(
        current
            .compare_policy(&previous, &previous_files, &current_files)
            .unwrap_err()
            .to_string()
            .contains("component transfer paths")
    );

    let (_, _, mut duplicated, _) = transfer_fixture();
    duplicated
        .component_transfers
        .push(transfer("transfer.duplicate", ("source", "destination"), 40, &["src/moved.rs"], "split.across-components"));
    assert!(
        duplicated
            .compare_policy(&previous, &previous_files, &current_files)
            .unwrap_err()
            .to_string()
            .contains("more than once")
    );

    let (_, _, mut grown, _) = transfer_fixture();
    grown.components[0].production_ceiling = 70;
    grown.components[1].production_ceiling = 50;
    let grown_files = inventory(&[("src/source.rs", 70, 70), ("src/destination.rs", 10, 10), ("src/moved.rs", 40, 40)]);
    assert!(
        grown
            .compare_policy(&previous, &previous_files, &grown_files)
            .unwrap_err()
            .to_string()
            .contains("cannot increase")
    );
}

#[test]
fn append_only_ledgers_and_component_graph_block_rewrite_and_round_trip() {
    let (_previous, previous_files, mut current, current_files) = transfer_fixture();
    let mut historical = current.clone();
    historical.path_evolutions[0].rationale = "rewritten history".to_owned();
    assert!(
        historical
            .compare_policy(&current, &current_files, &current_files)
            .unwrap_err()
            .to_string()
            .contains("append-only")
    );

    current.path_evolutions.push(evolution(
        "split.reverse",
        PathEvolutionKind::Split,
        &["src/moved.rs"],
        &["src/destination/kept.rs", "src/returned.rs"],
    ));
    current
        .component_transfers
        .push(transfer("transfer.reverse", ("destination", "source"), 10, &["src/returned.rs"], "split.reverse"));
    assert!(current.validate_current().unwrap_err().to_string().contains("round trip"));

    // The fixture itself remains valid evidence after the adversarial mutations.
    let (previous, _, current, _) = transfer_fixture();
    previous.validate_previous().expect("valid previous fixture");
    current.validate_current().expect("valid current fixture");
    let _ = previous_files;
}

#[test]
fn immutable_identity_status_and_undeclared_path_changes_still_fail() {
    let previous = hotspot_manifest(HotspotKind::Production, HotspotStatus::Resolved, &["src/hot.rs"], 900, 800);
    let files = inventory(&[("src/hot.rs", 900, 800)]);

    let mut reactivated = previous.clone();
    reactivated.hotspots[0].status = HotspotStatus::Active;
    assert!(
        reactivated
            .compare_policy(&previous, &files, &files)
            .unwrap_err()
            .to_string()
            .contains("cannot be reactivated")
    );

    let mut undeclared = previous.clone();
    undeclared.components[0].paths = vec!["src/renamed.rs".to_owned()];
    undeclared.hotspots[0].successors = vec!["src/renamed.rs".to_owned()];
    let renamed = inventory(&[("src/renamed.rs", 900, 800)]);
    assert!(
        undeclared
            .compare_policy(&previous, &files, &renamed)
            .unwrap_err()
            .to_string()
            .contains("do not exactly match")
    );
}

fn transfer_fixture() -> (StructureManifest, Inventory, StructureManifest, Inventory) {
    let mut previous = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/whole.rs"], 900, 100);
    previous.components[0].id = "source".to_owned();
    previous.components[0].baseline_paths = vec!["src/whole.rs".to_owned()];
    previous.components[0].paths = vec!["src/whole.rs".to_owned()];
    previous.hotspots[0].component = "source".to_owned();
    previous.hotspots[0].baseline_path = "src/whole.rs".to_owned();
    previous.components.push(component("destination", "src/destination.rs", "src/destination.rs", 10));
    let previous_files = inventory(&[("src/whole.rs", 900, 100), ("src/destination.rs", 10, 10)]);

    let mut current = previous.clone();
    current.components[0].paths = vec!["src/source.rs".to_owned()];
    current.components[0].production_ceiling = 60;
    current.components[1].paths = strings(&["src/destination.rs", "src/moved.rs"]);
    current.components[1].production_ceiling = 50;
    current.hotspots[0].status = HotspotStatus::Resolved;
    current.hotspots[0].successors = strings(&["src/source.rs", "src/moved.rs"]);
    current.path_evolutions.push(evolution(
        "split.across-components",
        PathEvolutionKind::Split,
        &["src/whole.rs"],
        &["src/source.rs", "src/moved.rs"],
    ));
    current.component_transfers.push(transfer(
        "transfer.source-to-destination",
        ("source", "destination"),
        40,
        &["src/moved.rs"],
        "split.across-components",
    ));
    let current_files = inventory(&[("src/source.rs", 600, 60), ("src/destination.rs", 10, 10), ("src/moved.rs", 300, 40)]);
    (previous, previous_files, current, current_files)
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
