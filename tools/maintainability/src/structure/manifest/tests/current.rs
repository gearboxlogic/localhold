use crate::structure::manifest::model::{HotspotKind, HotspotStatus};

use super::support::{adjustment, file, hotspot_manifest, inventory, ordinary_manifest};

#[test]
fn exact_component_budget_passes_but_growth_and_stale_ceiling_fail() {
    let observed = inventory(&[("src/lib.rs", 10, 10)]);
    let exact = ordinary_manifest("src/lib.rs", 10, 10);
    exact.validate_current().expect("valid fixture");
    exact.compare_current(&observed).expect("exact ceiling passes");

    let growth = ordinary_manifest("src/lib.rs", 9, 9);
    assert!(growth.compare_current(&observed).unwrap_err().to_string().contains("growth rejected"));

    let stale = ordinary_manifest("src/lib.rs", 11, 11);
    assert!(stale.compare_current(&observed).unwrap_err().to_string().contains("must be lowered"));
}

#[test]
fn current_file_map_rejects_unmapped_and_deleted_paths() {
    let manifest = ordinary_manifest("src/lib.rs", 10, 10);
    let unmapped = inventory(&[("src/lib.rs", 10, 10), ("src/new.rs", 1, 0)]);
    assert!(manifest.compare_current(&unmapped).unwrap_err().to_string().contains("unmapped"));

    let missing = inventory(&[]);
    assert!(manifest.compare_current(&missing).unwrap_err().to_string().contains("missing"));
}

#[test]
fn mapped_file_proliferation_cannot_create_component_headroom() {
    let mut manifest = ordinary_manifest("src/lib.rs", 10, 10);
    manifest.components[0].paths.push("src/new.rs".to_owned());
    let observed = inventory(&[("src/lib.rs", 10, 10), ("src/new.rs", 1, 1)]);
    assert!(manifest.compare_current(&observed).unwrap_err().to_string().contains("growth rejected"));
}

#[test]
fn ordinary_production_and_test_file_caps_are_distinct() {
    let production = ordinary_manifest("src/lib.rs", 801, 801);
    let production_inventory = inventory(&[("src/lib.rs", 801, 801)]);
    assert!(production.compare_current(&production_inventory).unwrap_err().to_string().contains("800-line production"));

    let test = ordinary_manifest("tests/contract.rs", 0, 0);
    let test_inventory = inventory(&[("tests/contract.rs", 1_001, 0)]);
    assert!(test.compare_current(&test_inventory).unwrap_err().to_string().contains("1000-line test"));
}

#[test]
fn active_hotspot_rejects_physical_or_production_growth() {
    let manifest = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot.rs"], 900, 800);
    let physical_growth = inventory(&[("src/hot.rs", 901, 800)]);
    assert!(manifest.compare_current(&physical_growth).unwrap_err().to_string().contains("physical growth rejected"));

    let production_growth = inventory(&[("src/hot.rs", 900, 801)]);
    assert!(manifest.compare_current(&production_growth).unwrap_err().to_string().contains("production growth rejected"));
}

#[test]
fn complete_subcap_split_closes_hotspot_without_resetting_aggregate_budget() {
    let manifest = hotspot_manifest(HotspotKind::Production, HotspotStatus::Resolved, &["src/hot/first.rs", "src/hot/second.rs"], 900, 900);
    manifest.validate_current().expect("valid split");
    manifest
        .compare_current(&inventory(&[("src/hot/first.rs", 450, 450), ("src/hot/second.rs", 450, 450)]))
        .expect("complete unchanged split passes");

    let grown = inventory(&[("src/hot/first.rs", 451, 451), ("src/hot/second.rs", 450, 450)]);
    assert!(manifest.compare_current(&grown).is_err());
}

#[test]
fn active_subcap_successors_must_be_marked_resolved() {
    let manifest = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot/first.rs", "src/hot/second.rs"], 900, 900);
    let observed = inventory(&[("src/hot/first.rs", 450, 450), ("src/hot/second.rs", 450, 450)]);
    assert!(manifest.compare_current(&observed).unwrap_err().to_string().contains("must be marked resolved"));
}

#[test]
fn verified_pre_gate_adjustment_reconciles_without_resetting_baseline() {
    let mut manifest = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot.rs"], 900, 800);
    manifest.pre_gate_adjustments.push(adjustment());
    manifest.components[0].production_ceiling = 803;
    manifest.hotspots[0].physical_ceiling = 903;
    manifest.hotspots[0].production_ceiling = 803;
    manifest.validate_current().expect("reviewed adjustment is valid");
    manifest
        .compare_baseline(&inventory(&[("src/hot.rs", 900, 800)]))
        .expect("original baseline remains verified");
    manifest
        .compare_current(&inventory(&[("src/hot.rs", 903, 803)]))
        .expect("closed adjustment reconciles current state");
}

#[test]
fn malformed_inventory_count_is_rejected_without_overflow() {
    let manifest = ordinary_manifest("src/lib.rs", 1, 1);
    let mut malformed = file("src/lib.rs", 1, 1);
    malformed.test_lines = usize::MAX;
    assert!(
        manifest
            .compare_current(&crate::structure::classify::Inventory { files: vec![malformed] })
            .unwrap_err()
            .to_string()
            .contains("overflow")
    );
}
