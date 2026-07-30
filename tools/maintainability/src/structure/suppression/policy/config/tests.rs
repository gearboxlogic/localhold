use std::fs;
use std::process::Command;

use super::*;
use crate::structure::suppression::policy::model::{ClippyConstraint, ClippySetting};

#[test]
fn cargo_allow_scan_covers_root_and_nested_manifests() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("tool")).expect("tool directory");
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[package]\nname='root'\nversion='0.1.0'\n[lints.clippy]\nunwrap_used='allow'\npanic='warn'\n",
    )
    .expect("root manifest");
    fs::write(
        workspace.path().join("tool/Cargo.toml"),
        "[package]\nname='tool'\nversion='0.1.0'\n[lints.rust]\nunsafe_code={level='allow'}\n",
    )
    .expect("tool manifest");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    assert_eq!(
        scan_cargo_allows(workspace.path()).expect("Cargo allowances"),
        BTreeSet::from([
            ("Cargo.toml".to_owned(), "clippy".to_owned(), "unwrap_used".to_owned()),
            ("tool/Cargo.toml".to_owned(), "rust".to_owned(), "unsafe_code".to_owned()),
        ])
    );
}

#[test]
fn clippy_constraints_are_directional() {
    compare_clippy_value("threshold", &toml::Value::Integer(4), &ClippyConstraint::MaximumInteger { value: 5 }).expect("lower threshold");
    assert!(compare_clippy_value("threshold", &toml::Value::Integer(6), &ClippyConstraint::MaximumInteger { value: 5 },).is_err());
    compare_clippy_value(
        "idents",
        &toml::Value::Array(vec![toml::Value::String("MCP".to_owned())]),
        &ClippyConstraint::StringSubset {
            values: vec!["MCP".to_owned(), "SQLite".to_owned()],
        },
    )
    .expect("smaller allowlist");
}

#[test]
fn weakening_tokens_distinguish_rust_lint_flags_from_application_options() {
    assert!(weakening_token("cargo clippy -- -A warnings"));
    assert!(weakening_token("cargo rustc -- --cap-lints=allow"));
    assert!(weakening_token("cargo clippy -- --allow warnings"));
    assert!(weakening_token("cargo clippy -- -W warnings"));
    assert!(weakening_token("cargo clippy -- --warn warnings"));
    assert!(weakening_token("cargo --config build.rustflags='-A warnings' clippy"));
    assert!(weakening_token("cargo deny --config deny.toml; cargo --config build.rustflags='-A warnings' clippy"));
    assert!(weakening_token("cargo \\\n  --config build.rustflags='-A warnings' clippy"));
    assert!(weakening_token("cargo -Z unstable-options -C ../other build"));
    assert!(weakening_token("cargo --change-directory=../other build"));
    assert!(!weakening_token("cargo rustc -- -C opt-level=3"));
    assert!(weakening_token("cargo-clippy clippy -- -A warnings"));
    assert!(weakening_token("cargo clippy -- -\"\"A warnings"));
    assert!(weakening_token("cargo clippy -- \"--al\"low warnings"));
    assert!(weakening_token("cargo clippy -- -\\A warnings"));
    assert!(weakening_token("rustc @policy/lints.args source.rs"));
    assert!(weakening_token("LABEL=@not-a-response rustc @policy/lints.args source.rs"));
    assert!(weakening_token("cargo rustc -- @policy/lints.args"));
    assert!(!weakening_token("cargo run -- @application-argument"));
    assert!(weakening_token("sh -c \"cargo --config net.offline=true check # literal\""));
    assert!(!weakening_token("cargo deny --config deny.toml"));
    assert!(!weakening_token("gitleaks --config policy.toml # cargo output"));
    assert!(!weakening_token("cargo build\ncc -Wall -Wextra"));
    assert!(!weakening_token("hold doctor --allow-downloads"));
    assert!(weakening_environment("export RUSTFLAGS='-A warnings'\nexec \"$CHECK\""));
    assert!(weakening_environment("CARGO_ENCODED_RUSTFLAGS=dynamic"));
    assert!(weakening_environment("RUSTDOCFLAGS=--cap-lints=allow"));
    assert!(weakening_environment("CARGO_ENCODED_RUSTDOCFLAGS=dynamic"));
    assert!(weakening_environment("CARGO_BUILD_RUSTDOCFLAGS=--cap-lints=allow"));
    assert!(weakening_environment("CARGO_TARGET_TEST_RUSTDOCFLAGS=--cap-lints=allow"));
    assert!(weakening_environment("CLIPPY_ARGS='--allow warnings'"));
    assert!(weakening_environment("CLIPPY_CONF_DIR=unreviewed"));
    assert!(weakening_environment("RUSTC_WRAPPER=unreviewed"));
    assert!(weakening_environment("CARGO_TARGET_TEST_RUSTFLAGS=unreviewed"));
    assert!(weakening_environment("CARGO_HOME=unreviewed"));
    assert!(weakening_environment("unset GITHUB_ACTIONS"));
    assert!(weakening_environment("unset GITHUB_EVENT_PATH"));
    assert!(weakening_environment("GITHUB_SHA=untrusted"));
    assert!(weakening_environment("LOCALHOLD_MAINTAINABILITY_BASE_REV=$GITHUB_SHA"));
    assert!(weakening_environment_for_surface("script/check.ps1", "$env:rustflags = $dynamic"));
    assert!(weakening_environment_for_surface("script/check.cmd", "set cargo_encoded_rustflags=%DYNAMIC%"));
    assert!(!weakening_environment_for_surface("script/check.sh", "rustflags=local"));
    assert!(!weakening_environment("rustc --version"));
    let scrubber = format!("{}\n", BOOTSTRAP_ENVIRONMENT_LINES.join("\n"));
    assert!(scrubber_environment_references_are_exact("script/check-maintainability-bootstrap.sh", &scrubber));
    assert!(!scrubber_environment_references_are_exact(
        "script/check-maintainability-bootstrap.sh",
        &format!("{scrubber}RUSTFLAGS='-A warnings'\n"),
    ));
    assert!(!scrubber_environment_references_are_exact(
        "script/check-maintainability-bootstrap.sh",
        &BOOTSTRAP_ENVIRONMENT_LINES[1..].join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact(
        "script/tests/test_maintainability_bootstrap.sh",
        &BOOTSTRAP_TEST_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact("mise.toml", &MISE_ENVIRONMENT_LINES.join("\n")));
    assert!(scrubber_environment_references_are_exact(
        ".github/workflows/ci.yml",
        &CI_REVISION_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(!scrubber_environment_references_are_exact(
        ".github/workflows/ci.yml",
        &CI_REVISION_ENVIRONMENT_LINES[..1].join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact(
        ".github/workflows/gpu-release-gate.yml",
        &GPU_RELEASE_REVISION_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(!scrubber_environment_references_are_exact("script/tests/new-command.sh", &scrubber));
}

#[test]
fn alternate_clippy_configuration_is_rejected_beside_nested_packages() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("tools/checker")).expect("nested package");
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='root'\nversion='0.1.0'\n").expect("root manifest");
    fs::write(workspace.path().join("tools/checker/Cargo.toml"), "[package]\nname='checker'\nversion='0.1.0'\n").expect("nested manifest");
    fs::write(workspace.path().join("clippy.toml"), "").expect("root Clippy configuration");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    reject_alternate_clippy_configuration(workspace.path()).expect("one canonical Clippy configuration");

    fs::write(workspace.path().join("tools/.clippy.toml"), "").expect("alternate Clippy configuration");
    assert!(
        reject_alternate_clippy_configuration(workspace.path())
            .unwrap_err()
            .to_string()
            .contains("alternate Clippy configuration")
    );
}

#[test]
fn command_surfaces_include_scripts_outside_the_legacy_script_directory() {
    for path in [
        "Justfile",
        "justfile",
        ".JUSTFILE",
        "module.just",
        ".github/workflows/ci.yml",
        ".github/actions/check/action.yaml",
        ".cargo/config",
        "nested/.cargo/config.toml",
        "nested/.CARGO/CONFIG.TOML",
        "script/release.py",
        "tools/ci/check.sh",
        "tools/ci/check.PS1",
        "Makefile",
        "package.json",
    ] {
        assert!(is_execution_surface(path), "missing command surface {path}");
    }
    assert!(!is_execution_surface("CONTRIBUTING.md"));
    assert!(!is_execution_surface("src/lib.rs"));
}

#[test]
fn command_policy_rejects_cargo_configuration_relocation() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(workspace.path().join("script/check.sh"), "cargo check\n").expect("safe command");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    reject_checked_in_weakening(workspace.path()).expect("safe Cargo command");

    fs::create_dir_all(workspace.path().join(".cargo")).expect("Cargo configuration directory");
    fs::write(workspace.path().join(".cargo/config.toml"), "[build]\nrustflags = ['-A', 'warnings']\n").expect("Cargo configuration");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("Cargo configuration"));
    fs::remove_dir_all(workspace.path().join(".cargo")).expect("remove Cargo configuration");

    fs::write(workspace.path().join("script/check.sh"), "CARGO_HOME=$DYNAMIC_HOME cargo check\n").expect("Cargo home injection");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("environment channel"));

    fs::write(workspace.path().join("script/check.sh"), "cargo -Z unstable-options -C ../other check\n").expect("Cargo directory relocation");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"));
}

#[test]
fn clippy_policy_requires_auditable_metadata() {
    let mut setting = clippy_setting();
    validate_clippy_configuration(&ClippyConfigurationFile {
        schema_version: 1,
        entries: vec![setting.clone()],
    })
    .expect("complete setting");
    setting.safety_invariant.clear();
    assert!(
        validate_clippy_configuration(&ClippyConfigurationFile {
            schema_version: 1,
            entries: vec![setting],
        })
        .is_err()
    );
}

fn clippy_setting() -> ClippySetting {
    ClippySetting {
        id: "clippy.threshold".to_owned(),
        key: "too-many-lines-threshold".to_owned(),
        constraint: ClippyConstraint::MaximumInteger { value: 75 },
        owner: "maintainers".to_owned(),
        issue: "issue".to_owned(),
        pull_request: "pull request".to_owned(),
        rationale: "visible debt".to_owned(),
        safety_invariant: "cannot rise".to_owned(),
        alternatives_considered: "default rejected".to_owned(),
        sentinel: "Clippy".to_owned(),
        evidence: "inventory".to_owned(),
        re_review_phase: "Phase 1".to_owned(),
    }
}

fn git(workspace: &Path, arguments: &[&str]) {
    let status = Command::new("git").current_dir(workspace).args(arguments).status().expect("run git");
    assert!(status.success());
}
