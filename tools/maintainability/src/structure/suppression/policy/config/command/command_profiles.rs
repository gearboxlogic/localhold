use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::arguments::reviewed_command_profiles;
use super::profile_policy::{POLICY_PATH, ProfileManifest};

pub(super) fn validate(workspace: &Path, checked_paths: &BTreeSet<String>) -> Result<Option<ProfileManifest>> {
    if !checked_paths.contains(POLICY_PATH) {
        return Ok(None);
    }
    require_regular_file(workspace, POLICY_PATH, "reviewed command profile policy")?;
    let bytes = fs::read(workspace.join(POLICY_PATH)).context("read reviewed command profile policy")?;
    let policy = ProfileManifest::parse(&bytes)?;
    let reviewed = reviewed_command_profiles();
    let governed = policy
        .profiles()
        .iter()
        .map(|profile| (profile.id.as_str(), profile.path.as_str()))
        .collect::<BTreeSet<_>>();
    if governed != reviewed {
        bail!("reviewed command profile policy identities and paths differ from the exact argument-profile inventory");
    }
    for profile in policy.profiles() {
        if !checked_paths.contains(&profile.path) {
            bail!("reviewed command profile source must remain tracked: {:?}", profile.path);
        }
        require_regular_file(workspace, &profile.path, "reviewed command profile source")?;
        let source = fs::read(workspace.join(&profile.path)).with_context(|| format!("read reviewed command profile source {:?}", profile.path))?;
        let observed = format!("{:x}", Sha256::digest(source));
        if observed != profile.current_sha256 {
            bail!(
                "reviewed command profile {:?} current digest does not match its complete checked-in source; stage a preapproved successor in a prior pull request",
                profile.id
            );
        }
    }
    compare_previous(workspace, &policy)?;
    Ok(Some(policy))
}

fn require_regular_file(workspace: &Path, path: &str, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(workspace.join(path)).with_context(|| format!("inspect {label} {path:?}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} {path:?} must be a regular non-symlink file");
    }
    Ok(())
}

fn compare_previous(workspace: &Path, current: &ProfileManifest) -> Result<()> {
    let Some(revision) = crate::structure::revision::maintainability_base_revision()? else {
        return Ok(());
    };
    compare_previous_at_revision(workspace, current, &revision)
}

fn compare_previous_at_revision(workspace: &Path, current: &ProfileManifest, revision: &str) -> Result<()> {
    let object = format!("{revision}:{POLICY_PATH}");
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["-c", "core.attributesFile=/dev/null", "show", "--no-ext-diff", "--no-textconv", &object])
        .output()
        .context("read reviewed command profile policy from base revision")?;
    if !output.status.success() {
        verify_initial_policy_revision(workspace, revision)?;
        return Ok(());
    }
    let previous = ProfileManifest::parse(&output.stdout).context("validate reviewed command profile policy from base revision")?;
    current.compare_previous(&previous)
}

fn verify_initial_policy_revision(workspace: &Path, revision: &str) -> Result<()> {
    let commit = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("verify reviewed command profile base revision")?;
    if !commit.success() {
        bail!("maintainability base revision {revision} is not available");
    }
    let entry = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-tree", "-z", "--full-tree", revision, "--", POLICY_PATH])
        .output()
        .context("inspect reviewed command profile policy in base revision")?;
    if !entry.status.success() {
        bail!("cannot inspect reviewed command profile policy in base revision {revision}");
    }
    if !entry.stdout.is_empty() {
        bail!("reviewed command profile policy exists in the base revision but could not be read");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn policy(current: &str, next: Option<&str>, retired: &[&str]) -> Vec<u8> {
        let next = next.map_or_else(|| "null".to_owned(), |digest| format!("\"{digest}\""));
        let retired = retired.iter().map(|digest| format!("\"{digest}\"")).collect::<Vec<_>>().join(",");
        format!(
            r#"{{"schema_version":1,"profiles":[{{"id":"reviewed","path":"script/reviewed.sh","current_sha256":"{current}","preapproved_next_sha256":{next},"retired_sha256":[{retired}],"issue":"https://example.invalid/1","rationale":"Reviewed cleanup.","safety_invariant":"Exact arguments and complete source are pinned."}}]}}"#
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
    fn git_base_policy_is_parsed_and_transition_checked() {
        let workspace = initialize();
        let base = commit(workspace.path(), Some(&policy(A, None, &[])));
        let staged = ProfileManifest::parse(&policy(A, Some(B), &[])).expect("staged policy");
        compare_previous_at_revision(workspace.path(), &staged, &base).expect("stage successor from Git base");

        let reset = ProfileManifest::parse(&policy(B, None, &[])).expect("invalid reset");
        let error = compare_previous_at_revision(workspace.path(), &reset, &base).unwrap_err();
        assert!(error.to_string().contains("must be unchanged"), "{error:#}");
    }

    #[test]
    fn malformed_existing_base_policy_fails_closed() {
        let workspace = initialize();
        let base = commit(workspace.path(), Some(b"not json"));
        let current = ProfileManifest::parse(&policy(A, None, &[])).expect("current policy");
        let error = compare_previous_at_revision(workspace.path(), &current, &base).unwrap_err();
        assert!(error.to_string().contains("validate reviewed command profile policy"), "{error:#}");
    }

    #[test]
    fn initial_creation_requires_an_existing_base_without_the_policy_path() {
        let workspace = initialize();
        let base = commit(workspace.path(), None);
        let current = ProfileManifest::parse(&policy(A, None, &[])).expect("current policy");
        compare_previous_at_revision(workspace.path(), &current, &base).expect("initial policy creation");

        let error = compare_previous_at_revision(workspace.path(), &current, "0000000000000000000000000000000000000000").unwrap_err();
        assert!(error.to_string().contains("not available"), "{error:#}");
    }
}
