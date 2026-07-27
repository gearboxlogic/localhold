use crate::scan::{SiteKind, UnsafeSite};

use super::{ExpectedSite, UnsafeManifest, validate_site_operation_cardinality};

fn manifest(fingerprint: &str) -> UnsafeManifest {
    UnsafeManifest {
        schema_version: 1,
        baseline_commit: "0".repeat(40),
        tracked_roots: UnsafeManifest::required_roots(),
        required_lints: Vec::new(),
        contracts: Vec::new(),
        sites: vec![expected_site(SiteKind::Block, vec!["operation.one".to_owned()], fingerprint)],
    }
}

fn expected_site(kind: SiteKind, operation_ids: Vec<String>, fingerprint: &str) -> ExpectedSite {
    ExpectedSite {
        id: "site.one".to_owned(),
        contract_id: "contract.one".to_owned(),
        path: "src/sample.rs".to_owned(),
        item: "boundary".to_owned(),
        kind,
        occurrence: 0,
        fingerprint: fingerprint.to_owned(),
        boundary_fingerprint: fingerprint.to_owned(),
        operation_ids,
    }
}

fn site(fingerprint: &str) -> UnsafeSite {
    UnsafeSite {
        path: "src/sample.rs".to_owned(),
        item: "boundary".to_owned(),
        kind: SiteKind::Block,
        occurrence: 0,
        fingerprint: fingerprint.to_owned(),
        boundary_fingerprint: fingerprint.to_owned(),
    }
}

#[test]
fn comparison_rejects_missing_unexpected_and_mutated_sites() {
    let fingerprint = "a".repeat(64);
    let policy = manifest(&fingerprint);
    assert!(policy.compare_sites(&[site(&fingerprint)]).is_ok());
    assert!(policy.compare_sites(&[]).is_err());
    assert!(policy.compare_sites(&[site(&"b".repeat(64))]).is_err());

    let mut changed_boundary = site(&fingerprint);
    changed_boundary.boundary_fingerprint = "b".repeat(64);
    assert!(policy.compare_sites(&[changed_boundary]).is_err());

    let mut moved = site(&fingerprint);
    moved.item = "other_boundary".to_owned();
    assert!(policy.compare_sites(&[moved]).is_err());
}

#[test]
fn site_operation_cardinality_is_exact() {
    let fingerprint = "a".repeat(64);
    assert!(validate_site_operation_cardinality(&expected_site(SiteKind::Block, vec!["operation.one".to_owned()], &fingerprint)).is_ok());
    assert!(validate_site_operation_cardinality(&expected_site(SiteKind::Block, Vec::new(), &fingerprint)).is_err());
    assert!(validate_site_operation_cardinality(&expected_site(SiteKind::Block, vec!["operation.one".to_owned(), "operation.two".to_owned()], &fingerprint,)).is_err());
    assert!(validate_site_operation_cardinality(&expected_site(SiteKind::LintException, Vec::new(), &fingerprint)).is_ok());
    assert!(validate_site_operation_cardinality(&expected_site(SiteKind::LintException, vec!["operation.one".to_owned()], &fingerprint)).is_err());
}
