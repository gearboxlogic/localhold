use super::*;
use crate::structure::classify::{FileMeasurement, Inventory};
use crate::structure::syntax::{
    ConcreteStoreCounts, ConcreteStoreSignatureSite, ConcreteStoreSignatureSites, ConcreteStoreSites, ProductionCfgContext, PublicReexportEvidence, TypeDeclarationEvidence,
    production_cfg_context,
};

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
    policy.compare_current(&exact, paths(&components)).expect("exact recovery debt");

    let growth = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0), ("src/engine.rs", 1, 0)]);
    let error = policy.compare_current(&growth, paths(&components)).unwrap_err();
    assert!(error.to_string().contains("src/engine.rs"));
}

#[test]
fn canonical_backend_declarations_cannot_be_renamed_behind_compatibility_exports() {
    let mut policy = policy();
    let components = components(&[("src/store/sqlite.rs", "sqlite-store"), ("src/store/postgres.rs", "postgres-store")]);
    let mut exact = inventory(&[("src/store/sqlite.rs", 0, 0), ("src/store/postgres.rs", 0, 0)]);
    exact.files[0].production_public_concrete_store_structs.sqlite_store = vec![declaration_fingerprint(ConcreteStoreName::SqliteStore)];
    exact.files[1].production_public_concrete_store_structs.postgres_store = vec![declaration_fingerprint(ConcreteStoreName::PostgresStore)];
    exact.files[0].production_store_binding_sites.sqlite_store = vec!["sqlite-declaration".to_owned(), "sqlite-implementation".to_owned()];
    exact.files[1].production_store_binding_sites.postgres_store = vec!["postgres-declaration".to_owned(), "postgres-implementation".to_owned()];
    adopt_binding_fingerprints(&mut policy, &exact, paths(&components));
    policy
        .compare_canonical_declarations("current", &exact, paths(&components))
        .expect("canonical public structs remain declared");

    let mut orphaned = exact.clone();
    orphaned.files[0].production_store_binding_sites.sqlite_store = vec!["sqlite-declaration".to_owned()];
    let error = policy.compare_canonical_declarations("current", &orphaned, paths(&components)).unwrap_err();
    assert!(error.to_string().contains("canonical declaration mismatch"));

    exact.files[0].production_public_concrete_store_structs.sqlite_store = vec!["f".repeat(64)];
    let error = policy.compare_canonical_declarations("current", &exact, paths(&components)).unwrap_err();
    assert!(error.to_string().contains("canonical declaration mismatch"));
}

#[test]
fn canonical_declarations_cannot_move_between_split_successors() {
    let mut policy = policy();
    let components = components(&[("src/store/sqlite/backend.rs", "sqlite-store"), ("src/store/postgres.rs", "postgres-store")]);
    let mut current = inventory(&[("src/store/sqlite/backend.rs", 0, 0), ("src/store/postgres.rs", 0, 0)]);
    current.files[0].production_public_concrete_store_structs.sqlite_store = vec![declaration_fingerprint(ConcreteStoreName::SqliteStore)];
    current.files[1].production_public_concrete_store_structs.postgres_store = vec![declaration_fingerprint(ConcreteStoreName::PostgresStore)];
    current.files[0].production_store_binding_sites.sqlite_store = vec!["sqlite-binding".to_owned()];
    current.files[1].production_store_binding_sites.postgres_store = vec!["postgres-binding".to_owned()];
    let canonical = BTreeMap::from([("src/store/sqlite/backend.rs".to_owned(), "src/store/sqlite.rs".to_owned())]);
    let retained_site = canonical.clone();
    let retained_paths = PathAttribution::with_lineage(&components, &canonical, &retained_site);
    adopt_binding_fingerprints(&mut policy, &current, retained_paths);
    policy
        .compare_canonical_declarations("current", &current, retained_paths)
        .expect("retained split successor keeps the reviewed declaration identity");

    let sibling_site = BTreeMap::from([("src/store/sqlite/backend.rs".to_owned(), "src/store/sqlite/backend.rs".to_owned())]);
    let error = policy
        .compare_canonical_declarations("current", &current, PathAttribution::with_lineage(&components, &canonical, &sibling_site))
        .unwrap_err();
    assert!(error.to_string().contains("canonical declaration mismatch"));
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
        .compare_current(&inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 1, 0)]), paths(&components))
        .expect("ratcheted current counts");
    assert!(
        policy
            .compare_current(&inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]), paths(&components))
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
        .compare_site_fingerprints(&exact, &baseline, paths(&restricted_components), paths(&restricted_components))
        .expect("unchanged reviewed syntax sites");

    let mut moved = exact;
    moved.files[0].production_concrete_store_sites.sqlite_store = vec!["server-field".to_owned()];
    assert!(
        policy
            .compare_site_fingerprints(&moved, &baseline, paths(&restricted_components), paths(&restricted_components))
            .unwrap_err()
            .to_string()
            .contains("moved or changed")
    );

    let mut duplicated = baseline.clone();
    duplicated.files[1].production_concrete_store_sites.sqlite_store.push("embedding-import".to_owned());
    assert!(
        policy
            .compare_site_fingerprints(&duplicated, &baseline, paths(&restricted_components), paths(&restricted_components),)
            .unwrap_err()
            .to_string()
            .contains("moved or changed")
    );

    let transferred_components = components(&[("src/server/mod.rs", "sqlite-store"), ("src/embedding/status.rs", "embedding")]);
    policy
        .compare_site_fingerprints(&baseline, &baseline, paths(&transferred_components), paths(&restricted_components))
        .expect("debt generic default retains its reviewed component attribution");
    let mut moved_after_transfer = baseline.clone();
    moved_after_transfer.files[0].production_concrete_store_sites.sqlite_store = vec!["server-field".to_owned()];
    assert!(
        policy
            .compare_site_fingerprints(&moved_after_transfer, &baseline, paths(&transferred_components), paths(&restricted_components),)
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
            .compare_site_fingerprints(&new_default, &unrestricted_baseline, paths(&unrestricted_components), paths(&unrestricted_components),)
            .unwrap_err()
            .to_string()
            .contains("generic default")
    );

    let mut new_signature = unrestricted_baseline.clone();
    new_signature.files[0].production_signature_store_sites.sqlite_store = vec![signature("exported-open", &[])];
    assert!(
        policy
            .compare_site_fingerprints(&new_signature, &unrestricted_baseline, paths(&unrestricted_components), paths(&unrestricted_components),)
            .unwrap_err()
            .to_string()
            .contains("production signature")
    );
}

#[test]
fn public_reexports_are_additive_signature_evidence() {
    let policy = policy();
    let components = components(&[("src/lib.rs", "composition"), ("src/store/sqlite.rs", "sqlite-store")]);
    let mut private_signature = inventory(&[("src/lib.rs", 0, 0), ("src/store/sqlite.rs", 1, 0)]);
    private_signature.files[1].production_module = vec!["store".to_owned(), "sqlite".to_owned()];
    private_signature.files[1].production_signature_store_sites.sqlite_store = vec![signature("private-open", &["store", "sqlite", "open"])];
    let mut reexported_signature = private_signature.clone();
    reexported_signature.files[0].production_public_reexports = vec![PublicReexportEvidence {
        exported_path: vec!["open".to_owned()],
        target_path: vec!["store".to_owned(), "sqlite".to_owned(), "open".to_owned()],
        fingerprint: "public-use-helper-open".to_owned(),
        cfg: ProductionCfgContext::default(),
    }];

    let error = policy
        .compare_site_fingerprints(&reexported_signature, &private_signature, paths(&components), paths(&components))
        .unwrap_err();
    assert!(error.to_string().contains("production signature"));
    policy
        .compare_site_fingerprints(&private_signature, &reexported_signature, paths(&components), paths(&components))
        .expect("removing a public re-export reduces concrete-store exposure");
}

#[test]
fn public_reexports_must_share_a_cfg_with_the_concrete_signature() {
    let policy = policy();
    let components = components(&[("src/lib.rs", "composition"), ("src/store/sqlite.rs", "sqlite-store")]);
    let mut baseline = inventory(&[("src/lib.rs", 0, 0), ("src/store/sqlite.rs", 1, 0)]);
    baseline.files[1].production_module = vec!["store".to_owned(), "sqlite".to_owned()];
    let mut gated_signature = signature("private-open", &["store", "sqlite", "open"]);
    gated_signature.cfg = cfg_context("feature = \"legacy\"");
    baseline.files[1].production_signature_store_sites.sqlite_store = vec![gated_signature];
    let mut current = baseline.clone();
    current.files[0].production_public_reexports = vec![PublicReexportEvidence {
        exported_path: vec!["open".to_owned()],
        target_path: vec!["store".to_owned(), "sqlite".to_owned(), "open".to_owned()],
        fingerprint: "public-use-helper-open".to_owned(),
        cfg: cfg_context("not(feature = \"legacy\")"),
    }];

    policy
        .compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components))
        .expect("a mutually exclusive re-export cannot expose the concrete signature");

    current.files[0].production_public_reexports[0].cfg = cfg_context("feature = \"legacy\"");
    let error = policy.compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components)).unwrap_err();
    assert!(error.to_string().contains("production signature"));
}

#[test]
fn public_globs_are_evidence_for_canonical_store_declarations() {
    let policy = policy();
    let components = components(&[("src/lib.rs", "composition"), ("src/store/sqlite.rs", "sqlite-store")]);
    let mut baseline = inventory(&[("src/lib.rs", 0, 0), ("src/store/sqlite.rs", 1, 0)]);
    baseline.files[1].production_signature_store_sites.sqlite_store = vec![signature("canonical-sqlite-declaration", &["store", "sqlite", "SqliteStore"])];
    let mut current = baseline.clone();
    current.files[0].production_public_reexports = vec![PublicReexportEvidence {
        exported_path: vec!["*".to_owned()],
        target_path: vec!["store".to_owned(), "*".to_owned()],
        fingerprint: "public-use-store-glob".to_owned(),
        cfg: ProductionCfgContext::default(),
    }];

    let error = policy.compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components)).unwrap_err();
    assert!(error.to_string().contains("production signature"));
}

#[test]
fn inline_module_reexports_match_the_concrete_bearing_item() {
    let policy = policy();
    let components = components(&[("src/ui/mod.rs", "sqlite-store")]);
    let mut baseline = inventory(&[("src/ui/mod.rs", 1, 0)]);
    baseline.files[0].production_module = vec!["ui".to_owned()];
    baseline.files[0].production_signature_store_sites.sqlite_store = vec![signature("private-open", &["ui", "helper", "open"])];
    let mut exposed = baseline.clone();
    exposed.files[0].production_public_reexports = vec![PublicReexportEvidence {
        exported_path: vec!["ui".to_owned(), "open".to_owned()],
        target_path: vec!["ui".to_owned(), "helper".to_owned(), "open".to_owned()],
        fingerprint: "public-use-helper-open".to_owned(),
        cfg: ProductionCfgContext::default(),
    }];

    let error = policy.compare_site_fingerprints(&exposed, &baseline, paths(&components), paths(&components)).unwrap_err();
    assert!(error.to_string().contains("production signature"));

    exposed.files[0].production_public_reexports[0].target_path = vec!["ui".to_owned(), "helper".to_owned(), "Envelope".to_owned(), "Sqlite".to_owned()];
    baseline.files[0].production_signature_store_sites.sqlite_store = vec![signature("private-variant", &["ui", "helper", "Envelope"])];
    exposed.files[0].production_signature_store_sites.sqlite_store = baseline.files[0].production_signature_store_sites.sqlite_store.clone();
    let error = policy.compare_site_fingerprints(&exposed, &baseline, paths(&components), paths(&components)).unwrap_err();
    assert!(error.to_string().contains("production signature"));
}

#[test]
fn unrelated_public_reexports_do_not_change_signature_evidence() {
    let policy = policy();
    let components = components(&[("src/store/sqlite.rs", "sqlite-store"), ("src/metrics.rs", "persistence-core")]);
    let mut baseline = inventory(&[("src/store/sqlite.rs", 1, 0), ("src/metrics.rs", 0, 0)]);
    baseline.files[0].production_module = vec!["store".to_owned(), "sqlite".to_owned()];
    baseline.files[0].production_signature_store_sites.sqlite_store = vec![signature("private-open", &["store", "sqlite", "open"])];
    baseline.files[1].production_module = vec!["metrics".to_owned()];
    let mut current = baseline.clone();
    current.files[1].production_public_reexports = vec![PublicReexportEvidence {
        exported_path: vec!["Counter".to_owned()],
        target_path: vec!["metrics".to_owned(), "Counter".to_owned()],
        fingerprint: "public-use-counter".to_owned(),
        cfg: ProductionCfgContext::default(),
    }];

    policy
        .compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components))
        .expect("an unrelated module re-export does not affect store signatures");

    current.files[1].production_public_reexports = vec![PublicReexportEvidence {
        exported_path: vec!["unrelated".to_owned()],
        target_path: vec!["store".to_owned(), "sqlite".to_owned(), "unrelated".to_owned()],
        fingerprint: "public-use-same-module-unrelated".to_owned(),
        cfg: ProductionCfgContext::default(),
    }];
    policy
        .compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components))
        .expect("an unrelated item re-export from the signature module does not affect store signatures");
}

#[test]
fn public_reexports_match_signatures_only_within_the_same_target() {
    let policy = policy();
    let components = components(&[("src/lib.rs", "sqlite-store"), ("src/main.rs", "composition")]);
    let mut baseline = inventory(&[("src/lib.rs", 1, 0), ("src/main.rs", 0, 0)]);
    baseline.files[0].production_targets = vec!["src/lib.rs".to_owned()];
    baseline.files[0].production_signature_store_sites.sqlite_store = vec![signature("private-adapter-method", &["hidden", "Adapter"])];
    baseline.files[1].production_targets = vec!["src/main.rs".to_owned()];
    let mut current = baseline.clone();
    current.files[1].production_public_reexports = vec![PublicReexportEvidence {
        exported_path: vec!["Adapter".to_owned()],
        target_path: vec!["hidden".to_owned(), "Adapter".to_owned()],
        fingerprint: "binary-adapter-reexport".to_owned(),
        cfg: ProductionCfgContext::default(),
    }];

    policy
        .compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components))
        .expect("a binary re-export cannot expose a library item with the same module path");

    current.files[1].production_targets = vec!["src/lib.rs".to_owned()];
    let error = policy.compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components)).unwrap_err();
    assert!(error.to_string().contains("production signature"));
}

#[test]
fn impl_self_type_visibility_is_signature_evidence() {
    let policy = policy();
    let components = components(&[("src/adapter.rs", "sqlite-store"), ("src/hidden.rs", "sqlite-store")]);
    let mut baseline = inventory(&[("src/adapter.rs", 1, 0), ("src/hidden.rs", 0, 0)]);
    baseline.files[0].production_signature_store_sites.sqlite_store = vec![impl_signature("adapter-open", &["hidden", "Adapter"])];
    baseline.files[1].production_type_declarations = vec![type_declaration("private-adapter", &["hidden", "Adapter"])];
    let mut current = baseline.clone();
    current.files[1].production_type_declarations[0].fingerprint = "restricted-adapter".to_owned();

    let error = policy.compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components)).unwrap_err();
    assert!(error.to_string().contains("production signature"));
}

#[test]
fn impl_self_type_declarations_must_share_the_signature_target_and_cfg() {
    let policy = policy();
    let components = components(&[("src/lib.rs", "sqlite-store"), ("src/main.rs", "composition")]);
    let mut baseline = inventory(&[("src/lib.rs", 1, 0), ("src/main.rs", 0, 0)]);
    baseline.files[0].production_targets = vec!["src/lib.rs".to_owned()];
    let mut signature = impl_signature("adapter-open", &["hidden", "Adapter"]);
    signature.cfg = cfg_context("feature = \"legacy\"");
    baseline.files[0].production_signature_store_sites.sqlite_store = vec![signature];
    baseline.files[1].production_targets = vec!["src/main.rs".to_owned()];
    let mut declaration = type_declaration("private-adapter", &["hidden", "Adapter"]);
    declaration.cfg = cfg_context("not(feature = \"legacy\")");
    baseline.files[1].production_type_declarations = vec![declaration];
    let mut current = baseline.clone();
    current.files[1].production_type_declarations[0].fingerprint = "restricted-adapter".to_owned();

    policy
        .compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components))
        .expect("a declaration in another target with an exclusive cfg cannot affect the impl signature");

    baseline.files[1].production_targets = vec!["src/lib.rs".to_owned()];
    current.files[1].production_targets = vec!["src/lib.rs".to_owned()];
    baseline.files[1].production_type_declarations[0].cfg = cfg_context("feature = \"legacy\"");
    current.files[1].production_type_declarations[0].cfg = cfg_context("feature = \"legacy\"");
    let error = policy.compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components)).unwrap_err();
    assert!(error.to_string().contains("production signature"));
}

#[test]
fn transitive_public_reexports_are_signature_evidence() {
    let policy = policy();
    let components = components(&[("src/ui/mod.rs", "composition"), ("src/ui/facade.rs", "composition"), ("src/ui/helper.rs", "sqlite-store")]);
    let mut baseline = inventory(&[("src/ui/mod.rs", 0, 0), ("src/ui/facade.rs", 0, 0), ("src/ui/helper.rs", 1, 0)]);
    baseline.files[2].production_module = vec!["ui".to_owned(), "helper".to_owned()];
    baseline.files[2].production_signature_store_sites.sqlite_store = vec![signature("private-open", &["ui", "helper", "open"])];
    baseline.files[1].production_public_reexports = vec![PublicReexportEvidence {
        exported_path: vec!["ui".to_owned(), "facade".to_owned(), "open".to_owned()],
        target_path: vec!["ui".to_owned(), "helper".to_owned(), "open".to_owned()],
        fingerprint: "facade-open".to_owned(),
        cfg: ProductionCfgContext::default(),
    }];
    let mut current = baseline.clone();
    current.files[0].production_public_reexports = vec![PublicReexportEvidence {
        exported_path: vec!["ui".to_owned(), "open".to_owned()],
        target_path: vec!["ui".to_owned(), "facade".to_owned(), "open".to_owned()],
        fingerprint: "ui-open".to_owned(),
        cfg: ProductionCfgContext::default(),
    }];

    let error = policy.compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components)).unwrap_err();
    assert!(error.to_string().contains("production signature"));
}

#[test]
fn previous_revision_sites_prevent_swapping_retired_baseline_occurrences() {
    let policy = policy();
    let components = components(&[("src/server/mod.rs", "protocol"), ("src/embedding/status.rs", "embedding")]);
    let mut baseline = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]);
    baseline.files[1].production_concrete_store_sites.sqlite_store = vec!["embedding-import".to_owned(), "embedding-call".to_owned()];
    let mut previous = baseline.clone();
    previous.files[1].production_concrete_store_sites.sqlite_store = vec!["embedding-import".to_owned()];
    let mut current = baseline.clone();
    current.files[1].production_concrete_store_sites.sqlite_store = vec!["embedding-call".to_owned()];

    policy
        .compare_site_fingerprints(&current, &baseline, paths(&components), paths(&components))
        .expect("each revision remains a subset of the fixed recovery baseline");
    assert!(
        policy
            .compare_site_fingerprints_against(
                AttributedInventory {
                    inventory: &current,
                    paths: paths(&components),
                },
                AttributedInventory {
                    inventory: &previous,
                    paths: paths(&components),
                },
                "previous-revision",
            )
            .unwrap_err()
            .to_string()
            .contains("previous-revision")
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
            .compare_current(&current, paths(&transferred_components))
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
    policy.compare_current(&observed, paths(&components)).expect("reviewed composition boundaries");
}

#[test]
fn zero_debt_prevents_resurrection_and_test_only_files_contribute_zero() {
    let mut policy = policy();
    policy.debt[0].current_count = 0;
    let current_components = components(&[("src/server/mod.rs", "protocol"), ("src/embedding/status.rs", "embedding")]);
    policy
        .compare_current(&inventory(&[("src/server/mod.rs", 0, 0), ("src/embedding/status.rs", 2, 0)]), paths(&current_components))
        .expect("retired site remains absent");
    assert!(
        policy
            .compare_current(&inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]), paths(&current_components),)
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
        .compare_current(&Inventory { files }, paths(&test_components))
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

    let mut pre_fingerprint_policy = previous.clone();
    for declaration in &mut pre_fingerprint_policy.canonical_declarations {
        declaration.fingerprint.clear();
        declaration.baseline_binding_fingerprint.clear();
        declaration.current_binding_fingerprint.clear();
    }
    previous
        .compare_policy(&pre_fingerprint_policy)
        .expect("one-time canonical declaration fingerprint adoption");

    let mut replaced_declaration = previous.clone();
    replaced_declaration.canonical_declarations[0].fingerprint = "f".repeat(64);
    assert!(
        replaced_declaration
            .compare_policy(&previous)
            .unwrap_err()
            .to_string()
            .contains("canonical concrete-store declarations are immutable")
    );
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

    let mut missing_fingerprint = policy.clone();
    missing_fingerprint.canonical_declarations[0].fingerprint.clear();
    assert!(missing_fingerprint.validate().unwrap_err().to_string().contains("fingerprints must not be empty"));

    let mut missing_binding = policy.clone();
    missing_binding.canonical_declarations[0].current_binding_fingerprint.clear();
    assert!(missing_binding.validate().unwrap_err().to_string().contains("binding fingerprints must not be empty"));

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
    let empty_components = BTreeMap::new();
    let error = policy.compare_current(&inventory(&[("src/server/mod.rs", 1, 0)]), paths(&empty_components)).unwrap_err();
    assert!(error.to_string().contains("has no logical component"));
}

#[test]
fn renamed_debt_successors_keep_their_counts_and_sites_governed() {
    let policy = policy();
    let baseline_components = components(&[("src/server/mod.rs", "protocol"), ("src/embedding/status.rs", "embedding")]);
    let current_components = components(&[("src/server/mod.rs", "protocol"), ("src/embedding/renamed.rs", "sqlite-store")]);
    let canonical_paths = BTreeMap::from([("src/embedding/renamed.rs".to_owned(), "src/embedding/status.rs".to_owned())]);
    let site_paths = canonical_paths.clone();
    let mut baseline = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]);
    baseline.files[1].production_concrete_store_sites.sqlite_store = vec!["embedding-import".to_owned(), "embedding-call".to_owned()];
    let mut current = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/renamed.rs", 2, 0)]);
    current.files[1].production_concrete_store_sites.sqlite_store = baseline.files[1].production_concrete_store_sites.sqlite_store.clone();

    policy
        .compare_current(&current, PathAttribution::with_lineage(&current_components, &canonical_paths, &site_paths))
        .expect("renamed debt count remains attributed to its reviewed lineage");
    policy
        .compare_site_fingerprints(
            &current,
            &baseline,
            PathAttribution::with_lineage(&current_components, &canonical_paths, &site_paths),
            paths(&baseline_components),
        )
        .expect("renamed debt sites remain attributed to their reviewed lineage");

    current.files[1].production_concrete_store_sites.sqlite_store[0] = "moved-import".to_owned();
    assert!(
        policy
            .compare_site_fingerprints(
                &current,
                &baseline,
                PathAttribution::with_lineage(&current_components, &canonical_paths, &site_paths),
                paths(&baseline_components),
            )
            .unwrap_err()
            .to_string()
            .contains("moved or changed")
    );
}

#[test]
fn split_successors_cannot_exchange_reviewed_sites() {
    let policy = policy();
    let baseline_components = components(&[("src/server/mod.rs", "protocol"), ("src/embedding/status.rs", "embedding")]);
    let current_components = components(&[
        ("src/server/mod.rs", "protocol"),
        ("src/embedding/first.rs", "embedding"),
        ("src/embedding/second.rs", "embedding"),
    ]);
    let canonical_paths = BTreeMap::from([
        ("src/embedding/first.rs".to_owned(), "src/embedding/status.rs".to_owned()),
        ("src/embedding/second.rs".to_owned(), "src/embedding/status.rs".to_owned()),
    ]);
    let site_paths = BTreeMap::from([
        ("src/embedding/first.rs".to_owned(), "src/embedding/first.rs".to_owned()),
        ("src/embedding/second.rs".to_owned(), "src/embedding/second.rs".to_owned()),
    ]);
    let mut baseline = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]);
    baseline.files[1].production_concrete_store_sites.sqlite_store = vec!["embedding-import".to_owned(), "embedding-call".to_owned()];

    let mut split = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/first.rs", 0, 0), ("src/embedding/second.rs", 2, 0)]);
    split.files[2].production_concrete_store_sites.sqlite_store = baseline.files[1].production_concrete_store_sites.sqlite_store.clone();
    let error = policy
        .compare_site_fingerprints(
            &split,
            &baseline,
            PathAttribution::with_lineage(&current_components, &canonical_paths, &site_paths),
            paths(&baseline_components),
        )
        .unwrap_err();
    assert!(error.to_string().contains("moved or changed"));
}

#[test]
fn retained_renamed_successor_keeps_its_site_identity_through_a_later_split() {
    let policy = policy();
    let baseline_components = components(&[("src/server/mod.rs", "protocol"), ("src/embedding/status.rs", "embedding")]);
    let current_components = components(&[
        ("src/server/mod.rs", "protocol"),
        ("src/embedding/renamed.rs", "embedding"),
        ("src/embedding/extracted.rs", "embedding"),
    ]);
    let canonical_paths = BTreeMap::from([
        ("src/embedding/renamed.rs".to_owned(), "src/embedding/status.rs".to_owned()),
        ("src/embedding/extracted.rs".to_owned(), "src/embedding/status.rs".to_owned()),
    ]);
    let site_paths = BTreeMap::from([
        ("src/embedding/renamed.rs".to_owned(), "src/embedding/status.rs".to_owned()),
        ("src/embedding/extracted.rs".to_owned(), "src/embedding/extracted.rs".to_owned()),
    ]);
    let mut baseline = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/status.rs", 2, 0)]);
    baseline.files[1].production_concrete_store_sites.sqlite_store = vec!["embedding-import".to_owned(), "embedding-call".to_owned()];
    let mut current = inventory(&[("src/server/mod.rs", 1, 0), ("src/embedding/renamed.rs", 2, 0), ("src/embedding/extracted.rs", 0, 0)]);
    current.files[1].production_concrete_store_sites.sqlite_store = baseline.files[1].production_concrete_store_sites.sqlite_store.clone();

    policy
        .compare_site_fingerprints(
            &current,
            &baseline,
            PathAttribution::with_lineage(&current_components, &canonical_paths, &site_paths),
            paths(&baseline_components),
        )
        .expect("the retained renamed file keeps its original site identity");

    current.files[1].production_concrete_store_sites.sqlite_store.clear();
    current.files[2].production_concrete_store_sites.sqlite_store = baseline.files[1].production_concrete_store_sites.sqlite_store.clone();
    assert!(
        policy
            .compare_site_fingerprints(
                &current,
                &baseline,
                PathAttribution::with_lineage(&current_components, &canonical_paths, &site_paths),
                paths(&baseline_components),
            )
            .unwrap_err()
            .to_string()
            .contains("moved or changed")
    );
}

fn policy() -> ConcreteStorePolicy {
    ConcreteStorePolicy {
        schema_version: CURRENT_SCHEMA_VERSION,
        baseline_commit: "b05f7a43345b39d40b456fb9ed46d479c4bf26e0".to_owned(),
        unrestricted_components: UNRESTRICTED_COMPONENTS.into_iter().map(str::to_owned).collect(),
        canonical_declarations: vec![
            declaration("sqlite-store", "src/store/sqlite.rs", ConcreteStoreName::SqliteStore),
            declaration("postgres-store", "src/store/postgres.rs", ConcreteStoreName::PostgresStore),
        ],
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

fn declaration(component: &str, path: &str, store: ConcreteStoreName) -> ConcreteStoreDeclaration {
    ConcreteStoreDeclaration {
        component: component.to_owned(),
        path: path.to_owned(),
        store,
        fingerprint: declaration_fingerprint(store),
        baseline_binding_fingerprint: "2".repeat(64),
        current_binding_fingerprint: "2".repeat(64),
    }
}

fn adopt_binding_fingerprints(policy: &mut ConcreteStorePolicy, inventory: &Inventory, paths: PathAttribution<'_>) {
    for declaration in &mut policy.canonical_declarations {
        let binding = canonical_binding_fingerprint(inventory, paths, &declaration.component, declaration.store).expect("fixture binding fingerprint");
        declaration.baseline_binding_fingerprint.clone_from(&binding);
        declaration.current_binding_fingerprint = binding;
    }
}

fn declaration_fingerprint(store: ConcreteStoreName) -> String {
    match store {
        ConcreteStoreName::SqliteStore => "0".repeat(64),
        ConcreteStoreName::PostgresStore => "1".repeat(64),
    }
}

fn paths<'a>(component_paths: &'a BTreeMap<&'a str, &'a str>) -> PathAttribution<'a> {
    PathAttribution::identity(component_paths)
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
        production_targets: vec!["crate".to_owned()],
        production_module: Vec::new(),
        production_internal_imports: Vec::new(),
        production_public_reexports: Vec::new(),
        production_type_declarations: Vec::new(),
        production_concrete_stores: ConcreteStoreCounts { sqlite_store, postgres_store },
        production_public_concrete_store_structs: ConcreteStoreSites::default(),
        production_concrete_store_sites: ConcreteStoreSites::default(),
        production_generic_default_store_sites: ConcreteStoreSites::default(),
        production_signature_store_sites: ConcreteStoreSignatureSites::default(),
        production_store_binding_sites: ConcreteStoreSites::default(),
    }
}

fn signature(fingerprint: &str, item_path: &[&str]) -> ConcreteStoreSignatureSite {
    ConcreteStoreSignatureSite {
        fingerprint: fingerprint.to_owned(),
        item_path: item_path.iter().map(|segment| (*segment).to_owned()).collect(),
        cfg: ProductionCfgContext::default(),
        impl_self_type: false,
    }
}

fn impl_signature(fingerprint: &str, item_path: &[&str]) -> ConcreteStoreSignatureSite {
    ConcreteStoreSignatureSite {
        impl_self_type: true,
        ..signature(fingerprint, item_path)
    }
}

fn type_declaration(fingerprint: &str, item_path: &[&str]) -> TypeDeclarationEvidence {
    TypeDeclarationEvidence {
        fingerprint: fingerprint.to_owned(),
        item_path: item_path.iter().map(|segment| (*segment).to_owned()).collect(),
        cfg: ProductionCfgContext::default(),
    }
}

fn cfg_context(predicate: &str) -> ProductionCfgContext {
    let item = syn::parse_str::<syn::ItemFn>(&format!("#[cfg({predicate})] fn gated() {{}}")).expect("cfg fixture");
    production_cfg_context(&item.attrs, &ProductionCfgContext::default())
        .expect("valid cfg fixture")
        .expect("production-compatible cfg fixture")
}

fn components(entries: &[(&'static str, &'static str)]) -> BTreeMap<&'static str, &'static str> {
    entries.iter().copied().collect()
}
