use crate::structure::classify::{FileMeasurement, Inventory};
use crate::structure::manifest::model::{
    ComponentTransfer, FeatureFreezeStatus, FileException, FileExceptionKind, FileExceptionStatus, Hotspot, HotspotKind, HotspotStatus, Limits, LogicalComponent, PathEvolution,
    PathEvolutionKind, PreGateAdjustment, SplitAllowance, SplitAllowanceStatus, StructureManifest,
};
use crate::structure::syntax::{ConcreteStoreCounts, ConcreteStoreSites};

pub(super) fn file(path: &str, physical: usize, production: usize) -> FileMeasurement {
    let test_lines = physical.checked_sub(production).expect("fixture production lines must not exceed physical lines");
    FileMeasurement {
        path: path.to_owned(),
        physical_lines: physical,
        production_lines: production,
        test_lines,
        production_module: Vec::new(),
        production_internal_imports: Vec::new(),
        production_public_reexports: Vec::new(),
        production_concrete_stores: ConcreteStoreCounts::default(),
        production_public_concrete_store_structs: ConcreteStoreSites::default(),
        production_concrete_store_sites: ConcreteStoreSites::default(),
        production_generic_default_store_sites: ConcreteStoreSites::default(),
        production_signature_store_sites: ConcreteStoreSites::default(),
        production_store_binding_sites: ConcreteStoreSites::default(),
    }
}

pub(super) type FileSpec<'a> = (&'a str, usize, usize);

pub(super) fn inventory(files: &[FileSpec<'_>]) -> Inventory {
    Inventory {
        files: files.iter().map(|(path, physical, production)| file(path, *physical, *production)).collect(),
    }
}

pub(super) fn ordinary_manifest(path: &str, baseline_production: usize, current_production: usize) -> StructureManifest {
    let mut manifest = base_manifest();
    manifest.components.push(LogicalComponent {
        id: "component".to_owned(),
        baseline_paths: vec![path.to_owned()],
        paths: vec![path.to_owned()],
        baseline_production_lines: baseline_production,
        production_ceiling: current_production,
    });
    manifest
}

pub(super) fn hotspot_manifest(kind: HotspotKind, status: HotspotStatus, successors: &[&str], physical: usize, production: usize) -> StructureManifest {
    let paths = successors.iter().map(|path| (*path).to_owned()).collect();
    let mut manifest = base_manifest();
    manifest.components.push(LogicalComponent {
        id: "component".to_owned(),
        baseline_paths: vec!["src/hot.rs".to_owned()],
        paths,
        baseline_production_lines: production,
        production_ceiling: production,
    });
    manifest.hotspots.push(Hotspot {
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
    });
    manifest
}

fn base_manifest() -> StructureManifest {
    StructureManifest {
        schema_version: 4,
        program_phase: 0,
        feature_freeze: FeatureFreezeStatus::Active,
        baseline_commit: "a".repeat(40),
        tracked_roots: vec!["src".to_owned(), "tests".to_owned(), "benches".to_owned()],
        limits: Limits {
            production_file_physical_lines: 800,
            test_file_physical_lines: 1_000,
        },
        pre_gate_adjustments: Vec::new(),
        components: Vec::new(),
        hotspots: Vec::new(),
        path_evolutions: Vec::new(),
        component_transfers: Vec::new(),
        file_exceptions: Vec::new(),
        split_allowances: Vec::new(),
    }
}

pub(super) fn file_exception(id: &str, path: &str, kind: FileExceptionKind, ceiling: usize) -> FileException {
    let (fixture_name, removal_phase) = if kind == FileExceptionKind::HistoricalFixtureMatrix {
        (Some("published database upgrade matrix".to_owned()), Some(1))
    } else {
        (None, None)
    };
    FileException {
        id: id.to_owned(),
        path: path.to_owned(),
        kind,
        status: FileExceptionStatus::Active,
        approved_physical_ceiling: ceiling,
        current_physical_ceiling: ceiling,
        owner: "maintainers".to_owned(),
        issue: "https://example.invalid/issues/1".to_owned(),
        pull_request: "https://example.invalid/pulls/1".to_owned(),
        rationale: "reviewed cohesive file boundary".to_owned(),
        fixture_name,
        removal_phase,
    }
}

pub(super) fn component(id: &str, baseline_path: &str, path: &str, production: usize) -> LogicalComponent {
    LogicalComponent {
        id: id.to_owned(),
        baseline_paths: vec![baseline_path.to_owned()],
        paths: vec![path.to_owned()],
        baseline_production_lines: production,
        production_ceiling: production,
    }
}

pub(super) fn evolution(id: &str, kind: PathEvolutionKind, sources: &[&str], successors: &[&str]) -> PathEvolution {
    PathEvolution {
        id: id.to_owned(),
        kind,
        sources: sources.iter().map(|path| (*path).to_owned()).collect(),
        successors: successors.iter().map(|path| (*path).to_owned()).collect(),
        issue: "https://example.invalid/issues/1".to_owned(),
        pull_request: "https://example.invalid/pulls/1".to_owned(),
        rationale: "governed structural evolution".to_owned(),
    }
}

pub(super) fn transfer(id: &str, route: (&str, &str), production_lines: usize, paths: &[&str], path_evolution: &str) -> ComponentTransfer {
    let (source, destination) = route;
    ComponentTransfer {
        id: id.to_owned(),
        source_component: source.to_owned(),
        destination_component: destination.to_owned(),
        production_lines,
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        path_evolution: path_evolution.to_owned(),
        issue: "https://example.invalid/issues/1".to_owned(),
        pull_request: "https://example.invalid/pulls/1".to_owned(),
        rationale: "exact measured production transfer".to_owned(),
    }
}

pub(super) fn adjustment() -> PreGateAdjustment {
    PreGateAdjustment {
        id: "phase0.reviewed-adjustment".to_owned(),
        component: "component".to_owned(),
        hotspot: "component.hot".to_owned(),
        physical_lines: 3,
        production_lines: 3,
        issue: "issue".to_owned(),
        pull_request: "pull request".to_owned(),
        rationale: "reviewed before the gate existed".to_owned(),
    }
}

pub(super) fn split_allowance(hotspot: &str, path_evolution: &str, physical: usize, production: usize) -> SplitAllowance {
    SplitAllowance {
        id: format!("{hotspot}.split-overhead"),
        hotspot: hotspot.to_owned(),
        path_evolution: path_evolution.to_owned(),
        status: SplitAllowanceStatus::Active,
        approved_physical_lines: physical,
        approved_production_lines: production,
        current_physical_lines: physical,
        current_production_lines: production,
        owner: "maintainers".to_owned(),
        recovery_issue: "https://example.invalid/issues/recover-split-overhead".to_owned(),
        pull_request: "https://example.invalid/pulls/1".to_owned(),
        rationale: "temporary mechanical overhead from the hotspot's first structural split".to_owned(),
        due_phase: 1,
    }
}
