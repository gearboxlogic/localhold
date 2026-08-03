use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub(super) const POLICY_PATH: &str = "policy/maintainability/reviewed-command-profiles.json";

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ProfileManifest {
    schema_version: u32,
    profiles: Vec<SourceProfile>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceProfile {
    pub(super) id: String,
    pub(super) path: String,
    pub(super) current_sha256: String,
    pub(super) preapproved_next_sha256: Option<String>,
    pub(super) retired_sha256: Vec<String>,
    issue: String,
    rationale: String,
    safety_invariant: String,
}

impl ProfileManifest {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        let manifest: Self = serde_json::from_slice(bytes).context("parse reviewed command profile policy")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub(super) fn profiles(&self) -> &[SourceProfile] {
        &self.profiles
    }

    pub(super) fn source_is_current(&self, path: &str, source: &str) -> bool {
        let observed = format!("{:x}", Sha256::digest(source.as_bytes()));
        self.profiles.iter().filter(|profile| profile.path == path && profile.current_sha256 == observed).count() == 1
    }

    pub(super) fn compare_previous(&self, previous: &Self) -> Result<()> {
        if self.schema_version != previous.schema_version {
            bail!("reviewed command profile policy schema is immutable");
        }
        let current = profile_map(&self.profiles)?;
        let prior = profile_map(&previous.profiles)?;
        if current.keys().collect::<BTreeSet<_>>() != prior.keys().collect::<BTreeSet<_>>() {
            bail!("reviewed command profile IDs cannot be added or removed");
        }
        for (id, profile) in current {
            let prior = prior.get(id).context("previous reviewed command profile is missing")?;
            if profile.path != prior.path || profile.issue != prior.issue || profile.rationale != prior.rationale || profile.safety_invariant != prior.safety_invariant {
                bail!("reviewed command profile {id:?} identity and evidence are immutable");
            }
            validate_transition(profile, prior)?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("reviewed command profile policy schema must be 1");
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for profile in &self.profiles {
            validate_profile(profile, &mut ids, &mut paths)?;
        }
        Ok(())
    }
}

fn validate_profile<'a>(profile: &'a SourceProfile, ids: &mut BTreeSet<&'a str>, paths: &mut BTreeSet<&'a str>) -> Result<()> {
    if profile.id.trim().is_empty() || !ids.insert(profile.id.as_str()) {
        bail!("reviewed command profile IDs must be nonempty and unique");
    }
    super::super::validate_relative_path(&profile.path, "reviewed command profile path")?;
    if !paths.insert(profile.path.as_str()) {
        bail!("reviewed command profile paths must be unique");
    }
    for (label, value) in [
        ("issue", profile.issue.as_str()),
        ("rationale", profile.rationale.as_str()),
        ("safety invariant", profile.safety_invariant.as_str()),
    ] {
        if value.trim().is_empty() {
            bail!("reviewed command profile {:?} requires {label}", profile.id);
        }
    }
    validate_sha256(&profile.current_sha256, "current")?;
    let mut digests = BTreeSet::from([profile.current_sha256.as_str()]);
    if let Some(next) = &profile.preapproved_next_sha256 {
        validate_sha256(next, "preapproved next")?;
        if !digests.insert(next) {
            bail!("reviewed command profile {:?} has duplicate live digests", profile.id);
        }
    }
    for retired in &profile.retired_sha256 {
        validate_sha256(retired, "retired")?;
        if !digests.insert(retired) {
            bail!("reviewed command profile {:?} reuses a live or retired digest", profile.id);
        }
    }
    Ok(())
}

fn validate_transition(current: &SourceProfile, previous: &SourceProfile) -> Result<()> {
    let unchanged = current.current_sha256 == previous.current_sha256
        && current.preapproved_next_sha256 == previous.preapproved_next_sha256
        && current.retired_sha256 == previous.retired_sha256;
    let staged = current.current_sha256 == previous.current_sha256
        && previous.preapproved_next_sha256.is_none()
        && current.preapproved_next_sha256.is_some()
        && current.retired_sha256 == previous.retired_sha256;
    let cancelled = current.current_sha256 == previous.current_sha256
        && previous.preapproved_next_sha256.is_some()
        && current.preapproved_next_sha256.is_none()
        && current.retired_sha256 == previous.retired_sha256;
    let mut promoted_retired = previous.retired_sha256.clone();
    promoted_retired.push(previous.current_sha256.clone());
    let promoted = previous.preapproved_next_sha256.as_deref() == Some(current.current_sha256.as_str())
        && current.preapproved_next_sha256.is_none()
        && current.retired_sha256 == promoted_retired;
    if !(unchanged || staged || cancelled || promoted) {
        bail!(
            "reviewed command profile {:?} must be unchanged, stage or cancel one successor, or promote the preapproved successor and retire the old digest",
            current.id
        );
    }
    Ok(())
}

fn profile_map(profiles: &[SourceProfile]) -> Result<BTreeMap<&str, &SourceProfile>> {
    let mut mapped = BTreeMap::new();
    for profile in profiles {
        if mapped.insert(profile.id.as_str(), profile).is_some() {
            bail!("reviewed command profile IDs must be unique");
        }
    }
    Ok(mapped)
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) {
        bail!("reviewed command profile {label} digest must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(current: &str, next: Option<&str>, retired: &[&str]) -> ProfileManifest {
        let next = next.map_or_else(|| "null".to_owned(), |digest| format!("\"{digest}\""));
        let retired = retired.iter().map(|digest| format!("\"{digest}\"")).collect::<Vec<_>>().join(",");
        let source = format!(
            r#"{{
                "schema_version": 1,
                "profiles": [{{
                    "id": "reviewed",
                    "path": "script/reviewed.sh",
                    "current_sha256": "{current}",
                    "preapproved_next_sha256": {next},
                    "retired_sha256": [{retired}],
                    "issue": "https://example.invalid/1",
                    "rationale": "Reviewed dynamic cleanup.",
                    "safety_invariant": "The exact command and complete source are pinned."
                }}]
            }}"#
        );
        ProfileManifest::parse(source.as_bytes()).expect("profile manifest")
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn transitions_are_bounded_and_one_way() {
        let current = manifest(A, None, &[]);
        let staged = manifest(A, Some(B), &[]);
        staged.compare_previous(&current).expect("stage successor");

        let promoted = manifest(B, None, &[A]);
        promoted.compare_previous(&staged).expect("promote successor");
        assert!(current.compare_previous(&promoted).is_err());
        assert!(manifest(A, None, &[B]).compare_previous(&promoted).is_err());
        assert!(manifest(C, None, &[A, B]).compare_previous(&promoted).is_err());
    }

    #[test]
    fn pending_successor_can_only_be_cancelled_before_promotion() {
        let current = manifest(A, None, &[]);
        let staged = manifest(A, Some(B), &[]);
        current.compare_previous(&staged).expect("cancel successor");
        assert!(manifest(A, Some(C), &[]).compare_previous(&staged).is_err());
        assert!(manifest(B, Some(C), &[A]).compare_previous(&staged).is_err());
    }

    #[test]
    fn validated_transition_authorizes_only_the_current_source() {
        let old_source = "old source\n";
        let new_source = "new source\n";
        let old = format!("{:x}", Sha256::digest(old_source));
        let new = format!("{:x}", Sha256::digest(new_source));

        let initial = manifest(&old, None, &[]);
        let staged = manifest(&old, Some(&new), &[]);
        staged.compare_previous(&initial).expect("stage successor");
        assert!(staged.source_is_current("script/reviewed.sh", old_source));
        assert!(!staged.source_is_current("script/reviewed.sh", new_source));

        let promoted = manifest(&new, None, &[&old]);
        promoted.compare_previous(&staged).expect("promote successor");
        assert!(promoted.source_is_current("script/reviewed.sh", new_source));
        assert!(!promoted.source_is_current("script/reviewed.sh", old_source));
        assert!(manifest(&new, None, &[]).compare_previous(&initial).is_err());
        assert!(manifest(&old, Some(A), &[]).compare_previous(&staged).is_err());
    }
}
