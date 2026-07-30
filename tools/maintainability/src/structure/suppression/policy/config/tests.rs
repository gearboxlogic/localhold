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
fn cargo_allow_scan_resolves_workspace_lint_inheritance() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("member")).expect("workspace member");
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers=['member']\n\n[workspace.lints.rust]\nunsafe_code='forbid'\nwarnings='allow'\n",
    )
    .expect("workspace manifest");
    fs::write(
        workspace.path().join("member/Cargo.toml"),
        "[package]\nname='member'\nversion='0.1.0'\n\n[lints]\nworkspace=true\n",
    )
    .expect("member manifest");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    assert_eq!(
        scan_cargo_allows(workspace.path()).expect("inherited Cargo allowances"),
        BTreeSet::from([("member/Cargo.toml".to_owned(), "rust".to_owned(), "warnings".to_owned())])
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
    assert!(weakening_token("cargo rustc -- \"--cap-\"lints allow"));
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
    assert!(weakening_token_for_surface("script/check.cmd", "CARGO.EXE clippy -- -A warnings"));
    assert!(weakening_token_for_surface("script/check.ps1", "Rustc.ExE -W warnings source.rs"));
    assert!(!weakening_token_for_surface("script/check.sh", "CARGO.EXE clippy -- -A warnings"));
    assert!(weakening_token("rustc @policy/lints.args source.rs"));
    assert!(weakening_token("rustc -D warnings --allow=warnings source.rs"));
    assert!(weakening_token("rustc -D warnings --warn=warnings source.rs"));
    assert!(weakening_token("rustc -D warnings --force-warn=unused_variables source.rs"));
    assert!(weakening_token("LABEL=@not-a-response rustc @policy/lints.args source.rs"));
    assert!(weakening_token("run_cargo() { cargo \"$@\"; }\nrun_cargo clippy -- -A warnings"));
    assert!(weakening_token("cargo rustc -- @policy/lints.args"));
    assert!(!weakening_token("cargo run -- @application-argument"));
    assert!(weakening_token("sh -c \"cargo --config net.offline=true check # literal\""));
    assert!(!weakening_token("cargo deny --config deny.toml"));
    assert!(!weakening_token("gitleaks --config policy.toml # cargo output"));
    assert!(!weakening_token("cargo build\ncc -Wall -Wextra"));
    assert!(!weakening_token("hold doctor --allow-downloads"));
}

#[test]
fn powershell_continuations_cannot_split_lint_arguments_from_cargo() {
    assert!(weakening_token_for_surface("script/check.ps1", "cargo clippy -- `\r\n  -A warnings"));
    assert!(weakening_token_for_surface("script/check.ps1", "cargo clippy -- `\n  --allow=warnings"));
}

#[test]
fn folded_yaml_commands_are_scanned_as_executed() {
    assert!(weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - run: >-\n      cargo clippy --\n      -A warnings\n"
    ));
    assert!(weakening_token_for_surface(
        ".github/actions/check/action.yaml",
        "runs:\n  steps:\n    - 'run': > # folded command\n        cargo clippy --\n        --allow warnings\n"
    ));
    assert!(!weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - run: >\n      cargo build\n\n      echo application -A argument\n"
    ));
    assert!(weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "jobs:\n  windows:\n    runs-on: windows-latest\n    steps:\n      - run: CARGO.EXE clippy -- -A warnings\n"
    ));
}

#[test]
fn weakening_environment_channels_are_detected() {
    assert!(weakening_environment("export RUSTFLAGS='-A warnings'\nexec \"$CHECK\""));
    assert!(weakening_environment("export RUST''FLAGS='--cap-lints allow'"));
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
    assert!(weakening_environment("MISE_OVERRIDE_CONFIG_FILENAMES=policy.toml"));
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
        ".mise.toml",
        "mise.development.toml",
        ".mise.windows.local.toml",
        "mise/config.local.toml",
        ".mise/config.production.toml",
        ".config/mise/config.ci.local.toml",
        ".config/mise/conf.d/quality.toml",
        ".CONFIG/MISE/CONF.D/QUALITY.TOML",
        ".rtx.toml",
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

    fs::remove_file(workspace.path().join("script/check.sh")).expect("delete command surface");
    reject_checked_in_weakening(workspace.path()).expect("deleted command surfaces are absent");
}

#[test]
fn command_policy_scans_extensionless_scripts() {
    for (source, executable) in [("cargo clippy -- -A warnings\n", true), ("#!/bin/sh\ncargo clippy -- -A warnings\n", false)] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join("tools")).expect("tool directory");
        fs::write(workspace.path().join("tools/run-lints"), source).expect("extensionless lint script");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", "."]);
        if executable {
            git(workspace.path(), &["update-index", "--chmod=+x", "tools/run-lints"]);
        }

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"));
    }
}

#[test]
fn command_policy_rejects_sourced_environment_files() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("policy")).expect("policy directory");
    fs::write(workspace.path().join("Justfile"), "check:\n    . policy/lints.env; cargo clippy -- -D warnings\n").expect("sourced lint environment");
    fs::write(workspace.path().join("policy/lints.env"), "export RUSTFLAGS=--cap-lints=allow\n").expect("lint environment");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("sourced-file indirection"));
}

#[test]
fn yaml_source_labels_are_not_shell_indirection() {
    assert!(!has_sourced_file_indirection(
        ".github/workflows/ci.yml",
        "steps:\n  - name: Restore source cache\n    run: cargo clippy\n"
    ));
    assert!(!has_sourced_file_indirection("script/install.sh", "Builds LocalHold from the locked source tree\n"));
    assert!(has_sourced_file_indirection(
        "script/check.sh",
        "if MODE=strict source policy/lints.env; then cargo clippy; fi\n"
    ));
    assert!(has_sourced_file_indirection(
        ".github/workflows/ci.yml",
        "steps:\n  - run: |\n      . policy/lints.env\n      cargo clippy\n"
    ));
    assert!(has_sourced_file_indirection(".github/workflows/ci.yml", "steps:\n  - run: source policy/lints.env\n"));
}

#[test]
fn command_policy_discovers_directly_compiled_rust_sources() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(workspace.path().join("script/check.sh"), "rustc -D warnings script/check.rs\n").expect("direct compiler command");
    fs::write(workspace.path().join("script/check.rs"), "fn main() {}\n").expect("direct Rust source");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let sources = reject_checked_in_weakening(workspace.path()).expect("non-weakening direct compiler command");
    assert_eq!(sources, BTreeSet::from(["script/check.rs".to_owned()]));

    fs::write(workspace.path().join("script/check.sh"), "rustc --version\n").expect("compiler version command");
    assert!(reject_checked_in_weakening(workspace.path()).expect("informational compiler command").is_empty());

    fs::write(workspace.path().join("script/check.sh"), "rustc \"$DIRECT_SOURCE\"\n").expect("opaque direct compiler command");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("without auditable repository-relative .rs inputs"));
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
