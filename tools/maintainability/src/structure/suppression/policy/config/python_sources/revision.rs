use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use super::POLICY_PATH;
use super::profile::ProfilePolicy;

pub(super) fn compare_previous(workspace: &Path, current: &ProfilePolicy) -> Result<()> {
    let Some(revision) = crate::structure::revision::maintainability_base_revision()? else {
        return Ok(());
    };
    compare_at_revision(workspace, current, &revision)
}

fn compare_at_revision(workspace: &Path, current: &ProfilePolicy, revision: &str) -> Result<()> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args([
            "-c",
            "core.attributesFile=/dev/null",
            "show",
            "--no-ext-diff",
            "--no-textconv",
            &format!("{revision}:{POLICY_PATH}"),
        ])
        .output()
        .context("read Python source profile policy from base revision")?;
    if !output.status.success() {
        verify_initial_policy_revision(workspace, revision)?;
        return Ok(());
    }
    let previous = ProfilePolicy::parse(&output.stdout).context("validate Python source profile policy from base revision")?;
    current.compare_previous(&previous)
}

fn verify_initial_policy_revision(workspace: &Path, revision: &str) -> Result<()> {
    let commit = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("verify Python source profile base revision")?;
    if !commit.success() {
        bail!("maintainability base revision {revision} is not available");
    }
    let entry = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-tree", "-z", "--full-tree", revision, "--", POLICY_PATH])
        .output()
        .context("inspect Python source profile policy in base revision")?;
    if !entry.status.success() {
        bail!("cannot inspect Python source profile policy in base revision {revision}");
    }
    if !entry.stdout.is_empty() {
        bail!("Python source profile policy exists in the base revision but could not be read");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn policy(current: &str, next: Option<&str>, retired: &[&str]) -> Vec<u8> {
        let next = next.map_or_else(|| "null".to_owned(), |digest| format!("\"{digest}\""));
        let retired = retired.iter().map(|digest| format!("\"{digest}\"")).collect::<Vec<_>>().join(",");
        format!(
            r#"{{"schema_version":1,"id":"python-source-tree","current_sha256":"{current}","preapproved_next_sha256":{next},"retired_sha256":[{retired}],"issue":"https://example.invalid/1","rationale":"Atomic Python source review.","safety_invariant":"Only staged complete profiles may land."}}"#
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
    fn base_policy_transition_is_enforced() {
        let workspace = initialize();
        let base = commit(workspace.path(), Some(&policy(A, Some(B), &[])));
        let promoted = ProfilePolicy::parse(&policy(B, None, &[A])).expect("promoted policy");
        compare_at_revision(workspace.path(), &promoted, &base).expect("promote staged profile");

        let unstaged = ProfilePolicy::parse(&policy(B, None, &[])).expect("unstaged policy");
        assert!(compare_at_revision(workspace.path(), &unstaged, &base).is_err());
    }

    #[test]
    fn initial_creation_requires_an_existing_base_without_the_policy() {
        let workspace = initialize();
        let base = commit(workspace.path(), None);
        let current = ProfilePolicy::parse(&policy(A, None, &[])).expect("current policy");
        compare_at_revision(workspace.path(), &current, &base).expect("initial policy creation");
        assert!(compare_at_revision(workspace.path(), &current, "0000000000000000000000000000000000000000").is_err());
    }
}
