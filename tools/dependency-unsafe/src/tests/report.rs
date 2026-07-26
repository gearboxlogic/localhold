use super::{build_source_row, canonical_paths, reported_signals, validate_coverage_sets};
use crate::cargo_graph::{DependencyPackage, ResolvedGraph};
use crate::config::Classification;
use crate::scan::SourceAssessment;
use std::collections::{BTreeMap, BTreeSet};

fn package(id: &str) -> DependencyPackage {
    DependencyPackage {
        source_id: id.to_owned(),
        name: id.to_owned(),
        version: "1".to_owned(),
        checksum: "checksum".to_owned(),
        features: Vec::new(),
        build_script: false,
        proc_macro: false,
    }
}

fn graph(edges: BTreeSet<(String, String)>) -> ResolvedGraph {
    ResolvedGraph {
        configuration_id: "test".to_owned(),
        profile: "dev".to_owned(),
        requested_features: Vec::new(),
        all_features: false,
        include_dev: false,
        root_label: "root".to_owned(),
        packages: BTreeMap::from([("a".to_owned(), package("a")), ("b".to_owned(), package("b")), ("leaf".to_owned(), package("leaf"))]),
        edges,
    }
}

#[test]
fn canonical_path_uses_sorted_shortest_route() {
    let graph = graph(BTreeSet::from([
        ("root".to_owned(), "b".to_owned()),
        ("root".to_owned(), "a".to_owned()),
        ("a".to_owned(), "leaf".to_owned()),
        ("b".to_owned(), "leaf".to_owned()),
    ]));
    let paths = canonical_paths(&graph).expect("connected graph");
    assert_eq!(paths["leaf"], ["root", "a", "leaf"]);
}

#[test]
fn canonical_path_rejects_unreachable_packages() {
    let graph = graph(BTreeSet::from([("root".to_owned(), "a".to_owned()), ("a".to_owned(), "leaf".to_owned())]));
    assert!(canonical_paths(&graph).is_err());
}

#[test]
fn build_script_and_proc_macro_are_explicit_exposure_signals() {
    let mut package = package("fixture");
    package.build_script = true;
    package.proc_macro = true;
    let assessment = SourceAssessment {
        rust_unsafe_present: false,
        signals: BTreeSet::new(),
    };
    assert_eq!(reported_signals(&assessment, &package), ["build-script", "proc-macro"]);
}

#[test]
fn source_row_policy_enforcement_covers_required_inventory_stale_and_safe_cases() {
    let package = package("fixture");
    let exposed = SourceAssessment {
        rust_unsafe_present: true,
        signals: BTreeSet::from(["rust-unsafe-syntax".to_owned()]),
    };
    assert!(build_source_row(&package, &exposed, None, true, std::path::Path::new("policy")).is_err());
    let inventory = build_source_row(&package, &exposed, None, false, std::path::Path::new("policy"))
        .expect("inventory allows unclassified exposure")
        .expect("exposed source row");
    assert!(inventory.classification.is_none());
    assert!(
        build_source_row(
            &package,
            &SourceAssessment {
                rust_unsafe_present: false,
                signals: BTreeSet::new(),
            },
            Some(Classification::Other),
            true,
            std::path::Path::new("policy"),
        )
        .is_err()
    );
    assert!(
        build_source_row(
            &package,
            &SourceAssessment {
                rust_unsafe_present: false,
                signals: BTreeSet::new(),
            },
            None,
            true,
            std::path::Path::new("policy"),
        )
        .expect("safe unclassified package")
        .is_none()
    );
}

#[test]
fn classification_coverage_requires_exact_native_baseline_union() {
    let ids = BTreeSet::from(["a".to_owned(), "b".to_owned()]);
    validate_coverage_sets(&ids, &ids).expect("exact coverage");
    assert!(validate_coverage_sets(&ids, &BTreeSet::from(["a".to_owned()])).is_err());
    assert!(validate_coverage_sets(&ids, &BTreeSet::from(["a".to_owned(), "b".to_owned(), "c".to_owned()])).is_err());
}
