use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use super::{is_conventional_binary_root, measure_sources, physical_line_count, scan_revision, scan_workspace};

mod target_features;

fn inventory(sources: &[(&str, &str)]) -> super::Inventory {
    let sources = sources.iter().map(|(path, source)| ((*path).to_owned(), (*source).to_owned())).collect::<BTreeMap<_, _>>();
    measure_sources(sources).expect("fixture inventory")
}

#[test]
fn physical_lines_are_lf_crlf_and_final_newline_stable() {
    assert_eq!(physical_line_count(""), 0);
    assert_eq!(physical_line_count("one"), 1);
    assert_eq!(physical_line_count("one\n"), 1);
    assert_eq!(physical_line_count("one\r\ntwo\r\n"), 2);
}

#[test]
fn automatic_binary_roots_exclude_nested_support_modules() {
    assert!(is_conventional_binary_root("src/main.rs"));
    assert!(is_conventional_binary_root("src/bin/worker.rs"));
    assert!(is_conventional_binary_root("src/bin/worker/main.rs"));
    assert!(!is_conventional_binary_root("src/bin.rs"));
    assert!(!is_conventional_binary_root("src/bin/worker/support.rs"));
    assert!(!is_conventional_binary_root("src/bin/worker/nested/main.rs"));
}

#[test]
fn cfg_test_and_testing_feature_items_are_not_production() {
    let inventory = inventory(&[(
        "src/lib.rs",
        "fn production() {}\n\
         #[cfg(test)]\nfn test_only() {}\n\
         #[cfg(any(test, feature = \"testing\"))]\nfn harness_only() {}\n\
         #[cfg(all(unix, test))]\nfn unix_test_only() {}\n\
         #[cfg(feature = \"other\")]\nfn optional_production() {}\n",
    )]);
    assert_eq!(inventory.files[0].physical_lines, 9);
    assert_eq!(inventory.files[0].production_lines, 3);
    assert_eq!(inventory.files[0].test_lines, 6);
}

#[test]
fn cfg_attr_is_test_only_only_when_it_cannot_exist_in_production() {
    let inventory = inventory(&[(
        "src/lib.rs",
        "#[cfg_attr(all(), cfg(test))]\nfn always_test() {}\n\
         #[cfg_attr(all(), cfg_attr(all(), cfg(test)))]\nfn nested_always_test() {}\n\
         #[cfg_attr(feature = \"other\", cfg(test))]\nfn sometimes_production() {}\n",
    )]);
    assert_eq!(inventory.files[0].test_lines, 4);
    assert_eq!(inventory.files[0].production_lines, 2);
}

#[test]
fn cfg_test_on_nested_syntax_is_classified() {
    let inventory = inventory(&[(
        "src/lib.rs",
        "enum Mode {\n    Live,\n    #[cfg(test)]\n    Test,\n}\n\
         impl Mode {\n    #[cfg(test)]\n    fn fixture() {}\n}\n\
         fn choose(value: Mode) {\n    match value {\n        Mode::Live => {}\n        #[cfg(test)]\n        Mode::Test => {}\n    }\n}\n",
    )]);
    let file = &inventory.files[0];
    assert_eq!(file.physical_lines, file.production_lines + file.test_lines);
    assert_eq!(file.test_lines, 6);
}

#[test]
fn external_modules_reachable_only_from_tests_are_wholly_test_only() {
    let inventory = inventory(&[
        ("src/lib.rs", "#[cfg(test)]\nmod tests;\nfn production() {}\n"),
        ("src/tests.rs", "mod nested;\nfn helper() {}\n"),
        ("src/tests/nested.rs", "fn nested_helper() {}\n"),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/lib.rs"].production_lines, 1);
    assert_eq!(by_path["src/tests.rs"].production_lines, 0);
    assert_eq!(by_path["src/tests/nested.rs"].production_lines, 0);
    assert!(by_path["src/tests.rs"].production_targets.is_empty());
    assert!(by_path["src/tests/nested.rs"].production_targets.is_empty());
}

#[test]
fn inner_file_test_cfg_propagates_to_external_child_modules() {
    let inventory = inventory(&[
        ("src/lib.rs", "mod support;\n"),
        ("src/support.rs", "#![cfg(test)]\nmod nested;\n"),
        ("src/support/nested.rs", "use crate::server::TestOnly;\n"),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/support.rs"].production_lines, 0);
    assert_eq!(by_path["src/support/nested.rs"].production_lines, 0);
    assert!(by_path["src/support/nested.rs"].production_internal_imports.is_empty());
}

#[test]
fn any_production_module_edge_keeps_the_target_in_production() {
    let inventory = inventory(&[
        ("src/lib.rs", "#[cfg(not(test))]\nmod shared;\n#[cfg(test)]\nmod shared;\nfn production() {}\n"),
        ("src/shared.rs", "fn shared() {}\n"),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/shared.rs"].production_lines, 1);
}

#[test]
fn production_sources_retain_their_originating_target_roots() {
    let inventory = inventory(&[("src/lib.rs", "mod shared;\n"), ("src/main.rs", "mod shared;\n"), ("src/shared.rs", "fn shared() {}\n")]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/lib.rs"].production_targets, ["src/lib.rs"]);
    assert_eq!(by_path["src/main.rs"].production_targets, ["src/main.rs"]);
    assert_eq!(by_path["src/shared.rs"].production_targets, ["src/lib.rs", "src/main.rs"]);
}

#[test]
fn production_target_memberships_exclude_cfg_impossible_paths() {
    let inventory = inventory(&[
        ("src/lib.rs", "mod shared;\n"),
        ("src/main.rs", "#![cfg(feature = \"legacy\")]\nmod shared;\nfn main() {}\n"),
        ("src/shared.rs", "#[cfg(not(feature = \"legacy\"))]\nmod child;\n"),
        ("src/shared/child.rs", "fn child() {}\n"),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/shared.rs"].production_targets, ["src/lib.rs", "src/main.rs"]);
    assert_eq!(by_path["src/shared/child.rs"].production_targets, ["src/lib.rs"]);
}

#[test]
fn binary_crate_roots_resolve_reexports_from_the_empty_module() {
    let inventory = inventory(&[
        ("src/main.rs", "mod hidden;\npub(crate) use hidden::open;\nfn main() {}\n"),
        ("src/hidden.rs", "pub(crate) fn open(_: SqliteStore) {}\n"),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    let root = by_path["src/main.rs"];
    let hidden = by_path["src/hidden.rs"];
    assert!(root.production_module.is_empty());
    assert_eq!(hidden.production_module, ["hidden"]);
    assert_eq!(root.production_public_reexports[0].target_path, ["hidden", "open"]);
    assert_eq!(hidden.production_signature_store_sites.sqlite_store[0].item_path, ["hidden", "open"]);
}

#[test]
fn concrete_store_counts_follow_production_reachability() {
    let inventory = inventory(&[
        (
            "src/lib.rs",
            "use crate::store::SqliteStore;\n\
             fn production(_: PostgresStore) {}\n\
             #[cfg(test)] fn test_only(_: SqliteStore, _: PostgresStore) {}\n\
             #[cfg(test)] mod support;\n",
        ),
        ("src/support.rs", "fn helper(_: SqliteStore, _: PostgresStore) {}\n"),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/lib.rs"].production_concrete_stores.sqlite_store, 1);
    assert_eq!(by_path["src/lib.rs"].production_concrete_stores.postgres_store, 1);
    assert_eq!(by_path["src/support.rs"].production_concrete_stores, super::ConcreteStoreCounts::default());
}

#[test]
fn out_of_line_modules_inherit_cfg_constraints_from_their_declaration_path() {
    let inventory = inventory(&[
        ("src/lib.rs", "#[cfg(feature = \"x\")]\nmod backend;\n"),
        (
            "src/backend.rs",
            "#[cfg(not(feature = \"x\"))]\n\
             fn unreachable() { SqliteStore::register_extension(); }\n\
             #[cfg(feature = \"x\")]\n\
             fn reachable() { PostgresStore::connect(); }\n",
        ),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/backend.rs"].production_lines, 2);
    assert_eq!(by_path["src/backend.rs"].test_lines, 2);
    assert_eq!(
        by_path["src/backend.rs"].production_concrete_stores,
        super::ConcreteStoreCounts {
            sqlite_store: 0,
            postgres_store: 1,
        }
    );
}

#[test]
fn out_of_line_store_signatures_inherit_module_visibility_identity() {
    let private = inventory(&[("src/lib.rs", "mod helper;\n"), ("src/helper.rs", "pub fn open() -> SqliteStore { loop {} }\n")]);
    let restricted = inventory(&[("src/lib.rs", "pub(crate) mod helper;\n"), ("src/helper.rs", "pub fn open() -> SqliteStore { loop {} }\n")]);
    let signature_sites = |inventory: &super::Inventory| {
        inventory
            .files
            .iter()
            .find(|file| file.path == "src/helper.rs")
            .expect("helper measurement")
            .production_signature_store_sites
            .clone()
    };
    assert_ne!(signature_sites(&private), signature_sites(&restricted));
}

#[test]
fn out_of_line_store_signatures_keep_cfg_visibility_pairs() {
    let public_on_feature = inventory(&[
        (
            "src/lib.rs",
            "#[cfg(feature = \"x\")]\npub mod helper;\n\
             #[cfg(not(feature = \"x\"))]\nmod helper;\n",
        ),
        ("src/helper.rs", "pub fn open() -> SqliteStore { loop {} }\n"),
    ]);
    let public_without_feature = inventory(&[
        (
            "src/lib.rs",
            "#[cfg(feature = \"x\")]\nmod helper;\n\
             #[cfg(not(feature = \"x\"))]\npub mod helper;\n",
        ),
        ("src/helper.rs", "pub fn open() -> SqliteStore { loop {} }\n"),
    ]);
    let signature_sites = |inventory: &super::Inventory| {
        inventory
            .files
            .iter()
            .find(|file| file.path == "src/helper.rs")
            .expect("helper measurement")
            .production_signature_store_sites
            .clone()
    };
    assert_ne!(signature_sites(&public_on_feature), signature_sites(&public_without_feature));
}

#[test]
fn public_reexports_are_inventoried_beside_concrete_store_signatures() {
    let private = inventory(&[("src/lib.rs", "mod helper;\n"), ("src/helper.rs", "pub fn open() -> SqliteStore { loop {} }\n")]);
    let exposed = inventory(&[
        ("src/lib.rs", "mod helper;\npub use helper::open;\n"),
        ("src/helper.rs", "pub fn open() -> SqliteStore { loop {} }\n"),
    ]);
    let private_root = private.files.iter().find(|file| file.path == "src/lib.rs").expect("private root measurement");
    let exposed_root = exposed.files.iter().find(|file| file.path == "src/lib.rs").expect("exposed root measurement");
    let exposed_helper = exposed.files.iter().find(|file| file.path == "src/helper.rs").expect("helper measurement");
    assert!(private_root.production_public_reexports.is_empty());
    assert_eq!(exposed_root.production_public_reexports.len(), 1);
    assert_eq!(exposed_root.production_public_reexports[0].exported_path, ["open"]);
    assert_eq!(exposed_root.production_public_reexports[0].target_path, ["helper", "open"]);
    assert_eq!(exposed_helper.production_signature_store_sites.sqlite_store.len(), 1);
    assert_eq!(exposed_helper.production_signature_store_sites.sqlite_store[0].item_path, ["helper", "open"]);
}

#[test]
fn public_reexports_match_concrete_store_methods_to_their_impl_type() {
    let measured = inventory(&[
        (
            "src/lib.rs",
            "mod hidden;\n\
             pub(crate) use hidden::Adapter;\n",
        ),
        (
            "src/hidden.rs",
            "pub(crate) struct Adapter;\n\
             impl InternalAdapter {\n\
                 pub(crate) fn open() -> SqliteStore { loop {} }\n\
             }\n\
             use self::Adapter as InternalAdapter;\n",
        ),
    ]);
    let root = measured.files.iter().find(|file| file.path == "src/lib.rs").expect("root measurement");
    let hidden = measured.files.iter().find(|file| file.path == "src/hidden.rs").expect("hidden measurement");
    assert_eq!(root.production_public_reexports[0].target_path, ["hidden", "Adapter"]);
    assert!(
        hidden
            .production_signature_store_sites
            .sqlite_store
            .iter()
            .any(|site| site.item_path == ["hidden", "Adapter"])
    );
}

#[test]
fn public_type_aliases_match_concrete_store_methods_to_their_target() {
    let measured = inventory(&[
        (
            "src/lib.rs",
            "mod hidden;\n\
             pub(crate) type PublicAdapter = hidden::Adapter;\n",
        ),
        (
            "src/hidden.rs",
            "pub(crate) struct Adapter;\n\
             impl Adapter {\n\
                 pub(crate) fn open() -> SqliteStore { loop {} }\n\
             }\n",
        ),
    ]);
    let root = measured.files.iter().find(|file| file.path == "src/lib.rs").expect("root measurement");
    let hidden = measured.files.iter().find(|file| file.path == "src/hidden.rs").expect("hidden measurement");
    assert_eq!(root.production_public_reexports.len(), 1);
    assert_eq!(root.production_public_reexports[0].exported_path, ["PublicAdapter"]);
    assert_eq!(root.production_public_reexports[0].target_path, ["hidden", "Adapter"]);
    assert!(
        hidden
            .production_signature_store_sites
            .sqlite_store
            .iter()
            .any(|site| site.item_path == ["hidden", "Adapter"])
    );
}

#[test]
fn public_reexport_chains_normalize_exported_and_target_paths() {
    let measured = inventory(&[
        ("src/lib.rs", "mod ui;\n"),
        (
            "src/ui/mod.rs",
            "mod helper;\n\
             mod facade { pub use super::helper::open; }\n\
             pub(crate) use self::facade::open;\n",
        ),
        ("src/ui/helper.rs", "pub fn open() -> SqliteStore { loop {} }\n"),
    ]);
    let ui = measured.files.iter().find(|file| file.path == "src/ui/mod.rs").expect("ui measurement");
    let paths = ui
        .production_public_reexports
        .iter()
        .map(|evidence| (evidence.exported_path.clone(), evidence.target_path.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            (
                vec!["ui".to_owned(), "facade".to_owned(), "open".to_owned()],
                vec!["ui".to_owned(), "helper".to_owned(), "open".to_owned()]
            ),
            (vec!["ui".to_owned(), "open".to_owned()], vec!["ui".to_owned(), "helper".to_owned(), "open".to_owned()]),
        ]
    );
}

#[test]
fn public_reexports_resolve_private_import_aliases_regardless_of_item_order() {
    for source in [
        "mod helper;\n\
         use self::helper as facade;\n\
         pub(crate) use self::facade::open;\n",
        "mod helper;\n\
         pub(crate) use self::facade::open;\n\
         use self::helper as facade;\n",
    ] {
        let measured = inventory(&[
            ("src/lib.rs", "mod ui;\n"),
            ("src/ui/mod.rs", source),
            ("src/ui/helper.rs", "pub fn open() -> SqliteStore { loop {} }\n"),
        ]);
        let ui = measured.files.iter().find(|file| file.path == "src/ui/mod.rs").expect("ui measurement");
        assert_eq!(ui.production_public_reexports.len(), 1);
        assert_eq!(ui.production_public_reexports[0].target_path, ["ui", "helper", "open"]);
    }
}

#[test]
fn public_reexports_ignore_block_scoped_import_aliases() {
    let measured = inventory(&[
        ("src/lib.rs", "mod ui;\n"),
        (
            "src/ui/mod.rs",
            "mod facade;\n\
             mod helper;\n\
             fn local() { use self::helper as facade; }\n\
             pub(crate) use self::facade::open;\n",
        ),
        ("src/ui/facade.rs", "pub fn open() -> SqliteStore { loop {} }\n"),
        ("src/ui/helper.rs", "pub fn open() -> PostgresStore { loop {} }\n"),
    ]);
    let ui = measured.files.iter().find(|file| file.path == "src/ui/mod.rs").expect("ui measurement");
    assert_eq!(ui.production_public_reexports.len(), 1);
    assert_eq!(ui.production_public_reexports[0].target_path, ["ui", "facade", "open"]);
}

#[test]
fn multiple_module_paths_disjoin_their_inherited_cfg_constraints() {
    let inventory = inventory(&[
        (
            "src/lib.rs",
            "#[cfg(feature = \"x\")]\nmod shared;\n\
             #[cfg(not(feature = \"x\"))]\nmod shared;\n",
        ),
        (
            "src/shared.rs",
            "#[cfg(feature = \"x\")]\nfn first() { SqliteStore::register_extension(); }\n\
             #[cfg(not(feature = \"x\"))]\nfn second() { PostgresStore::connect(); }\n",
        ),
    ]);
    let file = inventory.files.iter().find(|file| file.path == "src/shared.rs").expect("shared module measurement");
    assert_eq!(
        file.production_concrete_stores,
        super::ConcreteStoreCounts {
            sqlite_store: 1,
            postgres_store: 1,
        }
    );
}

#[test]
fn mutually_exclusive_module_ancestors_only_fingerprint_applicable_paths() {
    let measurements = |legacy_visibility: &str, current_visibility: &str| {
        inventory(&[
            (
                "src/lib.rs",
                &format!(
                    "#[cfg(feature = \"legacy\")]\n{legacy_visibility}mod shared;\n\
                     #[cfg(not(feature = \"legacy\"))]\n{current_visibility}mod shared;\n"
                ),
            ),
            (
                "src/shared.rs",
                "#[cfg(not(feature = \"legacy\"))]\n\
                 pub(crate) fn open() -> SqliteStore { loop {} }\n",
            ),
        ])
    };
    let signature_sites = |inventory: &super::Inventory| {
        inventory
            .files
            .iter()
            .find(|file| file.path == "src/shared.rs")
            .expect("shared module measurement")
            .production_signature_store_sites
            .sqlite_store
            .clone()
    };

    let baseline = measurements("", "");
    let inactive_path_changed = measurements("pub(crate) ", "");
    let active_path_changed = measurements("", "pub(crate) ");
    assert_eq!(signature_sites(&baseline), signature_sites(&inactive_path_changed));
    assert_ne!(signature_sites(&baseline), signature_sites(&active_path_changed));
}

#[test]
fn contradictory_transitive_module_paths_do_not_restore_broad_reachability() {
    let inventory = inventory(&[
        ("src/lib.rs", "#[cfg(feature = \"x\")]\nmod parent;\n"),
        ("src/parent.rs", "#[cfg(not(feature = \"x\"))]\nmod child;\n"),
        ("src/parent/child.rs", "fn dead() { SqliteStore::register_extension(); }\n"),
    ]);
    let child = inventory.files.iter().find(|file| file.path == "src/parent/child.rs").expect("child module measurement");
    assert_eq!(child.production_lines, 0);
    assert_eq!(child.production_concrete_stores, super::ConcreteStoreCounts::default());
}

#[test]
fn integration_and_benchmark_roots_are_wholly_test_only() {
    let inventory = inventory(&[("benches/load.rs", "fn benchmark_helper() {}\n"), ("tests/contract.rs", "fn integration_helper() {}\n")]);
    assert_eq!(inventory.files.len(), 2);
    assert!(inventory.files.iter().all(|file| file.production_lines == 0));
}

#[test]
fn restricted_imports_are_collected_only_from_library_sources() {
    let inventory = inventory(&[
        ("src/lib.rs", "use crate::server::LibraryDependency;\n"),
        ("src/main.rs", "use crate::server::CompositionDependency;\nfn main() {}\n"),
        ("src/server/mod.rs", "macro_rules! local { () => { let server = 1; } }\n"),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/lib.rs"].production_internal_imports, ["crate::server::LibraryDependency"]);
    assert!(by_path["src/main.rs"].production_internal_imports.is_empty());
    assert!(by_path["src/server/mod.rs"].production_internal_imports.is_empty());
}

#[test]
fn production_cargo_targets_outside_src_are_rejected() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"escape\"\npath = \"tests/escape.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("tests/escape.rs"), "fn main() {}\n").expect("escaped binary");

    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("production target must remain under src/"));
}

#[test]
fn declared_test_and_bench_targets_under_src_are_test_only() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[test]]\nname = \"custom-test\"\npath = \"src/custom_test.rs\"\n\
         \n[[bench]]\nname = \"custom-bench\"\npath = \"src/custom_bench.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("src/custom_test.rs"), "fn test_target() {}\n").expect("test target");
    fs::write(repository.path().join("src/custom_bench.rs"), "fn bench_target() {}\n").expect("bench target");
    fs::write(repository.path().join("src/unreferenced.rs"), "fn production_fallback() {}\n").expect("unreferenced source");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/custom_test.rs"].production_lines, 0);
    assert_eq!(by_path["src/custom_bench.rs"].production_lines, 0);
    assert_eq!(by_path["src/unreferenced.rs"].production_lines, 1);
}

#[test]
fn auxiliary_targets_cannot_reuse_library_modules() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src/shared", "tests", "benches"] {
        fs::create_dir_all(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[test]]\nname = \"shared\"\npath = \"src/shared.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "mod shared;\n").expect("library root");
    fs::write(repository.path().join("src/shared.rs"), "mod nested;\n").expect("shared test target");
    fs::write(repository.path().join("src/shared/nested.rs"), "use crate::server::Service;\n").expect("library child");

    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("test or benchmark target must not also be reachable from a library target"));
}

#[test]
fn declared_examples_are_production_roots() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[example]]\nname = \"demo\"\npath = \"tests/demo.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("tests/demo.rs"), "fn main() {}\n").expect("example target");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["tests/demo.rs"].production_lines, 1);
}

#[test]
fn custom_production_roots_resolve_modules_from_their_parent() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[bin]]\nname = \"cli\"\npath = \"src/cli.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "#[cfg(test)]\nmod shared;\n").expect("library root");
    fs::write(repository.path().join("src/cli.rs"), "mod shared;\nfn main() {}\n").expect("custom binary root");
    fs::write(repository.path().join("src/shared.rs"), "fn production_helper() {}\n").expect("shared module");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/shared.rs"].production_lines, 1);
}

#[test]
fn custom_binary_child_modules_are_composition_only() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src/cli", "tests", "benches"] {
        fs::create_dir_all(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[bin]]\nname = \"cli\"\npath = \"src/cli/main.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn library() {}\n").expect("library root");
    fs::write(repository.path().join("src/cli/main.rs"), "mod worker;\nfn main() {}\n").expect("binary root");
    fs::write(repository.path().join("src/cli/worker.rs"), "use crate::server::Service;\n").expect("binary child");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let worker = inventory.files.iter().find(|file| file.path == "src/cli/worker.rs").expect("worker measurement");
    assert!(worker.production_internal_imports.is_empty());
}

#[test]
fn ordinary_lib_and_main_modules_resolve_children_from_their_named_directory() {
    for module in ["lib", "main"] {
        let repository = tempfile::tempdir().expect("temporary repository");
        for root in [format!("src/foo/{module}"), "tests".to_owned(), "benches".to_owned()] {
            fs::create_dir_all(repository.path().join(root)).expect("source root");
        }
        let nested = format!("src/foo/{module}/nested.rs");
        fs::write(
            repository.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
                 \n[[bin]]\nname = \"nested-{module}\"\npath = \"{nested}\"\n"
            ),
        )
        .expect("package manifest");
        fs::write(repository.path().join("src/lib.rs"), "mod foo;\n").expect("library root");
        fs::write(repository.path().join("src/foo.rs"), format!("mod {module};\n")).expect("ordinary parent module");
        fs::write(repository.path().join(format!("src/foo/{module}.rs")), "mod nested;\n").expect("ordinary named module");
        fs::write(repository.path().join(nested), "use crate::server::Service;\nfn main() {}\n").expect("shared nested module");

        let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("composition target must not also be reachable from a library target"));
    }
}

#[test]
fn case_mismatched_module_sources_fail_closed_on_every_platform() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[bin]]\nname = \"shared\"\npath = \"src/Shared.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "mod shared;\n").expect("library root");
    fs::write(repository.path().join("src/Shared.rs"), "use crate::server::Service;\nfn main() {}\n").expect("mismatched module source");

    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
    assert!(format!("{error:#}").contains("source path case does not match its declaration"));
}

#[test]
fn disabled_automatic_binaries_leave_src_bin_modules_in_the_library_graph() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src/bin", "tests", "benches"] {
        fs::create_dir_all(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\nautobins = false\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "mod bin { mod worker; }\n").expect("library root");
    fs::write(repository.path().join("src/bin/worker.rs"), "use crate::server::Service;\n").expect("library child");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let worker = inventory.files.iter().find(|file| file.path == "src/bin/worker.rs").expect("worker measurement");
    assert_eq!(worker.production_internal_imports, ["crate::server::Service"]);
}

#[test]
fn declared_targets_infer_paths_when_automatic_discovery_is_disabled() {
    for (binary_name, binary_path) in [("fixture", "src/main.rs"), ("worker", "src/bin/worker.rs"), ("worker", "src/bin/worker/main.rs")] {
        let repository = tempfile::tempdir().expect("temporary repository");
        for root in ["src", "tests", "benches"] {
            fs::create_dir_all(repository.path().join(root)).expect("source root");
        }
        fs::create_dir_all(repository.path().join(binary_path).parent().expect("binary parent")).expect("binary parent");
        fs::write(
            repository.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\nautolib = false\nautobins = false\n\
                 \n[lib]\n\
                 \n[[bin]]\nname = \"{binary_name}\"\n"
            ),
        )
        .expect("package manifest");
        fs::write(repository.path().join("src/lib.rs"), "fn library() {}\n").expect("library root");
        fs::write(repository.path().join(binary_path), "use crate::server::CompositionOnly;\nfn main() {}\n").expect("binary root");

        let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("inferred target roots");
        let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
        assert_eq!(by_path["src/lib.rs"].production_lines, 1);
        assert!(by_path[binary_path].production_internal_imports.is_empty());
    }
}

#[test]
fn default_rust_2015_manifest_classifies_absolute_crate_paths() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(repository.path().join("Cargo.toml"), "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "mod server;\nfn build() -> ::server::Service { loop {} }\n").expect("library root");
    fs::write(repository.path().join("src/server.rs"), "pub struct Service;\n").expect("server module");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let library = inventory.files.iter().find(|file| file.path == "src/lib.rs").expect("library measurement");
    assert_eq!(library.production_internal_imports, ["crate::server::Service"]);
}

#[test]
fn package_edition_can_be_inherited_from_workspace_policy() {
    for (edition, expected) in [("2015", vec!["crate::server::Service"]), ("2024", Vec::new())] {
        let repository = tempfile::tempdir().expect("temporary repository");
        for root in ["src", "tests", "benches"] {
            fs::create_dir(repository.path().join(root)).expect("source root");
        }
        fs::write(
            repository.path().join("Cargo.toml"),
            format!(
                "[workspace]\n\
                 \n[workspace.package]\nedition = \"{edition}\"\n\
                 \n[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition.workspace = true\n"
            ),
        )
        .expect("package manifest");
        fs::write(repository.path().join("src/lib.rs"), "mod server;\nfn build() -> ::server::Service { loop {} }\n").expect("library root");
        fs::write(repository.path().join("src/server.rs"), "pub struct Service;\n").expect("server module");

        let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace-inherited edition");
        let library = inventory.files.iter().find(|file| file.path == "src/lib.rs").expect("library measurement");
        assert_eq!(library.production_internal_imports, expected);
    }
}

#[test]
fn modules_shared_with_the_library_are_not_composition_only() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir_all(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[bin]]\nname = \"cli\"\npath = \"src/cli.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "mod shared;\n").expect("library root");
    fs::write(repository.path().join("src/cli.rs"), "mod shared;\nfn main() {}\n").expect("binary root");
    fs::write(repository.path().join("src/shared.rs"), "use crate::server::Service;\n").expect("shared module");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let shared = inventory.files.iter().find(|file| file.path == "src/shared.rs").expect("shared measurement");
    assert_eq!(shared.production_internal_imports, ["crate::server::Service"]);
}

#[test]
fn custom_library_targets_resolve_imports_from_the_crate_root() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[lib]\npath = \"src/core.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/core.rs"), "use self::server::LocalHoldServer;\n").expect("custom library root");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    assert_eq!(inventory.files[0].production_internal_imports, ["crate::server::LocalHoldServer"]);
}

#[test]
fn custom_library_roots_cannot_claim_server_or_ui_exemptions() {
    for path in ["src/server/mod.rs", "src/ui/lib.rs"] {
        let repository = tempfile::tempdir().expect("temporary repository");
        for root in ["src", "tests", "benches"] {
            fs::create_dir_all(repository.path().join(root)).expect("source root");
        }
        fs::create_dir_all(repository.path().join(path).parent().expect("library parent")).expect("library parent");
        fs::write(
            repository.path().join("Cargo.toml"),
            format!("[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"{path}\"\n"),
        )
        .expect("package manifest");
        fs::write(repository.path().join(path), "use crate::server::Hidden;\n").expect("custom library root");

        let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("cannot use an exempt server/UI directory"));
    }
}

#[test]
fn nested_custom_library_targets_resolve_child_imports_from_the_crate_root() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src/core", "tests", "benches"] {
        fs::create_dir_all(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[lib]\npath = \"src/core/lib.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/core/lib.rs"), "mod worker;\n").expect("custom library root");
    fs::write(repository.path().join("src/core/worker.rs"), "use super::server::LocalHoldServer;\n").expect("custom library child");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let worker = inventory.files.iter().find(|file| file.path == "src/core/worker.rs").expect("worker measurement");
    assert_eq!(worker.production_internal_imports, ["crate::server::LocalHoldServer"]);
}

#[test]
fn custom_library_roots_do_not_create_ordinary_module_edges() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src/core", "tests", "benches"] {
        fs::create_dir_all(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[lib]\npath = \"src/core.rs\"\n\
         \n[[bin]]\nname = \"unrelated\"\npath = \"src/core/worker.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/core.rs"), "mod worker;\n").expect("custom library root");
    fs::write(repository.path().join("src/worker.rs"), "fn library_worker() {}\n").expect("library worker");
    fs::write(repository.path().join("src/core/worker.rs"), "fn main() {}\n").expect("unrelated binary");

    scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("valid disjoint targets");
}

#[test]
fn composition_targets_cannot_overlap_library_sources() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[bin]]\nname = \"shared\"\npath = \"src/lib.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "use crate::server::LocalHoldServer;\nfn main() {}\n").expect("shared target");

    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("composition target must not also be reachable from a library target"));
}

#[test]
fn raw_module_identifiers_cannot_hide_composition_overlap() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[bin]]\nname = \"shared\"\npath = \"src/type.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "mod r#type;\n").expect("library root");
    fs::write(repository.path().join("src/type.rs"), "use crate::server::LocalHoldServer;\nfn main() {}\n").expect("shared target");

    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("composition target must not also be reachable from a library target"));
}

#[test]
fn production_item_macros_cannot_hide_composition_overlap() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[bin]]\nname = \"shared\"\npath = \"src/shared.rs\"\n",
    )
    .expect("package manifest");
    fs::write(
        repository.path().join("src/lib.rs"),
        "macro_rules! include_shared { () => { mod shared; } }\ninclude_shared!();\n",
    )
    .expect("library root");
    fs::write(repository.path().join("src/shared.rs"), "use crate::server::LocalHoldServer;\nfn main() {}\n").expect("shared target");

    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("production item macros cannot safely define module edges"));
}

#[test]
fn local_item_macros_without_module_tokens_are_allowed() {
    let sources = BTreeMap::from([(
        "src/lib.rs".to_owned(),
        "macro_rules! numbered_placeholders { () => { const VALUE: usize = 1; } }\n\
         numbered_placeholders!();\n"
            .to_owned(),
    )]);

    measure_sources(sources).expect("known local macro cannot introduce module edges");
}

#[test]
fn unknown_production_item_macro_invocations_fail_closed() {
    let sources = BTreeMap::from([("src/lib.rs".to_owned(), "external_item_macro!();\n".to_owned())]);

    let error = measure_sources(sources).unwrap_err();
    assert!(error.to_string().contains("production item macros cannot safely define module edges"));
}

#[test]
fn local_item_macros_cannot_delegate_module_generation_to_unknown_macros() {
    let sources = BTreeMap::from([(
        "src/lib.rs".to_owned(),
        "macro_rules! delegate { () => { external_item_macro!(); } }\ndelegate!();\n".to_owned(),
    )]);

    let error = measure_sources(sources).unwrap_err();
    assert!(error.to_string().contains("production item macros cannot safely define module edges"));
}

#[test]
fn local_item_macros_cannot_substitute_module_keywords() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[bin]]\nname = \"shared\"\npath = \"src/shared.rs\"\n",
    )
    .expect("package manifest");
    fs::write(
        repository.path().join("src/lib.rs"),
        "macro_rules! emit { ($kind:ident) => { $kind shared; } }\nemit!(mod);\n",
    )
    .expect("library root");
    fs::write(repository.path().join("src/shared.rs"), "use crate::server::LocalHoldServer;\nfn main() {}\n").expect("shared target");

    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("production item macros cannot safely define module edges"));
}

#[test]
fn reviewed_local_macros_cannot_export_restricted_dependencies() {
    let sources = BTreeMap::from([
        ("src/lib.rs".to_owned(), "mod server;\n".to_owned()),
        (
            "src/server.rs".to_owned(),
            "#[macro_export]\nmacro_rules! transport_test { () => { $crate::server::secret() } }\n".to_owned(),
        ),
    ]);

    let error = measure_sources(sources).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("reviewed local macro"), "{message}");
    assert!(message.contains("server"));
}

#[test]
fn reviewed_local_macro_definitions_cannot_encode_restricted_path_literals() {
    let sources = BTreeMap::from([
        ("src/lib.rs".to_owned(), "mod server;\n".to_owned()),
        (
            "src/server.rs".to_owned(),
            "#[macro_export]\nmacro_rules! transport_test {\n\
                 () => { #[serde(serialize_with = \"crate::server::serialize\")] struct Generated; }\n\
             }\n"
            .to_owned(),
        ),
    ]);

    let error = measure_sources(sources).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("reviewed local macro"), "{message}");
    assert!(message.contains("server"));
}

#[test]
fn reviewed_exported_macros_are_audited_below_non_module_items() {
    let sources = BTreeMap::from([
        ("src/lib.rs".to_owned(), "mod server;\n".to_owned()),
        (
            "src/server.rs".to_owned(),
            "fn install() {\n\
                 #[macro_export]\nmacro_rules! transport_test { () => { $crate::server::secret() } }\n\
             }\n"
            .to_owned(),
        ),
    ]);
    let error = measure_sources(sources).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("reviewed local macro"), "{message}");
    assert!(message.contains("server"));

    let test_only = BTreeMap::from([(
        "src/lib.rs".to_owned(),
        "#[cfg(test)]\nfn install() {\n\
             #[macro_export]\nmacro_rules! transport_test { () => { $crate::server::test_only() } }\n\
         }\n"
        .to_owned(),
    )]);
    measure_sources(test_only).expect("test-only nested macro definition");
}

#[test]
fn reviewed_macros_in_wholly_test_only_sources_are_not_production_audited() {
    let sources = BTreeMap::from([
        ("src/lib.rs".to_owned(), "#[cfg(test)]\nmod test_support;\n".to_owned()),
        (
            "src/test_support.rs".to_owned(),
            "#[macro_export]\nmacro_rules! transport_test { () => { $crate::server::test_only() } }\n".to_owned(),
        ),
        (
            "tests/contract.rs".to_owned(),
            "#[macro_export]\nmacro_rules! transport_test { () => { $crate::ui::test_only() } }\n".to_owned(),
        ),
    ]);

    let inventory = measure_sources(sources).expect("test-only macro definitions");
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/test_support.rs"].production_lines, 0);
    assert_eq!(by_path["tests/contract.rs"].production_lines, 0);
}

#[test]
fn reviewed_production_macros_cannot_splice_opaque_metavariables_into_paths() {
    let sources = BTreeMap::from([(
        "src/lib.rs".to_owned(),
        "macro_rules! numbered_placeholders { ($part:ident) => { crate::$part::Service } }\n\
         fn build() { let _ = numbered_placeholders!(server); }\n"
            .to_owned(),
    )]);

    let error = measure_sources(sources).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("reviewed local macro"), "{message}");
    assert!(message.contains("non-literal metavariables"), "{message}");
}

#[test]
fn literal_only_local_macro_parameters_remain_classifiable() {
    let sources = BTreeMap::from([(
        "src/lib.rs".to_owned(),
        "macro_rules! numbered_placeholders { ($value:literal) => { const VALUE: usize = $value; } }\n\
         numbered_placeholders!(1);\n"
            .to_owned(),
    )]);

    measure_sources(sources).expect("literal substitutions cannot inject module syntax");
}

#[test]
fn no_package_feature_may_enable_testing() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");

    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[features]\ntesting = []\nrelease-hooks = [\"testing\"]\n",
    )
    .expect("package manifest");
    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("must not enable the test-only"));

    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[dependencies]\ntesting = { version = \"1\", optional = true }\n\
         \n[features]\ntesting = []\nrelease-hooks = [\"testing/fixtures\", \"testing?/optional-fixtures\"]\n",
    )
    .expect("package manifest");
    scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()])
        .expect("dependency feature syntax does not activate the local testing feature");
}

#[test]
fn revision_scan_honors_declared_test_targets_under_src() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::create_dir(repository.path().join("src")).expect("source root");
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[test]]\nname = \"custom-test\"\npath = \"src/custom_test.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("src/custom_test.rs"), "fn test_target() {}\n").expect("test target");
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

    let inventory = scan_revision(repository.path(), &revision, &["src".to_owned()]).expect("revision inventory");
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/custom_test.rs"].production_lines, 0);
}

#[test]
fn rust_examples_and_auxiliary_targets_outside_the_inventory_are_rejected() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches", "examples", "fixtures"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[[test]]\nname = \"escape\"\npath = \"fixtures/escape.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("fixtures/escape.rs"), "fn escaped_test() {}\n").expect("escaped test target");
    let roots = ["src".to_owned(), "tests".to_owned(), "benches".to_owned()];

    let error = scan_workspace(repository.path(), &roots).unwrap_err();
    assert!(error.to_string().contains("outside the structural source inventory"));

    fs::write(repository.path().join("Cargo.toml"), "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").expect("package manifest");
    fs::write(repository.path().join("examples/demo.rs"), "fn main() {}\n").expect("example source");
    let error = scan_workspace(repository.path(), &roots).unwrap_err();
    assert!(error.to_string().contains("untracked examples/"));
}

#[test]
fn explicit_and_conditional_module_paths_fail_closed() {
    for declaration in [
        "#[path = \"../tests/helper.rs\"]\nmod helper;\n",
        "#[cfg_attr(unix, path = \"../tests/helper.rs\")]\nmod helper;\n",
    ] {
        let sources = [
            ("src/lib.rs".to_owned(), declaration.to_owned()),
            ("tests/helper.rs".to_owned(), "fn helper() {}\n".to_owned()),
        ]
        .into_iter()
        .collect();
        assert!(measure_sources(sources).unwrap_err().to_string().contains("module paths cannot be classified safely"));
    }
}

#[test]
fn revision_scan_preserves_special_paths_and_reads_blobs_in_one_batch() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::create_dir(repository.path().join("src")).expect("source directory");
    fs::write(repository.path().join("Cargo.toml"), "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("src/café space.rs"), "fn special() {}\n").expect("special source");
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

    let inventory = scan_revision(repository.path(), &revision, &["src".to_owned()]).expect("revision inventory");
    let paths = inventory.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>();
    assert_eq!(paths, ["src/café space.rs", "src/lib.rs"]);
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
