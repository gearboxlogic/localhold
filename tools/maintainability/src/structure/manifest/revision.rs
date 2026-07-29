use std::collections::BTreeSet;
use std::env;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::model::StructureManifest;
use super::validate::{validate_relative_rust_path, validate_revision};
use crate::structure::MANIFEST_PATH;
use crate::structure::classify::{self, Inventory};

const BASE_REVISION_ENV: &str = "LOCALHOLD_MAINTAINABILITY_BASE_REV";

#[derive(Debug)]
pub(in crate::structure) struct PreviousRevision {
    pub(in crate::structure) manifest: StructureManifest,
    pub(in crate::structure) inventory: Inventory,
}

impl StructureManifest {
    pub fn compare_previous_revision(&self, workspace: &Path, current_inventory: &Inventory) -> Result<Option<PreviousRevision>> {
        self.compare_previous_revision_from(workspace, env::var(BASE_REVISION_ENV).ok().as_deref(), current_inventory)
    }

    pub(super) fn compare_previous_revision_from(&self, workspace: &Path, revision: Option<&str>, current_inventory: &Inventory) -> Result<Option<PreviousRevision>> {
        let Some(revision) = revision else {
            return Ok(None);
        };
        if revision.is_empty() || revision.len() == 40 && revision.bytes().all(|byte| byte == b'0') {
            return Ok(None);
        }
        validate_revision(revision).context("validate maintainability base revision")?;
        let object = format!("{revision}:{MANIFEST_PATH}");
        let output = Command::new("git")
            .current_dir(workspace)
            .args(["show", "--no-ext-diff", &object])
            .output()
            .context("read structure policy from maintainability base revision")?;
        if !output.status.success() {
            verify_initial_policy_revision(workspace, revision, &object)?;
            return Ok(None);
        }

        let previous: Self = serde_json::from_slice(&output.stdout).context("parse structure policy from maintainability base revision")?;
        previous.validate_previous().context("validate structure policy from maintainability base revision")?;
        let previous_inventory = classify::scan_revision(workspace, revision, &previous.tracked_roots)?;
        previous
            .compare_current(&previous_inventory)
            .context("verify structure policy evidence from maintainability base revision")?;
        let touched = changed_rust_paths(workspace, revision, &self.tracked_roots)?;
        self.compare_policy_with_touched(&previous, &previous_inventory, current_inventory, &touched)?;
        Ok(Some(PreviousRevision {
            manifest: previous,
            inventory: previous_inventory,
        }))
    }
}

fn changed_rust_paths(workspace: &Path, revision: &str, roots: &[String]) -> Result<BTreeSet<String>> {
    let diff = Command::new("git")
        .current_dir(workspace)
        .args(["diff", "--no-ext-diff", "--no-renames", "--name-only", "-z", revision, "--"])
        .args(roots)
        .output()
        .context("list Rust paths changed from maintainability base revision")?;
    if !diff.status.success() {
        bail!("git diff failed while listing changed structural paths");
    }
    let untracked = Command::new("git")
        .current_dir(workspace)
        .args(["ls-files", "--others", "--exclude-standard", "-z", "--"])
        .args(roots)
        .output()
        .context("list untracked Rust paths")?;
    if !untracked.status.success() {
        bail!("git ls-files failed while listing untracked structural paths");
    }
    let mut paths = BTreeSet::new();
    collect_rust_paths(&diff.stdout, &mut paths)?;
    collect_rust_paths(&untracked.stdout, &mut paths)?;
    Ok(paths)
}

fn collect_rust_paths(output: &[u8], paths: &mut BTreeSet<String>) -> Result<()> {
    for raw in output.split(|byte| *byte == b'\0').filter(|path| !path.is_empty()) {
        let path = std::str::from_utf8(raw).context("changed structural path is not UTF-8")?;
        if Path::new(path).extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        validate_relative_rust_path(path)?;
        paths.insert(path.to_owned());
    }
    Ok(())
}

fn verify_initial_policy_revision(workspace: &Path, revision: &str, object: &str) -> Result<()> {
    let status = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("verify maintainability base revision")?;
    if !status.success() {
        bail!("maintainability base revision {revision} is not available");
    }
    let status = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "-e", object])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("inspect structure policy in maintainability base revision")?;
    if status.success() {
        bail!("structure policy exists in base revision but could not be read");
    }
    Ok(())
}

#[cfg(test)]
mod touched_tests {
    use std::fs;

    use super::*;

    #[test]
    fn changed_paths_include_same_line_count_edits_and_untracked_rust() {
        let repository = tempfile::tempdir().expect("temporary repository");
        fs::create_dir(repository.path().join("src")).expect("source directory");
        fs::write(repository.path().join("src/lib.rs"), "fn old() {}\n").expect("baseline source");
        fs::write(repository.path().join("src/note.md"), "old\n").expect("baseline note");
        git(repository.path(), &["init", "-q"]);
        git(repository.path(), &["add", "."]);
        git(
            repository.path(),
            &["-c", "user.name=LocalHold", "-c", "user.email=localhold@example.invalid", "commit", "-q", "-m", "baseline"],
        );
        let revision = String::from_utf8(git_output(repository.path(), &["rev-parse", "HEAD"]))
            .expect("UTF-8 revision")
            .trim()
            .to_owned();

        fs::write(repository.path().join("src/lib.rs"), "fn new() {}\n").expect("same-line-count source edit");
        fs::write(repository.path().join("src/new.rs"), "fn added() {}\n").expect("untracked Rust source");
        fs::write(repository.path().join("src/note.md"), "new\n").expect("non-Rust edit");

        let changed = changed_rust_paths(repository.path(), &revision, &["src".to_owned()]).expect("changed Rust paths");
        assert_eq!(changed, BTreeSet::from(["src/lib.rs".to_owned(), "src/new.rs".to_owned()]));
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
}
