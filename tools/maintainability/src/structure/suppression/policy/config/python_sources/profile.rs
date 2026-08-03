use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::POLICY_PATH;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ProfilePolicy {
    schema_version: u32,
    id: String,
    pub(super) current_sha256: String,
    pub(super) preapproved_next_sha256: Option<String>,
    pub(super) retired_sha256: Vec<String>,
    issue: String,
    rationale: String,
    safety_invariant: String,
}

impl ProfilePolicy {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        let policy: Self = serde_json::from_slice(bytes).context("parse Python source profile policy")?;
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.id != "python-source-tree" {
            bail!("Python source profile policy identity is immutable");
        }
        for (label, value) in [
            ("issue", self.issue.as_str()),
            ("rationale", self.rationale.as_str()),
            ("safety invariant", self.safety_invariant.as_str()),
        ] {
            if value.trim().is_empty() {
                bail!("Python source profile policy {label} must not be empty");
            }
        }
        validate_digest("current", &self.current_sha256)?;
        if let Some(next) = &self.preapproved_next_sha256 {
            validate_digest("preapproved next", next)?;
            if next == &self.current_sha256 {
                bail!("Python source profile pending digest must differ from current");
            }
        }
        let mut retired = BTreeSet::new();
        for digest in &self.retired_sha256 {
            validate_digest("retired", digest)?;
            if !retired.insert(digest) {
                bail!("Python source profile retired digests must be unique");
            }
        }
        if retired.contains(&self.current_sha256) || self.preapproved_next_sha256.as_ref().is_some_and(|next| retired.contains(next)) {
            bail!("Python source profile active digests cannot be retired");
        }
        Ok(())
    }

    pub(super) fn matches_current(&self, observed: &str) -> bool {
        self.current_sha256 == observed
    }

    pub(super) fn compare_previous(&self, previous: &Self) -> Result<()> {
        if self.schema_version != previous.schema_version
            || self.id != previous.id
            || self.issue != previous.issue
            || self.rationale != previous.rationale
            || self.safety_invariant != previous.safety_invariant
        {
            bail!("Python source profile identity and evidence are immutable");
        }
        if self.current_sha256 == previous.current_sha256 && self.retired_sha256 == previous.retired_sha256 {
            return compare_pending(self.preapproved_next_sha256.as_deref(), previous.preapproved_next_sha256.as_deref(), &self.current_sha256);
        }
        let Some(next) = previous.preapproved_next_sha256.as_deref() else {
            bail!("Python source profile current digest can change only to a previously preapproved next digest");
        };
        let mut expected_retired = previous.retired_sha256.clone();
        expected_retired.push(previous.current_sha256.clone());
        if self.current_sha256 != next || self.preapproved_next_sha256.is_some() || self.retired_sha256 != expected_retired {
            bail!("Python source profile promotion must atomically promote pending, retire current, and clear pending");
        }
        Ok(())
    }
}

pub(super) fn load(workspace: &Path) -> Result<ProfilePolicy> {
    let path = workspace.join(POLICY_PATH);
    let metadata = fs::symlink_metadata(&path).with_context(|| format!("inspect Python source profile policy {POLICY_PATH:?}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("Python source profile policy must be a regular non-symlink file: {POLICY_PATH:?}");
    }
    let bytes = fs::read(path).with_context(|| format!("read Python source profile policy {POLICY_PATH:?}"))?;
    ProfilePolicy::parse(&bytes)
}

fn compare_pending(current: Option<&str>, previous: Option<&str>, active: &str) -> Result<()> {
    match (previous, current) {
        (None | Some(_), None) => Ok(()),
        (None, Some(next)) if next != active => Ok(()),
        (Some(old), Some(next)) if old == next => Ok(()),
        (None | Some(_), Some(_)) => bail!("Python source profile may stage only one pending digest and cannot replace it"),
    }
}

fn validate_digest(label: &str, value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        bail!("Python source profile {label} digest must be lowercase SHA-256");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn policy(current: &str, next: Option<&str>, retired: &[&str]) -> ProfilePolicy {
        ProfilePolicy {
            schema_version: 1,
            id: "python-source-tree".to_owned(),
            current_sha256: current.to_owned(),
            preapproved_next_sha256: next.map(str::to_owned),
            retired_sha256: retired.iter().map(|digest| (*digest).to_owned()).collect(),
            issue: "https://example.invalid/1".to_owned(),
            rationale: "Atomic Python source review.".to_owned(),
            safety_invariant: "Only staged complete profiles may land.".to_owned(),
        }
    }

    #[test]
    fn stage_promote_cancel_and_unchanged_transitions_are_exact() {
        let initial = policy(A, None, &[]);
        let staged = policy(A, Some(B), &[]);
        staged.compare_previous(&initial).expect("stage pending");
        assert!(staged.matches_current(A));
        assert!(!staged.matches_current(B));
        staged.compare_previous(&staged).expect("unchanged pending");
        initial.compare_previous(&staged).expect("cancel pending");
        let promoted = policy(B, None, &[A]);
        promoted.compare_previous(&staged).expect("promote pending");
        assert!(promoted.matches_current(B));
    }

    #[test]
    fn stale_replaced_unstaged_and_resurrected_profiles_are_rejected() {
        let staged = policy(A, Some(B), &[]);
        assert!(policy(A, Some(C), &[]).compare_previous(&staged).is_err());
        assert!(policy(C, None, &[A]).compare_previous(&policy(A, None, &[])).is_err());
        assert!(policy(B, Some(C), &[A]).compare_previous(&staged).is_err());
        assert!(policy(A, None, &[]).compare_previous(&policy(B, None, &[A])).is_err());
    }
}
