use super::*;
use crate::structure::classify::{FileMeasurement, Inventory};
use crate::structure::syntax::{ConcreteStoreCounts, ConcreteStoreSites};

type CountFixture<'a> = (&'a str, usize, usize);

#[test]
fn exact_recovery_debt_passes_but_new_production_names_fail() {
    let policy = policy();
    let components = components(&[
        ("src/server/mod.rs", "protocol"),
        ("src/embedding/status.rs", "embedding"),
        ("src/engine.rs", "engine-application"),
    ]);
    let exact = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0), ("src/engine.rs", 0, 0)]);
    policy.compare_current(&exact, &components).expect("exact recovery debt");

    let growth = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0), ("src/engine.rs", 1, 0)]);
    let error = policy.compare_current(&growth, &components).unwrap_err();
    assert!(error.to_string().contains("src/engine.rs"));
}

#[test]
fn baseline_and_current_counts_are_independent_ratchets() {
    let mut policy = policy();
    policy.debt[1].current_count = 1;
    let components = components(&[("src/server/mod.rs", "protocol"), ("src/embedding/status.rs", "embedding")]);

    policy
        .compare_baseline(&inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]), &components)
        .expect("immutable baseline counts");
    policy
        .compare_current(&inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 1, 0)]), &components)
        .expect("ratcheted current counts");
    assert!(
        policy
            .compare_current(&inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]), &components)
            .unwrap_err()
            .to_string()
            .contains("production-name mismatch")
    );
}

#[test]
fn reviewed_site_fingerprints_prevent_same_count_moves_and_new_generic_defaults() {
    let policy = policy();
    let restricted_components = components(&[("src/server/mod.rs", "protocol"), ("src/embedding/status.rs", "embedding")]);
    let mut baseline = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]);
    baseline.files[0].production_concrete_store_sites.sqlite_store = vec!["server-default".to_owned()];
    baseline.files[0].production_generic_default_store_sites.sqlite_store = vec!["server-default".to_owned()];
    baseline.files[1].production_concrete_store_sites.sqlite_store = vec!["embedding-import".to_owned(), "embedding-call".to_owned()];

    let exact = baseline.clone();
    policy
        .compare_site_fingerprints(&exact, &baseline, &restricted_components, &restricted_components)
        .expect("unchanged reviewed syntax sites");

    let mut moved = exact;
    moved.files[0].production_concrete_store_sites.sqlite_store = vec!["server-field".to_owned()];
    assert!(
        policy
            .compare_site_fingerprints(&moved, &baseline, &restricted_components, &restricted_components)
            .unwrap_err()
            .to_string()
            .contains("moved or changed")
    );

    let mut duplicated = baseline.clone();
    duplicated.files[1].production_concrete_store_sites.sqlite_store.push("embedding-import".to_owned());
    assert!(
        policy
            .compare_site_fingerprints(&duplicated, &baseline, &restricted_components, &restricted_components)
            .unwrap_err()
            .to_string()
            .contains("moved or changed")
    );

    let transferred_components = components(&[("src/server/mod.rs", "sqlite-store"), ("src/embedding/status.rs", "embedding")]);
    let mut moved_after_transfer = baseline.clone();
    moved_after_transfer.files[0].production_concrete_store_sites.sqlite_store = vec!["server-field".to_owned()];
    assert!(
        policy
            .compare_site_fingerprints(&moved_after_transfer, &baseline, &transferred_components, &restricted_components,)
            .unwrap_err()
            .to_string()
            .contains("moved or changed")
    );

    let unrestricted_components = components(&[("src/store/sqlite.rs", "sqlite-store")]);
    let unrestricted_baseline = inventory(&[("src/store/sqlite.rs", 1, 0)]);
    let mut new_default = unrestricted_baseline.clone();
    new_default.files[0].production_generic_default_store_sites.sqlite_store = vec!["hidden-default".to_owned()];
    assert!(
        policy
            .compare_site_fingerprints(&new_default, &unrestricted_baseline, &unrestricted_components, &unrestricted_components,)
            .unwrap_err()
            .to_string()
            .contains("generic default")
    );
}

#[test]
fn active_debt_remains_governed_after_a_same_path_component_transfer() {
    let mut policy = policy();
    policy.debt[0].current_count = 0;
    let transferred_components = components(&[("src/server/mod.rs", "sqlite-store"), ("src/embedding/status.rs", "embedding")]);
    let current = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]);
    assert!(
        policy
            .compare_current(&current, &transferred_components)
            .unwrap_err()
            .to_string()
            .contains("production-name mismatch")
    );
}

#[test]
fn persistence_ui_and_permanent_composition_components_are_unrestricted() {
    let policy = policy();
    let components = components(&[
        ("src/store/sqlite.rs", "sqlite-store"),
        ("src/store/postgres.rs", "postgres-store"),
        ("src/store/query.rs", "persistence-core"),
        ("src/store/migration.rs", "migration-schema"),
        ("src/store/context_store.rs", "context-governance"),
        ("src/main.rs", "composition"),
        ("src/doctor.rs", "doctor"),
        ("src/ui/mod.rs", "ui"),
        ("src/server/mod.rs", "protocol"),
        ("src/embedding/status.rs", "embedding"),
    ]);
    let observed = inventory(&[
        ("src/store/sqlite.rs", 20, 0),
        ("src/store/postgres.rs", 0, 20),
        ("src/store/query.rs", 10, 10),
        ("src/store/migration.rs", 10, 10),
        ("src/store/context_store.rs", 10, 10),
        ("src/main.rs", 10, 10),
        ("src/doctor.rs", 10, 10),
        ("src/ui/mod.rs", 10, 10),
        ("src/server/mod.rs", 1, 0),
        ("src/embedding/status.rs", 2, 0),
    ]);
    policy.compare_current(&observed, &components).expect("reviewed composition boundaries");
}

#[test]
fn zero_debt_prevents_resurrection_and_test_only_files_contribute_zero() {
    let mut policy = policy();
    policy.debt[0].current_count = 0;
    let current_components = components(&[("src/server/mod.rs", "protocol"), ("src/embedding/status.rs", "embedding")]);
    policy
        .compare_current(&inventory(&[("src/server/mod.rs", 0, 0), ("src/embedding/status.rs", 2, 0)]), &current_components)
        .expect("retired site remains absent");
    assert!(
        policy
            .compare_current(&inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]), &current_components,)
            .unwrap_err()
            .to_string()
            .contains("production-name mismatch")
    );

    let test_only = file("tests/concrete.rs", 0, 0);
    let test_components = components(&[
        ("src/server/mod.rs", "protocol"),
        ("src/embedding/status.rs", "embedding"),
        ("tests/concrete.rs", "integration-tests"),
    ]);
    let mut files = inventory(&[("src/server/mod.rs", 0, 0), ("src/embedding/status.rs", 2, 0)]).files;
    files.push(test_only);
    policy
        .compare_current(&Inventory { files }, &test_components)
        .expect("test-only concrete-store names are absent from production counts");
}

#[test]
fn policy_evolution_allows_only_downward_current_counts() {
    let previous = policy();
    let mut reduced = previous.clone();
    reduced.debt[1].current_count = 1;
    reduced.compare_policy(&previous).expect("downward count ratchet");

    let mut resurrected = reduced.clone();
    resurrected.debt[1].current_count = 2;
    assert!(resurrected.compare_policy(&reduced).unwrap_err().to_string().contains("cannot increase or resurrect"));

    let mut rewritten = previous.clone();
    rewritten.debt[0].rationale = "rewritten".to_owned();
    assert!(rewritten.compare_policy(&previous).unwrap_err().to_string().contains("evidence is immutable"));

    let mut added = previous.clone();
    added
        .debt
        .push(debt("phase0.new-debt", "engine-application", "src/engine.rs", ConcreteStoreName::SqliteStore, 1));
    assert!(added.compare_policy(&previous).unwrap_err().to_string().contains("new concrete-store debt is prohibited"));
}

#[test]
fn policy_validation_closes_capability_and_evidence_escapes() {
    let policy = policy();
    policy.validate().expect("valid policy");
    policy.require_baseline_commit("b05f7a43345b39d40b456fb9ed46d479c4bf26e0").expect("matching baseline");

    let mut broadened = policy.clone();
    broadened.unrestricted_components.push("engine-application".to_owned());
    assert!(broadened.validate().unwrap_err().to_string().contains("exact reviewed"));

    let mut duplicate = policy.clone();
    duplicate.debt.push(duplicate.debt[0].clone());
    assert!(duplicate.validate().unwrap_err().to_string().contains("duplicate concrete-store debt ID"));

    let mut zero_baseline = policy.clone();
    zero_baseline.debt[0].baseline_count = 0;
    assert!(zero_baseline.validate().unwrap_err().to_string().contains("baseline count must be positive"));

    let mut growth = policy.clone();
    growth.debt[0].current_count = 2;
    assert!(growth.validate().unwrap_err().to_string().contains("exceeds its recovery baseline"));

    let mut initial_reduction = policy;
    initial_reduction.debt[0].current_count = 0;
    assert!(
        initial_reduction
            .validate_initial_policy()
            .unwrap_err()
            .to_string()
            .contains("must equal recovery-baseline")
    );
}

#[test]
fn every_observed_path_requires_a_logical_component() {
    let policy = policy();
    let error = policy.compare_current(&inventory(&[("src/server/mod.rs", 1, 0)]), &BTreeMap::new()).unwrap_err();
    assert!(error.to_string().contains("has no logical component"));
}

fn policy() -> ConcreteStorePolicy {
    ConcreteStorePolicy {
        schema_version: CURRENT_SCHEMA_VERSION,
        baseline_commit: "b05f7a43345b39d40b456fb9ed46d479c4bf26e0".to_owned(),
        unrestricted_components: UNRESTRICTED_COMPONENTS.into_iter().map(str::to_owned).collect(),
        debt: vec![
            debt("phase0.protocol-default-sqlite-store", "protocol", "src/server/mod.rs", ConcreteStoreName::SqliteStore, 1),
            debt(
                "phase0.embedding-status-sqlite-store",
                "embedding",
                "src/embedding/status.rs",
                ConcreteStoreName::SqliteStore,
                2,
            ),
        ],
    }
}

fn debt(id: &str, component: &str, path: &str, store: ConcreteStoreName, count: usize) -> ConcreteStoreDebt {
    ConcreteStoreDebt {
        id: id.to_owned(),
        component: component.to_owned(),
        path: path.to_owned(),
        store,
        baseline_count: count,
        current_count: count,
        owner: "maintainers".to_owned(),
        issue: "https://github.com/gearboxlogic/localhold/issues/124".to_owned(),
        rationale: "Recovery-baseline coupling must not grow".to_owned(),
        resolution_phase: "Phase 4 or Phase 5 boundary restoration".to_owned(),
    }
}

fn inventory(files: &[CountFixture<'_>]) -> Inventory {
    Inventory {
        files: files.iter().map(|(path, sqlite, postgres)| file(path, *sqlite, *postgres)).collect(),
    }
}

fn file(path: &str, sqlite_store: usize, postgres_store: usize) -> FileMeasurement {
    FileMeasurement {
        path: path.to_owned(),
        physical_lines: 1,
        production_lines: 1,
        test_lines: 0,
        production_internal_imports: Vec::new(),
        production_concrete_stores: ConcreteStoreCounts { sqlite_store, postgres_store },
        production_concrete_store_sites: ConcreteStoreSites::default(),
        production_generic_default_store_sites: ConcreteStoreSites::default(),
    }
}

fn components(entries: &[(&'static str, &'static str)]) -> BTreeMap<&'static str, &'static str> {
    entries.iter().copied().collect()
}
