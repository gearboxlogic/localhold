use std::fs;
use std::path::Path;
use std::process::Command;

use super::{Hotspot, HotspotKind, HotspotStatus, Limits, LogicalComponent, PreGateAdjustment, StructureManifest};
use crate::structure::classify::{FileMeasurement, Inventory};

fn file(path: &str, physical: usize, production: usize) -> FileMeasurement {
    FileMeasurement {
        path: path.to_owned(),
        physical_lines: physical,
        production_lines: production,
        test_lines: physical - production,
    }
}

fn ordinary_manifest(path: &str, baseline_production: usize, current_production: usize) -> StructureManifest {
    StructureManifest {
        schema_version: 1,
        baseline_commit: "a".repeat(40),
        tracked_roots: vec!["src".to_owned(), "tests".to_owned(), "benches".to_owned()],
        limits: Limits {
            production_file_physical_lines: 800,
            test_file_physical_lines: 1_000,
        },
        pre_gate_adjustments: Vec::new(),
        components: vec![LogicalComponent {
            id: "component".to_owned(),
            baseline_paths: vec![path.to_owned()],
            paths: vec![path.to_owned()],
            baseline_production_lines: baseline_production,
            production_ceiling: current_production,
        }],
        hotspots: Vec::new(),
    }
}

fn hotspot_manifest(kind: HotspotKind, status: HotspotStatus, successors: &[&str], physical: usize, production: usize) -> StructureManifest {
    let paths = successors.iter().map(|path| (*path).to_owned()).collect();
    StructureManifest {
        schema_version: 1,
        baseline_commit: "a".repeat(40),
        tracked_roots: vec!["src".to_owned(), "tests".to_owned(), "benches".to_owned()],
        limits: Limits {
            production_file_physical_lines: 800,
            test_file_physical_lines: 1_000,
        },
        pre_gate_adjustments: Vec::new(),
        components: vec![LogicalComponent {
            id: "component".to_owned(),
            baseline_paths: vec!["src/hot.rs".to_owned()],
            paths,
            baseline_production_lines: production,
            production_ceiling: production,
        }],
        hotspots: vec![Hotspot {
            id: "component.hot".to_owned(),
            component: "component".to_owned(),
            kind,
            status,
            baseline_path: "src/hot.rs".to_owned(),
            baseline_physical_lines: physical,
            baseline_production_lines: production,
            successors: successors.iter().map(|path| (*path).to_owned()).collect(),
            physical_ceiling: physical,
            production_ceiling: production,
        }],
    }
}

#[test]
fn exact_component_budget_passes_but_growth_and_stale_ceiling_fail() {
    let inventory = Inventory {
        files: vec![file("src/lib.rs", 10, 10)],
    };
    let exact = ordinary_manifest("src/lib.rs", 10, 10);
    exact.validate().expect("valid fixture");
    exact.compare_current(&inventory).expect("exact ceiling passes");

    let growth = ordinary_manifest("src/lib.rs", 9, 9);
    assert!(growth.compare_current(&inventory).unwrap_err().to_string().contains("growth rejected"));

    let stale = ordinary_manifest("src/lib.rs", 11, 11);
    assert!(stale.compare_current(&inventory).unwrap_err().to_string().contains("must be lowered"));
}

#[test]
fn current_file_map_rejects_unmapped_and_deleted_paths() {
    let manifest = ordinary_manifest("src/lib.rs", 10, 10);
    let unmapped = Inventory {
        files: vec![file("src/lib.rs", 10, 10), file("src/new.rs", 1, 0)],
    };
    assert!(manifest.compare_current(&unmapped).unwrap_err().to_string().contains("unmapped"));

    let missing = Inventory { files: Vec::new() };
    assert!(manifest.compare_current(&missing).unwrap_err().to_string().contains("missing"));
}

#[test]
fn mapped_file_proliferation_cannot_create_component_headroom() {
    let mut manifest = ordinary_manifest("src/lib.rs", 10, 10);
    manifest.components[0].paths.push("src/new.rs".to_owned());
    let inventory = Inventory {
        files: vec![file("src/lib.rs", 10, 10), file("src/new.rs", 1, 1)],
    };
    assert!(manifest.compare_current(&inventory).unwrap_err().to_string().contains("growth rejected"));
}

#[test]
fn ordinary_production_and_test_file_caps_are_distinct() {
    let production = ordinary_manifest("src/lib.rs", 801, 801);
    let production_inventory = Inventory {
        files: vec![file("src/lib.rs", 801, 801)],
    };
    assert!(production.compare_current(&production_inventory).unwrap_err().to_string().contains("800-line production"));

    let test = ordinary_manifest("tests/contract.rs", 0, 0);
    let test_inventory = Inventory {
        files: vec![file("tests/contract.rs", 1_001, 0)],
    };
    assert!(test.compare_current(&test_inventory).unwrap_err().to_string().contains("1000-line test"));
}

#[test]
fn active_hotspot_rejects_physical_or_production_growth() {
    let manifest = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot.rs"], 900, 800);
    let physical_growth = Inventory {
        files: vec![file("src/hot.rs", 901, 800)],
    };
    assert!(manifest.compare_current(&physical_growth).unwrap_err().to_string().contains("physical growth rejected"));

    let production_growth = Inventory {
        files: vec![file("src/hot.rs", 900, 801)],
    };
    assert!(manifest.compare_current(&production_growth).unwrap_err().to_string().contains("production growth rejected"));
}

#[test]
fn complete_subcap_split_closes_the_hotspot_without_resetting_aggregate_budget() {
    let manifest = hotspot_manifest(HotspotKind::Production, HotspotStatus::Resolved, &["src/hot/first.rs", "src/hot/second.rs"], 900, 900);
    manifest.validate().expect("valid split");
    manifest
        .compare_current(&Inventory {
            files: vec![file("src/hot/first.rs", 450, 450), file("src/hot/second.rs", 450, 450)],
        })
        .expect("complete unchanged split passes");

    let grown = Inventory {
        files: vec![file("src/hot/first.rs", 451, 451), file("src/hot/second.rs", 450, 450)],
    };
    assert!(manifest.compare_current(&grown).is_err());
}

#[test]
fn active_subcap_successors_must_be_marked_resolved() {
    let manifest = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot/first.rs", "src/hot/second.rs"], 900, 900);
    let inventory = Inventory {
        files: vec![file("src/hot/first.rs", 450, 450), file("src/hot/second.rs", 450, 450)],
    };
    assert!(manifest.compare_current(&inventory).unwrap_err().to_string().contains("must be marked resolved"));
}

#[test]
fn verified_pre_gate_adjustment_reconciles_without_resetting_baseline() {
    let mut manifest = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot.rs"], 900, 800);
    manifest.pre_gate_adjustments.push(PreGateAdjustment {
        id: "phase0.reviewed-adjustment".to_owned(),
        component: "component".to_owned(),
        hotspot: "component.hot".to_owned(),
        physical_lines: 3,
        production_lines: 3,
        issue: "issue".to_owned(),
        pull_request: "pull request".to_owned(),
        rationale: "reviewed before the gate existed".to_owned(),
    });
    manifest.components[0].production_ceiling = 803;
    manifest.hotspots[0].physical_ceiling = 903;
    manifest.hotspots[0].production_ceiling = 803;
    manifest.validate().expect("reviewed adjustment is valid");
    manifest
        .compare_baseline(&Inventory {
            files: vec![file("src/hot.rs", 900, 800)],
        })
        .expect("original baseline remains verified");
    manifest
        .compare_current(&Inventory {
            files: vec![file("src/hot.rs", 903, 803)],
        })
        .expect("closed adjustment reconciles current state");
}

#[test]
fn policy_comparison_rejects_ceiling_inflation_component_moves_and_reactivation() {
    let previous = hotspot_manifest(HotspotKind::Production, HotspotStatus::Resolved, &["src/hot.rs"], 900, 800);

    let mut inflated = hotspot_manifest(HotspotKind::Production, HotspotStatus::Resolved, &["src/hot.rs"], 901, 800);
    inflated.hotspots[0].baseline_physical_lines = 900;
    assert!(inflated.compare_policy(&previous).is_err());

    let mut reactivated = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot.rs"], 900, 800);
    assert!(reactivated.compare_policy(&previous).is_err());

    reactivated.hotspots[0].status = HotspotStatus::Resolved;
    reactivated.components.push(LogicalComponent {
        id: "other".to_owned(),
        baseline_paths: vec!["src/other.rs".to_owned()],
        paths: vec!["src/hot.rs".to_owned()],
        baseline_production_lines: 0,
        production_ceiling: 0,
    });
    reactivated.components[0].paths.clear();
    assert!(reactivated.compare_policy(&previous).is_err());
}

#[test]
fn policy_comparison_rejects_path_and_successor_changes_until_transfer_governance() {
    let previous = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot.rs"], 900, 800);
    let mut changed = hotspot_manifest(HotspotKind::Production, HotspotStatus::Active, &["src/hot/first.rs", "src/hot/second.rs"], 900, 800);
    changed.hotspots[0].baseline_path = "src/hot.rs".to_owned();
    assert!(changed.compare_policy(&previous).unwrap_err().to_string().contains("path ownership"));
}

#[test]
fn policy_comparison_rejects_existing_path_moves_between_components() {
    let mut previous = ordinary_manifest("src/first.rs", 10, 10);
    previous.components.push(LogicalComponent {
        id: "other".to_owned(),
        baseline_paths: vec!["src/second.rs".to_owned()],
        paths: vec!["src/second.rs".to_owned()],
        baseline_production_lines: 20,
        production_ceiling: 20,
    });
    let mut current = ordinary_manifest("src/first.rs", 10, 10);
    current.components[0].paths = vec!["src/second.rs".to_owned()];
    current.components.push(LogicalComponent {
        id: "other".to_owned(),
        baseline_paths: vec!["src/second.rs".to_owned()],
        paths: vec!["src/first.rs".to_owned()],
        baseline_production_lines: 20,
        production_ceiling: 20,
    });
    assert!(current.compare_policy(&previous).unwrap_err().to_string().contains("path ownership"));
}

#[test]
fn previous_revision_selection_handles_absent_null_and_initial_policy_inputs() {
    let manifest = ordinary_manifest("src/lib.rs", 10, 10);
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::write(repository.path().join("README.md"), "fixture\n").expect("fixture file");
    git(repository.path(), &["init", "-q"]);
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
        &["-c", "user.name=LocalHold", "-c", "user.email=localhold@example.invalid", "commit", "-q", "-m", "fixture"],
    );
    let revision = String::from_utf8(git_output(repository.path(), &["rev-parse", "HEAD"]))
        .expect("UTF-8 revision")
        .trim()
        .to_owned();

    manifest
        .compare_previous_revision_from(repository.path(), None)
        .expect("unset revision is intentionally absent");
    manifest
        .compare_previous_revision_from(repository.path(), Some(""))
        .expect("empty revision is intentionally absent");
    manifest
        .compare_previous_revision_from(repository.path(), Some("0000000000000000000000000000000000000000"))
        .expect("null revision is intentionally absent");
    manifest
        .compare_previous_revision_from(repository.path(), Some(&revision))
        .expect("existing commit without a policy is the initial policy revision");
    assert!(
        manifest
            .compare_previous_revision_from(repository.path(), Some("ffffffffffffffffffffffffffffffffffffffff"))
            .unwrap_err()
            .to_string()
            .contains("is not available")
    );
}

fn git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git").current_dir(repository).args(arguments).status().expect("run git fixture command");
    assert!(status.success(), "git fixture command failed: {arguments:?}");
}

fn git_output(repository: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git").current_dir(repository).args(arguments).output().expect("run git fixture query");
    assert!(output.status.success(), "git fixture query failed: {arguments:?}");
    output.stdout
}
