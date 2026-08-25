use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub(super) const POLICY_PATH: &str = "policy/maintainability/reviewed-command-profiles.json";
#[derive(Clone, Copy)]
pub(super) struct LegacyTransitionBridge {
    pub(super) path: &'static str,
    pub(super) current: &'static str,
    pub(super) successor: &'static str,
    pub(super) opaque_execution_inputs: bool,
    pub(super) weakening: bool,
}

// Exact legacy bytes may bridge only to staged successors; promotion retires each bridge.
const LEGACY_TRANSITION_BRIDGES: &[LegacyTransitionBridge] = &[
    LegacyTransitionBridge {
        path: "script/bootstrap.sh",
        current: "36982c49561af13986fc34ddeefd759010cd615980604eca34d09ef5ba0358c3",
        successor: "e0302179ecc01f9feb178b74420db156736c04e33a4e67d304fa3bc2390fdbf3",
        opaque_execution_inputs: true,
        weakening: false,
    },
    LegacyTransitionBridge {
        path: "script/dep-audit.sh",
        current: "5542706978c03c28159305257466a32566fd66bcae9c7502de4be91fa45ae7d1",
        successor: "03b36529705c704b244dd5e128e1dd1461a66677bdda0bcceedaa582015160dc",
        opaque_execution_inputs: true,
        weakening: false,
    },
    LegacyTransitionBridge {
        path: "script/test-postgres-smoke.sh",
        current: "2f54d872c4773e0ade58b2c0d70bf37e43a477ab809b5ad454195af895169066",
        successor: "88a8e659f6e4c238041d037e4a49301806361a42c48706241c39ee8ad01e9724",
        opaque_execution_inputs: true,
        weakening: false,
    },
    LegacyTransitionBridge {
        path: "script/check-maintainability-bootstrap.sh",
        current: "29251ec4e800ba0e67085ac300dd68eb8bc8db00ab967779e8d72f467e816da0",
        successor: "ad7f09c1e56d9949fd5728db4a9a1a1e6b0e360e028e3fa24fc5f35e8aa678f0",
        opaque_execution_inputs: true,
        weakening: true,
    },
    LegacyTransitionBridge {
        path: "script/claude-review.sh",
        current: "c6c56c0212389a349b4a39e95d2578310bcc1a13bcbe8377c010ca69d1aefc8a",
        successor: "7a0b509574ded78ba3c0589bae798b4e6d7d7658e5bebe48515a3ae73fafbc78",
        opaque_execution_inputs: true,
        weakening: true,
    },
    LegacyTransitionBridge {
        path: "script/tests/test_claude_review.sh",
        current: "41c33e1d76f36d8c9e5050a15b24de19c3044078694170d95d672657f6f8940c",
        successor: "e3d3dfedbb7823e3505d5bf2393656e2d464929892686a83b66df6e9f6f0b07b",
        opaque_execution_inputs: false,
        weakening: true,
    },
    LegacyTransitionBridge {
        path: ".github/workflows/ci.yml",
        current: "a0028b1a3d2f94c2d914dc6328f8a903ae502b36f0bf3a9f3d86d9c76f22efd1",
        successor: "ce9dd65050334562bc7dc7ae6fc4a167641c251cd607542f11c4365c9beaec2d",
        opaque_execution_inputs: false,
        weakening: true,
    },
];

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

    pub(super) fn legacy_transition_bridge(&self, path: &str, source: &str) -> Option<LegacyTransitionBridge> {
        let observed = format!("{:x}", Sha256::digest(source.as_bytes()));
        let profile = self
            .profiles
            .iter()
            .find(|profile| profile.path == path && profile.current_sha256 == observed && profile.retired_sha256.is_empty())?;
        let staged_transition = (profile.path.as_str(), profile.current_sha256.as_str(), profile.preapproved_next_sha256.as_deref()?);
        LEGACY_TRANSITION_BRIDGES
            .iter()
            .copied()
            .find(|bridge| (bridge.path, bridge.current, bridge.successor) == staged_transition)
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
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn checked_in_legacy_transition_inventory_is_exact() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifest = ProfileManifest::parse(&fs::read(workspace.join(POLICY_PATH)).expect("checked-in profile policy")).expect("profile policy");
        let expected = LEGACY_TRANSITION_BRIDGES
            .iter()
            .map(|bridge| (bridge.path, bridge.current, bridge.successor))
            .collect::<BTreeSet<_>>();
        let actual = manifest
            .profiles()
            .iter()
            .filter_map(|profile| {
                let successor = profile.preapproved_next_sha256.as_deref()?;
                let source = fs::read_to_string(workspace.join(&profile.path)).expect("checked-in bridge source");
                manifest
                    .legacy_transition_bridge(&profile.path, &source)
                    .is_some()
                    .then_some((profile.path.as_str(), profile.current_sha256.as_str(), successor))
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);

        for bridge in LEGACY_TRANSITION_BRIDGES {
            let source = fs::read_to_string(workspace.join(bridge.path)).expect("checked-in bridge source");
            assert!(manifest.legacy_transition_bridge(bridge.path, &source).is_some());
            assert!(manifest.legacy_transition_bridge(bridge.path, &format!("{source}\n# tampered")).is_none());

            let mut changed = ProfileManifest::parse(&fs::read(workspace.join(POLICY_PATH)).expect("checked-in profile policy")).expect("profile policy");
            let profile = changed.profiles.iter_mut().find(|profile| profile.path == bridge.path).expect("bridge profile");
            profile.preapproved_next_sha256 = Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned());
            assert!(changed.legacy_transition_bridge(bridge.path, &source).is_none());
        }
    }

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
        assert!(staged.legacy_transition_bridge("script/reviewed.sh", old_source).is_none());
        assert!(staged.legacy_transition_bridge("script/reviewed.sh", new_source).is_none());
        assert!(initial.legacy_transition_bridge("script/reviewed.sh", old_source).is_none());

        let promoted = manifest(&new, None, &[&old]);
        promoted.compare_previous(&staged).expect("promote successor");
        assert!(promoted.source_is_current("script/reviewed.sh", new_source));
        assert!(!promoted.source_is_current("script/reviewed.sh", old_source));
        assert!(promoted.legacy_transition_bridge("script/reviewed.sh", new_source).is_none());
        assert!(manifest(&new, None, &[]).compare_previous(&initial).is_err());
        assert!(manifest(&old, Some(A), &[]).compare_previous(&staged).is_err());
    }
}
