use std::collections::BTreeMap;

use super::model::{Disposition, SourceException, Status};
use super::source::{compare_current, validate_exceptions};
use super::{POLICY_ROOT, checked_policy_path};
use crate::structure::suppression::{SourceCategory, SourceSuppression};

const SOURCE_ID: &str = "source.0000000000000000000000000000000000000000000000000000000000000000";

#[test]
fn exact_source_baseline_is_a_downward_only_multiset() {
    let site = site();
    let baseline = BTreeMap::from([((SourceCategory::Production, SOURCE_ID.to_owned()), 1)]);
    assert_eq!(compare_current(std::slice::from_ref(&site), &baseline, &[], true).expect("exact adoption"), baseline);
    assert!(compare_current(&[], &baseline, &[], false).is_ok());

    let growth = [site.clone(), site];
    assert!(compare_current(&growth, &baseline, &[], false).unwrap_err().to_string().contains("new, moved"));
}

#[test]
fn source_policy_rejects_allow_attributes_empty_reasons_and_macro_carried_levels() {
    let baseline = BTreeMap::from([((SourceCategory::Production, SOURCE_ID.to_owned()), 1)]);
    for mutate in [
        |site: &mut SourceSuppression| site.level = "allow".to_owned(),
        |site: &mut SourceSuppression| site.reason.clear(),
        |site: &mut SourceSuppression| site.macro_carried = true,
    ] as [fn(&mut SourceSuppression); 3]
    {
        let mut invalid = site();
        mutate(&mut invalid);
        assert!(compare_current(&[invalid], &baseline, &[], false).is_err());
    }
}

#[test]
fn temporary_source_exceptions_require_removal_evidence() {
    let mut exception = source_exception();
    validate_exceptions(std::slice::from_ref(&exception)).expect("complete temporary evidence");
    let mut duplicate = exception.clone();
    duplicate.id = "exception.duplicate".to_owned();
    assert!(
        validate_exceptions(&[exception.clone(), duplicate])
            .unwrap_err()
            .to_string()
            .contains("duplicate lint-suppression source exception for")
    );
    exception.removal_phase = None;
    assert!(validate_exceptions(&[exception]).unwrap_err().to_string().contains("removal phase"));
}

#[cfg(unix)]
#[test]
fn policy_fragments_reject_symlinked_ancestors() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("temporary workspace");
    let policy_parent = workspace.path().join("policy/maintainability");
    let real_fragments = workspace.path().join("real-fragments");
    std::fs::create_dir_all(&policy_parent).expect("policy parent");
    std::fs::create_dir(&real_fragments).expect("real fragment directory");
    std::fs::write(real_fragments.join("fragment.json"), "{}").expect("fragment");
    symlink(&real_fragments, policy_parent.join("lint-suppressions")).expect("symlink fragment root");

    let relative = format!("{POLICY_ROOT}/fragment.json");
    let error = checked_policy_path(workspace.path(), &relative).unwrap_err();
    assert!(error.to_string().contains("path components must not be symlinks"));
}

fn site() -> SourceSuppression {
    SourceSuppression {
        id: SOURCE_ID.to_owned(),
        path: "src/service.rs".to_owned(),
        component: "protocol".to_owned(),
        item: "serve".to_owned(),
        scope: "item-fn".to_owned(),
        signature: Some("signature".to_owned()),
        target: None,
        category: SourceCategory::Production,
        level: "expect".to_owned(),
        lint: "clippy::too_many_arguments".to_owned(),
        reason: "reviewed protocol boundary".to_owned(),
        macro_carried: false,
        occurrence: 0,
        fingerprint: "attribute".to_owned(),
    }
}

fn source_exception() -> SourceException {
    SourceException {
        id: "exception.protocol".to_owned(),
        source_id: SOURCE_ID.to_owned(),
        category: SourceCategory::Production,
        max_occurrences: 1,
        disposition: Disposition::Temporary,
        status: Status::Active,
        owner: "maintainers".to_owned(),
        issue: "issue".to_owned(),
        pull_request: "pull request".to_owned(),
        rationale: "reviewed boundary".to_owned(),
        safety_invariant: "exact signature".to_owned(),
        alternatives_considered: "parameter object rejected".to_owned(),
        evidence: "focused tests".to_owned(),
        re_review_phase: "Phase 1".to_owned(),
        removal_issue: Some("removal issue".to_owned()),
        removal_phase: Some("Phase 2".to_owned()),
    }
}
