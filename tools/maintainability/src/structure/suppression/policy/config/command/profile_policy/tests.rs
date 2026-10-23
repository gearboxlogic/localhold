use std::fs;
use std::path::Path;

use super::*;

#[test]
fn checked_in_legacy_transition_inventory_is_exact() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = ProfileManifest::parse(&fs::read(workspace.join(POLICY_PATH)).expect("checked-in profile policy")).expect("profile policy");
    assert_eq!(
        manifest.profiles().iter().filter(|profile| profile.preapproved_next_sha256.is_some()).count(),
        LEGACY_TRANSITION_BRIDGES.len()
    );

    for bridge in LEGACY_TRANSITION_BRIDGES {
        let source = fs::read_to_string(workspace.join(bridge.path)).expect("checked-in bridge source");
        assert!(manifest.legacy_transition_bridge(bridge.path, &source).is_some());
        assert!(manifest.legacy_transition_bridge(bridge.path, &format!("{source}\n# tampered")).is_none());

        let mut changed = ProfileManifest::parse(&fs::read(workspace.join(POLICY_PATH)).expect("checked-in profile policy")).expect("profile policy");
        let profile = changed.profiles.iter_mut().find(|profile| profile.path == bridge.path).expect("bridge profile");
        let mut changed_history = ProfileManifest::parse(&fs::read(workspace.join(POLICY_PATH)).expect("checked-in policy")).expect("profile policy");
        changed_history
            .profiles
            .iter_mut()
            .find(|profile| profile.path == bridge.path)
            .expect("bridge profile")
            .retired_sha256
            .push("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned());
        assert!(changed_history.legacy_transition_bridge(bridge.path, &source).is_none());
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
