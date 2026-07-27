use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::manifest::DependencyPin;
use tempfile::tempdir;

use super::{
    LockedPackage, compare_dependency_packages, lint_setting, parse_root_dependency, verify_audited_target_path, verify_dependency_routes, verify_lint, verify_lint_precedence,
    verify_no_workspace_cargo_config,
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

    for priority in [1, 2] {
        let overridden = cargo(&format!(
            "
            [lints.rust]
            unsafe_code = {{ level = 'deny', priority = 1 }}
            unsafe_op_in_unsafe_fn = {{ level = 'deny', priority = 1 }}
            rust_2024_compatibility = {{ level = 'allow', priority = {priority} }}
            "
        ));
        assert!(verify_lint_precedence(&overridden, &requirements).is_err());
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
    assert!(verify_no_workspace_cargo_config(workspace.path()).is_ok());
    fs::create_dir(workspace.path().join(".cargo")).expect("Cargo config directory");
    fs::write(workspace.path().join(".cargo/config.toml"), "[build]\nrustflags = ['--cap-lints=allow']\n").expect("Cargo config");
    assert!(verify_no_workspace_cargo_config(workspace.path()).is_err());
}

#[test]
fn dependency_contract_rejects_source_checksum_version_and_multiplicity_drift() {
    let pin = DependencyPin {
        name: "sqlite-vec".to_owned(),
        version: "0.1.9".to_owned(),
        source: "registry".to_owned(),
        checksum: "checksum".to_owned(),
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
fn cargo_target_paths_cannot_escape_audited_roots() {
    for allowed in ["src/main.rs", "tests/contract.rs", "benches/latency.rs", "examples/client.rs"] {
        assert!(verify_audited_target_path("target.path", allowed).is_ok());
    }
    for rejected in ["build.rs", "outside.rs", "src/../outside.rs", "/absolute.rs", "src/not-rust.txt"] {
        assert!(verify_audited_target_path("target.path", rejected).is_err());
    }
}
