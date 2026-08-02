use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::action_inputs;

pub(super) const DEPENDENCY_REVIEW_ACTION: &str = "actions/dependency-review-action@a1d282b36b6f3519aa1f3fc636f609c47dddb294";
pub(super) const MISE_ACTION: &str = "jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d";

const DEPENDENCY_REVIEW_PATH: &str = ".github/workflows/dependency-review.yml";
const PR_CLASSIFICATION_PATH: &str = ".github/workflows/pr-classification.yml";
const PR_CLASSIFICATION_SHA256: &str = "63a8a0a43170256ccecf81757b5844c97c320e063cdb6865ae7e9dd9f4b26e8b";
const MISE_VERSION: &str = "2026.7.5";
const REVIEWED_MISE_PROFILES: &[(&str, &str)] = &[(
    "627903d61cd155a318e0dffa4a29052099fbed1834bd485e7859fdcad03c0529",
    "24a3c64cbd2123ba9ab457eba21a65c7960d189d6685fe1d2bfd4a979134c358",
)];
// Stage intentional changes by adding the next complete profile beside the
// current one, merging that checker ratchet, and only then changing the
// guarded files. Remove the obsolete profile in a final cleanup change.
const REVIEWED_DEPENDENCY_REVIEW_WORKFLOWS: &[&str] = &[r"name: Dependency Review

on:
  pull_request:
    branches: [main]

permissions:
  contents: read

jobs:
  dependency-review:
    name: Dependency Review
    if: ${{ !github.event.repository.private }}
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7.0.0
        with:
          persist-credentials: false

      - uses: actions/dependency-review-action@a1d282b36b6f3519aa1f3fc636f609c47dddb294 # v5.0.0"];

pub(super) fn validate_configuration(workspace: &Path, tracked_paths: &BTreeSet<String>) -> Result<()> {
    let mise_toml = reviewed_file(workspace, tracked_paths, "mise.toml")?;
    let mise_lock = reviewed_file(workspace, tracked_paths, "mise.lock")?;
    let profile = (digest(&mise_toml), digest(&mise_lock));
    if !REVIEWED_MISE_PROFILES.iter().any(|reviewed| reviewed.0 == profile.0 && reviewed.1 == profile.1) {
        bail!("mise.toml and mise.lock must match one complete reviewed tool profile before CI may activate Mise");
    }

    let dependency_review = reviewed_file(workspace, tracked_paths, DEPENDENCY_REVIEW_PATH)?;
    let dependency_review = std::str::from_utf8(&dependency_review).context("dependency-review workflow must be UTF-8")?;
    if !REVIEWED_DEPENDENCY_REVIEW_WORKFLOWS.contains(&dependency_review.trim_end()) {
        bail!("{DEPENDENCY_REVIEW_PATH:?} must retain its exact reviewed trigger, permissions, job controls, checkout, and dependency-review step");
    }
    // The bridge permits introduction only at this exact digest. The feature
    // slice makes the file mandatory as it lands, closing the absence window.
    if tracked_paths.contains(PR_CLASSIFICATION_PATH) {
        let classification = reviewed_file(workspace, tracked_paths, PR_CLASSIFICATION_PATH)?;
        if digest(&classification) != PR_CLASSIFICATION_SHA256 {
            bail!("{PR_CLASSIFICATION_PATH:?} must match the reviewed required-check workflow before it can be introduced");
        }
    }
    Ok(())
}

pub(super) fn validate_mise_action(path: &str, lines: &[&str], uses_index: usize) -> Result<()> {
    let inputs = action_inputs(path, lines, uses_index)?;
    if inputs.len() != 1 || inputs[0].key != "version" || inputs[0].literal(path)? != MISE_VERSION {
        bail!("Mise activation in {path:?} must use only the exact reviewed version {MISE_VERSION:?}");
    }
    Ok(())
}

pub(super) fn validate_dependency_review_reference(path: &str) -> Result<()> {
    if path != DEPENDENCY_REVIEW_PATH {
        bail!("the dependency-review action may run only in the exact guarded workflow {DEPENDENCY_REVIEW_PATH:?}");
    }
    Ok(())
}

fn reviewed_file(workspace: &Path, tracked_paths: &BTreeSet<String>, path: &str) -> Result<Vec<u8>> {
    if !tracked_paths.contains(path) {
        bail!("guarded CI input {path:?} must be tracked");
    }
    let absolute = workspace.join(path);
    let metadata = fs::symlink_metadata(&absolute).with_context(|| format!("inspect guarded CI input {path:?}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("guarded CI input {path:?} must be a regular non-symlink file");
    }
    fs::read(absolute).with_context(|| format!("read guarded CI input {path:?}"))
}

fn digest(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked_paths() -> BTreeSet<String> {
        ["mise.toml", "mise.lock", DEPENDENCY_REVIEW_PATH].into_iter().map(str::to_owned).collect()
    }

    fn fixture() -> tempfile::TempDir {
        let fixture = tempfile::tempdir().expect("temp fixture");
        fs::create_dir_all(fixture.path().join(".github/workflows")).expect("workflow directory");
        for path in ["mise.toml", "mise.lock", DEPENDENCY_REVIEW_PATH] {
            fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path), fixture.path().join(path)).expect("copy guarded input");
        }
        fixture
    }

    #[test]
    fn guarded_configuration_accepts_the_reviewed_files() {
        let fixture = fixture();
        validate_configuration(fixture.path(), &tracked_paths()).expect("reviewed configuration");
    }

    #[test]
    fn mise_tool_inventory_and_lockfile_are_one_reviewed_profile() {
        for path in ["mise.toml", "mise.lock"] {
            let fixture = fixture();
            fs::write(fixture.path().join(path), b"unreviewed\n").expect("alter guarded input");
            assert!(validate_configuration(fixture.path(), &tracked_paths()).is_err(), "accepted altered {path}");
        }
    }

    #[test]
    fn dependency_review_control_flow_is_exact() {
        for alteration in [
            "      - if: ${{ false }}\n        uses: actions/dependency-review-action",
            "      - continue-on-error: true\n        uses: actions/dependency-review-action",
            "      - run: cp quality/Justfile Justfile\n\n      - uses: actions/dependency-review-action",
        ] {
            let fixture = fixture();
            let path = fixture.path().join(DEPENDENCY_REVIEW_PATH);
            let source = fs::read_to_string(&path).expect("read workflow");
            fs::write(&path, source.replace("      - uses: actions/dependency-review-action", alteration)).expect("alter workflow");
            assert!(validate_configuration(fixture.path(), &tracked_paths()).is_err(), "accepted {alteration:?}");
        }
    }

    #[test]
    fn staged_classification_workflow_rejects_an_unreviewed_profile() {
        let fixture = fixture();
        fs::write(fixture.path().join(PR_CLASSIFICATION_PATH), b"unreviewed\n").expect("classification workflow fixture");
        let mut paths = tracked_paths();
        paths.insert(PR_CLASSIFICATION_PATH.to_owned());
        assert!(validate_configuration(fixture.path(), &paths).is_err());
    }

    #[test]
    fn mise_action_requires_only_the_reviewed_version() {
        for (inputs, accepted) in [
            ("version: 2026.7.5", true),
            ("version: latest", false),
            ("version: 2026.7.5\n      install: false", false),
            ("version: 2026.7.5\n      version: 2026.7.5", false),
        ] {
            let source = format!("steps:\n  - uses: {MISE_ACTION}\n    with:\n      {inputs}\n");
            let lines = source.lines().collect::<Vec<_>>();
            assert_eq!(validate_mise_action(".github/workflows/test.yml", &lines, 1).is_ok(), accepted, "{inputs:?}");
        }
    }
}
