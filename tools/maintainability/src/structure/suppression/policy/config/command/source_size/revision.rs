use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use super::model::{POLICY_PATH, ToolingStructureManifest};

pub(super) fn compare_previous(workspace: &Path, current: &ToolingStructureManifest) -> Result<()> {
    let Some(revision) = crate::structure::revision::maintainability_base_revision()? else {
        return Ok(());
    };
    compare_previous_at_revision(workspace, current, &revision)
}

fn compare_previous_at_revision(workspace: &Path, current: &ToolingStructureManifest, revision: &str) -> Result<()> {
    let object = format!("{revision}:{POLICY_PATH}");
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["-c", "core.attributesFile=/dev/null", "show", "--no-ext-diff", "--no-textconv", &object])
        .output()
        .context("read maintainability tooling structure policy from base revision")?;
    if !output.status.success() {
        verify_initial_policy_revision(workspace, revision)?;
        return Ok(());
    }
    let previous = ToolingStructureManifest::parse(&output.stdout).context("validate maintainability tooling structure policy from base revision")?;
    current.compare_previous(&previous)
}

fn verify_initial_policy_revision(workspace: &Path, revision: &str) -> Result<()> {
    let commit = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("verify maintainability tooling policy base revision")?;
    if !commit.success() {
        bail!("maintainability base revision {revision} is not available");
    }
    let tree_entry = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-tree", "-z", "--full-tree", revision, "--", POLICY_PATH])
        .output()
        .context("inspect maintainability tooling policy in base revision")?;
    if !tree_entry.status.success() {
        bail!("cannot inspect maintainability tooling structure policy in base revision {revision}");
    }
    if !tree_entry.stdout.is_empty() {
        bail!("maintainability tooling structure policy exists in the base revision but could not be read");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;

    fn policy(component: usize, hotspot: usize) -> Vec<u8> {
        format!(
            r#"{{
                "schema_version": 1,
                "root_manifest": "tools/maintainability/Cargo.toml",
                "source_root": "tools/maintainability/src",
                "limits": {{"production_file_physical_lines": 800, "test_file_physical_lines": 1000}},
                "component": {{"id": "maintainability-analyzer", "physical_ceiling": {component}}},
                "source_profile": {{
                    "current_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "preapproved_next_sha256": null,
                    "retired_sha256": []
                }},
                "hotspots": [{{
                    "id": "tooling.large",
                    "path": "tools/maintainability/src/large.rs",
                    "status": "active",
                    "physical_ceiling": {hotspot},
                    "issue": "https://example.invalid/1",
                    "rationale": "Legacy analyzer hotspot."
                }}]
            }}"#
        )
        .into_bytes()
    }

    fn git(workspace: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git").current_dir(workspace).args(arguments).output().expect("run Git");
        assert!(output.status.success(), "git {arguments:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).expect("Git output").trim().to_owned()
    }

    fn initialize() -> tempfile::TempDir {
        let workspace = tempfile::tempdir().expect("temporary repository");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["config", "user.name", "LocalHold"]);
        git(workspace.path(), &["config", "user.email", "localhold@example.invalid"]);
        workspace
    }

    fn commit(workspace: &Path, bytes: Option<&[u8]>) -> String {
        if let Some(bytes) = bytes {
            let target = workspace.join(POLICY_PATH);
            fs::create_dir_all(target.parent().expect("policy parent")).expect("policy directory");
            fs::write(target, bytes).expect("policy");
        } else {
            fs::write(workspace.join("README"), "initial\n").expect("initial file");
        }
        git(workspace, &["add", "."]);
        git(workspace, &["commit", "-qm", "fixture"]);
        git(workspace, &["rev-parse", "HEAD"])
    }

    #[test]
    fn existing_git_base_policy_is_compared_and_growth_is_rejected() {
        let workspace = initialize();
        let base = commit(workspace.path(), Some(&policy(900, 900)));
        let unchanged = ToolingStructureManifest::parse(&policy(900, 900)).expect("unchanged policy");
        compare_previous_at_revision(workspace.path(), &unchanged, &base).expect("compare existing Git policy");

        let growth = ToolingStructureManifest::parse(&policy(901, 901)).expect("growth policy");
        let error = compare_previous_at_revision(workspace.path(), &growth, &base).unwrap_err();
        assert!(error.to_string().contains("cannot increase"), "{error:#}");
    }

    #[test]
    fn malformed_existing_git_policy_fails_closed() {
        let workspace = initialize();
        let base = commit(workspace.path(), Some(b"not json"));
        let current = ToolingStructureManifest::parse(&policy(900, 900)).expect("current policy");
        let error = compare_previous_at_revision(workspace.path(), &current, &base).unwrap_err();
        assert!(error.to_string().contains("validate maintainability tooling structure policy"), "{error:#}");
    }

    #[test]
    fn initial_creation_requires_an_existing_base_without_the_policy_path() {
        let workspace = initialize();
        let base = commit(workspace.path(), None);
        let current = ToolingStructureManifest::parse(&policy(900, 900)).expect("current policy");
        compare_previous_at_revision(workspace.path(), &current, &base).expect("initial policy creation");

        let error = compare_previous_at_revision(workspace.path(), &current, "0000000000000000000000000000000000000000").unwrap_err();
        assert!(error.to_string().contains("not available"), "{error:#}");
    }
}
