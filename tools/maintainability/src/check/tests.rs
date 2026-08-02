use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::manifest::DependencyPin;
use tempfile::tempdir;

use super::{
    LockedPackage, compare_dependency_packages, lint_setting, parse_root_dependency, verify_audited_target_path, verify_cargo_metadata_workspace, verify_cargo_target_paths,
    verify_dependency_routes, verify_expansion_dependency_routes, verify_first_party_package_routes, verify_lint, verify_lint_precedence,
    verify_maintainer_expansion_dependency_routes, verify_no_cargo_config, verify_no_cargo_config_with_home,
};

fn cargo(source: &str) -> toml::Value {
    toml::from_str(source).expect("Cargo TOML")
}

#[test]
fn lint_setting_supports_cargo_string_and_table_forms() {
    let string = cargo("value = 'deny'").get("value").expect("value").clone();
    let table = cargo("value = { level = 'deny', priority = -1 }").get("value").expect("value").clone();
    assert_eq!(lint_setting(&string).expect("string setting"), ("deny", 0));
    assert_eq!(lint_setting(&table).expect("table setting"), ("deny", -1));
}

#[test]
fn required_lint_rejects_missing_weakened_or_wrong_priority_configuration() {
    let cargo = cargo(
        "
        [lints.rust]
        unsafe_code = { level = 'warn', priority = 1 }
        unsafe_op_in_unsafe_fn = 'deny'
        ",
    );
    assert!(verify_lint(&cargo, "rust", "unsafe_code", "deny", 1).is_err());
    assert!(verify_lint(&cargo, "rust", "unsafe_op_in_unsafe_fn", "deny", 1).is_err());
    assert!(verify_lint(&cargo, "clippy", "undocumented_unsafe_blocks", "deny", 1).is_err());
}

#[test]
fn required_lints_reserve_priority_over_other_settings() {
    let requirements = [("rust", "unsafe_code", "deny", 1), ("rust", "unsafe_op_in_unsafe_fn", "deny", 1)];
    let accepted = cargo(
        "
        [lints.rust]
        unsafe_code = { level = 'deny', priority = 1 }
        unsafe_op_in_unsafe_fn = { level = 'deny', priority = 1 }
        rust_2024_compatibility = { level = 'allow', priority = 0 }
        ",
    );
    assert!(verify_lint_precedence(&accepted, &requirements).is_ok());

    let unrelated = cargo(
        "
        [lints.rust]
        unsafe_code = { level = 'deny', priority = 1 }
        unsafe_op_in_unsafe_fn = { level = 'deny', priority = 1 }
        dead_code = { level = 'deny', priority = 2 }
        ",
    );
    assert!(verify_lint_precedence(&unrelated, &requirements).is_ok());

    for level in ["deny", "forbid"] {
        for priority in [1, 2] {
            let strengthened = cargo(&format!(
                "
                [lints.rust]
                unsafe_code = {{ level = 'deny', priority = 1 }}
                unsafe_op_in_unsafe_fn = {{ level = 'deny', priority = 1 }}
                rust_2024_compatibility = {{ level = '{level}', priority = {priority} }}
                "
            ));
            assert!(verify_lint_precedence(&strengthened, &requirements).is_ok());
        }
    }

    for level in ["allow", "warn", "force-warn"] {
        let weakened = cargo(&format!(
            "
            [lints.rust]
            unsafe_code = {{ level = 'deny', priority = 1 }}
            unsafe_op_in_unsafe_fn = {{ level = 'deny', priority = 1 }}
            rust_2024_compatibility = {{ level = '{level}', priority = 1 }}
            "
        ));
        assert!(verify_lint_precedence(&weakened, &requirements).is_err());
    }
}

#[test]
fn clippy_groups_cannot_override_documentation_requirement() {
    let requirements = [("clippy", "undocumented_unsafe_blocks", "deny", 1)];
    let accepted = cargo(
        "
        [lints.clippy]
        undocumented_unsafe_blocks = { level = 'deny', priority = 1 }
        restriction = { level = 'allow', priority = 0 }
        ",
    );
    assert!(verify_lint_precedence(&accepted, &requirements).is_ok());

    for level in ["deny", "forbid"] {
        let strengthened = cargo(&format!(
            "
            [lints.clippy]
            undocumented_unsafe_blocks = {{ level = 'deny', priority = 1 }}
            restriction = {{ level = '{level}', priority = 2 }}
            "
        ));
        assert!(verify_lint_precedence(&strengthened, &requirements).is_ok());
    }

    let overridden = cargo(
        "
        [lints.clippy]
        undocumented_unsafe_blocks = { level = 'deny', priority = 1 }
        restriction = { level = 'allow', priority = 1 }
        ",
    );
    assert!(verify_lint_precedence(&overridden, &requirements).is_err());

    let malformed = cargo(
        "
        [lints.clippy]
        undocumented_unsafe_blocks = { level = 'deny', priority = 1 }
        restriction = { level = 'allow', priority = 'later' }
        ",
    );
    assert!(verify_lint_precedence(&malformed, &requirements).is_err());
}

#[test]
fn workspace_cargo_config_cannot_override_safety_flags() {
    let workspace = tempdir().expect("temporary workspace");
    let project = workspace.path().join("project");
    fs::create_dir(&project).expect("nested project");
    assert!(verify_no_cargo_config(&project).is_ok());
    fs::create_dir(workspace.path().join(".cargo")).expect("Cargo config directory");
    fs::write(workspace.path().join(".cargo/config.toml"), "[build]\nrustflags = ['--cap-lints=allow']\n").expect("Cargo config");
    assert!(verify_no_cargo_config(&project).is_err());

    let isolated = tempdir().expect("isolated workspace");
    let cargo_home = isolated.path().join("cargo-home");
    fs::create_dir(&cargo_home).expect("Cargo home");
    fs::write(cargo_home.join("config"), "[build]\nrustflags = ['--cap-lints=allow']\n").expect("Cargo home config");
    assert!(verify_no_cargo_config_with_home(isolated.path(), Some(&cargo_home)).is_err());
}

#[test]
fn dependency_contract_rejects_source_checksum_version_and_multiplicity_drift() {
    let pin = DependencyPin {
        name: "sqlite-vec".to_owned(),
        version: "0.1.9".to_owned(),
        source: "registry".to_owned(),
        checksum: "checksum".to_owned(),
        resolved_features: Vec::new(),
        incoming_routes: vec!["root".to_owned()],
    };
    let required = BTreeMap::from([("sqlite-vec", &pin)]);
    let package = LockedPackage {
        name: "sqlite-vec".to_owned(),
        version: "0.1.9".to_owned(),
        source: Some("registry".to_owned()),
        checksum: Some("checksum".to_owned()),
    };
    assert!(compare_dependency_packages(&BTreeMap::new(), &required).is_err());
    assert!(compare_dependency_packages(&BTreeMap::from([("sqlite-vec", vec![&package, &package])]), &required).is_err());
    assert!(compare_dependency_packages(&BTreeMap::from([("sqlite-vec", vec![&package])]), &required).is_ok());

    for drifted in [
        LockedPackage {
            version: "0.2.0".to_owned(),
            ..package.clone()
        },
        LockedPackage {
            source: Some("git".to_owned()),
            ..package.clone()
        },
        LockedPackage {
            checksum: Some("changed".to_owned()),
            ..package
        },
    ] {
        assert!(compare_dependency_packages(&BTreeMap::from([("sqlite-vec", vec![&drifted])]), &required).is_err());
    }
}

#[test]
fn root_dependency_contract_rejects_feature_and_route_drift() {
    let string = cargo("value = '0.1'").get("value").expect("value").clone();
    let parsed = parse_root_dependency(&string).expect("string dependency");
    assert_eq!(parsed.version, "0.1");
    assert!(parsed.default_features);
    assert!(parsed.features.is_empty());

    let table = cargo("value = { version = '0.40', default-features = false, features = ['backup', 'bundled'] }")
        .get("value")
        .expect("value")
        .clone();
    let parsed = parse_root_dependency(&table).expect("table dependency");
    assert!(!parsed.default_features);
    assert_eq!(parsed.features, ["backup", "bundled"]);

    let unsupported = cargo("value = { version = '0.40', path = '../other' }").get("value").expect("value").clone();
    assert!(parse_root_dependency(&unsupported).is_err());
}

#[test]
fn alternate_dependency_routes_fail_closed() {
    let protected = BTreeSet::from(["libsqlite3-sys", "rusqlite", "sqlite-vec"]);
    let reviewed_root = BTreeSet::from(["rusqlite", "sqlite-vec"]);
    let accepted = cargo(
        "
        [dependencies]
        rusqlite = { version = '0.40', features = ['bundled'] }
        sqlite-vec = '0.1'

        [features]
        testing = []
        ",
    );
    assert!(verify_dependency_routes(&accepted, &protected, &reviewed_root).is_ok());

    for rejected in [
        "
        [dependencies]
        rusqlite = '0.40'
        sqlite-alias = { package = 'rusqlite', version = '0.40', features = ['modern_sqlite'] }
        ",
        "
        [dev-dependencies]
        sqlite-alias = { package = 'sqlite-vec', version = '0.1' }
        ",
        "
        [build-dependencies]
        rusqlite = { version = '0.40', features = ['modern_sqlite'] }
        ",
        "
        [dependencies]
        libsqlite3-sys = { version = '0.38.1', features = ['bundled'] }
        ",
        "
        [target.'cfg(unix)'.dependencies]
        sqlite-sys = { package = 'libsqlite3-sys', version = '0.38.1', features = ['bundled'] }
        ",
        "
        [target.'cfg(target_os = \"macos\")'.dependencies]
        sqlite-alias = { package = 'rusqlite', version = '0.40', features = ['modern_sqlite'] }
        ",
        "
        [workspace.dependencies]
        sqlite-alias = { package = 'rusqlite', version = '0.40', features = ['modern_sqlite'] }

        [target.'cfg(unix)'.dependencies]
        sqlite-alias = { workspace = true }
        ",
        "
        [features]
        extra = ['rusqlite/modern_sqlite']
        ",
        "
        [features]
        extra = ['rusqlite?/modern_sqlite']
        ",
        "
        [features]
        extra = ['dep:sqlite-vec']
        ",
        "
        [features]
        extra = ['libsqlite3-sys/bundled']
        ",
    ] {
        assert!(
            verify_dependency_routes(&cargo(rejected), &protected, &reviewed_root).is_err(),
            "route must be rejected: {rejected}"
        );
    }
}

#[test]
fn expansion_dependencies_cannot_be_renamed_or_impersonated() {
    let accepted = cargo(
        "
        [dependencies]
        tokio = '1'
        tracing = '0.1'

        [dev-dependencies]
        proptest = '1'
        ",
    );
    assert!(verify_expansion_dependency_routes(&accepted).is_ok());
    assert!(verify_expansion_dependency_routes(&cargo("[dependencies]\nquote = '1'\n")).is_err());
    assert!(verify_maintainer_expansion_dependency_routes(&cargo("[dependencies]\nanyhow = '1'\nquote = '1'\nsyn = '2'\n")).is_ok());

    for rejected in [
        "
        [dependencies]
        tokio = { package = 'opaque-macro', path = '../opaque' }
        ",
        "
        [dependencies]
        runtime = { package = 'tokio', version = '1' }
        ",
        "
        [target.'cfg(unix)'.dev-dependencies]
        tracing = { package = 'opaque-macro', path = '../opaque' }
        ",
        "
        [dependencies]
        tokio = { path = '../crafted-tokio' }
        ",
        "
        [dependencies]
        tracing = { git = 'https://example.invalid/tracing', version = '0.1' }
        ",
        "
        [dependencies]
        serde = { workspace = true }
        ",
        "
        [dependencies]
        transport_test = { path = '../opaque-macro' }
        ",
        "
        [dependencies]
        serde-json = { package = 'opaque-macro', path = '../opaque-macro' }
        ",
        "
        [dependencies]
        transport-test = { package = 'opaque-macro', path = '../opaque-macro' }
        ",
        "
        [dependencies]
        tokio = '1'

        [patch.crates-io]
        tokio = { path = '../crafted-tokio' }
        ",
        "
        [dependencies]
        tokio = '1'

        [replace]
        'tokio:1.0.0' = { path = '../crafted-tokio' }
        ",
    ] {
        assert!(
            verify_expansion_dependency_routes(&cargo(rejected)).is_err(),
            "renamed expansion dependency must be rejected: {rejected}"
        );
    }
}

#[test]
fn cargo_target_paths_cannot_escape_audited_roots() {
    for allowed in ["src/main.rs", "tests/contract.rs", "benches/latency.rs", "examples/client.rs"] {
        assert!(verify_audited_target_path("target.path", allowed).is_ok());
    }
    for rejected in ["build.rs", "outside.rs", "src/../outside.rs", "/absolute.rs", "src/not-rust.txt"] {
        assert!(verify_audited_target_path("target.path", rejected).is_err());
    }
}

#[test]
fn package_build_scripts_are_disabled() {
    for accepted in ["[package]\nname = 'fixture'", "[package]\nname = 'fixture'\nbuild = false"] {
        assert!(verify_cargo_target_paths(&cargo(accepted)).is_ok(), "configuration must be accepted: {accepted}");
    }
    for rejected in [
        "[package]\nbuild = 'build.rs'",
        "[package]\nbuild = 'src/build.rs'",
        "[package]\nbuild = true",
        "[package]\nbuild = 1",
        "[package]\nbuild = { workspace = true }",
    ] {
        assert!(verify_cargo_target_paths(&cargo(rejected)).is_err(), "build script must be rejected: {rejected}");
    }
}

#[test]
fn first_party_rust_stays_in_the_audited_root_package() {
    let reviewed_self_route = cargo(
        "
        [package]
        name = 'localhold'
        [dev-dependencies]
        localhold = { path = '.', features = ['testing'] }
        ",
    );
    assert!(verify_first_party_package_routes(&reviewed_self_route).is_ok());

    for rejected in [
        "[package]\nname = 'localhold'\n[workspace]\nmembers = ['crates/helper']",
        "[package]\nname = 'localhold'\nworkspace = '..'",
        "[package]\nname = 'localhold'\n[dependencies]\nhelper = { path = 'crates/helper' }",
        "[package]\nname = 'localhold'\n[build-dependencies]\nhelper = { path = 'crates/helper' }",
        "[package]\nname = 'localhold'\n[dev-dependencies]\nhelper = { path = 'crates/helper' }",
        "[package]\nname = 'localhold'\n[dev-dependencies]\nalias = { package = 'localhold', path = '.' }",
        "[package]\nname = 'localhold'\n[target.'cfg(unix)'.dependencies]\nhelper = { path = 'crates/helper' }",
    ] {
        assert!(
            verify_first_party_package_routes(&cargo(rejected)).is_err(),
            "local package route must be rejected: {rejected}"
        );
    }
}

#[test]
fn cargo_metadata_cannot_resolve_an_external_workspace_root() {
    let workspace = tempdir().expect("audited workspace");
    let external = tempdir().expect("external workspace");
    let metadata = |workspace_root: &std::path::Path| serde_json::to_vec(&serde_json::json!({ "workspace_root": workspace_root })).expect("Cargo metadata");

    assert!(verify_cargo_metadata_workspace(workspace.path(), &metadata(workspace.path())).is_ok());
    assert!(verify_cargo_metadata_workspace(workspace.path(), &metadata(external.path())).is_err());
}
