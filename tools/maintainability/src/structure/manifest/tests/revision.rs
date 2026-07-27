use std::fs;
use std::path::Path;
use std::process::Command;

use super::support::{file_exception, inventory, ordinary_manifest};
use crate::structure::manifest::model::FileExceptionKind;

#[test]
fn previous_revision_selection_handles_absent_null_and_initial_policy_inputs() {
    let manifest = ordinary_manifest("src/lib.rs", 10, 10);
    let current = inventory(&[("src/lib.rs", 10, 10)]);
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
        .compare_previous_revision_from(repository.path(), None, &current)
        .expect("unset revision is intentionally absent");
    manifest
        .compare_previous_revision_from(repository.path(), Some(""), &current)
        .expect("empty revision is intentionally absent");
    manifest
        .compare_previous_revision_from(repository.path(), Some("0000000000000000000000000000000000000000"), &current)
        .expect("null revision is intentionally absent");
    manifest
        .compare_previous_revision_from(repository.path(), Some(&revision), &current)
        .expect("existing commit without a policy is the initial policy revision");
    assert!(
        manifest
            .compare_previous_revision_from(repository.path(), Some("ffffffffffffffffffffffffffffffffffffffff"), &current,)
            .unwrap_err()
            .to_string()
            .contains("is not available")
    );
}

#[test]
fn current_schema_cannot_downgrade_but_previous_schema_generations_are_readable() {
    let mut evolution_schema = ordinary_manifest("src/lib.rs", 10, 10);
    evolution_schema.schema_version = 2;
    evolution_schema.validate_previous().expect("immediate previous schema is readable");
    assert!(evolution_schema.validate_current().is_err());

    let mut legacy = ordinary_manifest("src/lib.rs", 10, 10);
    legacy.schema_version = 1;
    legacy.validate_previous().expect("original structure schema remains readable");
    assert!(legacy.validate_current().is_err());

    legacy.path_evolutions.push(super::support::evolution(
        "history.invalid",
        crate::structure::manifest::model::PathEvolutionKind::Rename,
        &["src/lib.rs"],
        &["src/new.rs"],
    ));
    assert!(legacy.validate_previous().unwrap_err().to_string().contains("cannot contain evolution ledgers"));

    evolution_schema
        .file_exceptions
        .push(file_exception("history.invalid", "src/lib.rs", FileExceptionKind::ProductionCohesive, 900));
    assert!(evolution_schema.validate_previous().unwrap_err().to_string().contains("before version 3"));
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
