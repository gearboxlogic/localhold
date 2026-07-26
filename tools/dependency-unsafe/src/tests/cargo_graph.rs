use std::collections::{BTreeMap, BTreeSet};

use super::{
    CRATES_IO_SOURCE, DependencyKind, GraphConfig, LockedPackage, Lockfile, Metadata, Node, NodeDependency, Package, Resolve, Target, dependency_is_selected, graph_from_tree,
    metadata_arguments, parse_host_target, parse_tree, require_native_target, tree_arguments, validate_target,
};
use crate::config::PlatformConfig;

fn configuration(include_dev: bool) -> GraphConfig {
    GraphConfig {
        id: "test".to_owned(),
        profile: "dev".to_owned(),
        all_features: false,
        features: vec!["feature".to_owned()],
        include_dev,
    }
}

fn platform() -> PlatformConfig {
    PlatformConfig {
        name: "linux".to_owned(),
        target: "x86_64-unknown-linux-gnu".to_owned(),
        baseline: "baseline".to_owned(),
        configurations: Vec::new(),
    }
}

#[test]
fn development_edges_require_explicit_configuration() {
    let dependency = NodeDependency {
        pkg: "dev".to_owned(),
        dep_kinds: vec![DependencyKind { kind: Some("dev".to_owned()) }],
    };
    assert!(!dependency_is_selected(&dependency, false));
    assert!(dependency_is_selected(&dependency, true));
}

#[test]
fn normal_and_build_edges_are_always_selected() {
    for kind in [None, Some("normal".to_owned()), Some("build".to_owned())] {
        let dependency = NodeDependency {
            pkg: "dependency".to_owned(),
            dep_kinds: vec![DependencyKind { kind }],
        };
        assert!(dependency_is_selected(&dependency, false));
    }
}

#[test]
fn cargo_commands_keep_metadata_unfiltered_and_tree_targeted() {
    let configuration = configuration(false);
    let metadata = metadata_arguments(&platform(), &configuration);
    assert!(!metadata.iter().any(|argument| argument == "--filter-platform"));
    assert!(metadata.windows(2).any(|pair| pair == ["--features", "feature"]));

    let tree = tree_arguments(&platform(), &configuration);
    assert!(tree.windows(2).any(|pair| pair == ["--target", "x86_64-unknown-linux-gnu"]));
    assert!(tree.windows(2).any(|pair| pair == ["--edges", "normal,build"]));
    assert!(tree.iter().any(|argument| argument == "--no-dedupe"));
}

#[test]
fn tree_parser_reconstructs_repeated_edges_and_features() {
    let index = BTreeMap::from([
        ("root v1".to_owned(), vec!["root".to_owned()]),
        ("a v1".to_owned(), vec!["a".to_owned()]),
        ("b v1".to_owned(), vec!["b".to_owned()]),
    ]);
    let tree = parse_tree(b"0root v1|\n1a v1|x\n2b v1|y\n1b v1|z\n", &index).expect("valid tree");
    assert_eq!(tree.root_id, "root");
    assert_eq!(tree.features["b"], BTreeSet::from(["y".to_owned(), "z".to_owned()]));
    assert!(tree.edges.contains(&("a".to_owned(), "b".to_owned())));
    assert!(tree.edges.contains(&("root".to_owned(), "b".to_owned())));
}

#[test]
fn tree_parser_rejects_malformed_or_ambiguous_graphs() {
    let index = BTreeMap::from([("root v1".to_owned(), vec!["root".to_owned()]), ("leaf v1".to_owned(), vec!["leaf".to_owned()])]);
    assert!(parse_tree(b"0root v1|\n2leaf v1|\n", &index).is_err());
    assert!(parse_tree(b"0root v1|\n0leaf v1|\n", &index).is_err());
    assert!(parse_tree(b"0root v1|\n\n", &index).is_err());

    let ambiguous = BTreeMap::from([("same v1".to_owned(), vec!["registry-a".to_owned(), "registry-b".to_owned()])]);
    assert!(parse_tree(b"0same v1|\n", &ambiguous).is_err());
}

#[test]
fn graph_conversion_binds_crates_io_checksum_features_and_route() {
    let root = "path+file:///workspace#localhold@0.2.0".to_owned();
    let dependency = format!("{CRATES_IO_SOURCE}#fixture@1.2.3");
    let metadata = Metadata {
        packages: vec![
            Package {
                id: root.clone(),
                name: "localhold".to_owned(),
                version: "0.2.0".to_owned(),
                source: None,
                targets: Vec::new(),
            },
            Package {
                id: dependency.clone(),
                name: "fixture".to_owned(),
                version: "1.2.3".to_owned(),
                source: Some(CRATES_IO_SOURCE.to_owned()),
                targets: vec![Target {
                    kind: vec!["custom-build".to_owned()],
                }],
            },
        ],
        resolve: Some(Resolve {
            root: Some(root.clone()),
            nodes: vec![
                Node {
                    id: root,
                    deps: vec![NodeDependency {
                        pkg: dependency.clone(),
                        dep_kinds: vec![DependencyKind { kind: None }],
                    }],
                },
                Node {
                    id: dependency.clone(),
                    deps: Vec::new(),
                },
            ],
        }),
    };
    let checksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let lockfile = Lockfile {
        version: 4,
        package: vec![LockedPackage {
            name: "fixture".to_owned(),
            version: "1.2.3".to_owned(),
            source: Some(CRATES_IO_SOURCE.to_owned()),
            checksum: Some(checksum.to_owned()),
        }],
    };
    let tree = b"0localhold v0.2.0 (/workspace)|\n1fixture v1.2.3|enabled\n";

    let graph = graph_from_tree(metadata, &lockfile, &configuration(false), tree).expect("valid graph");
    let source_id = format!("crates.io:fixture@1.2.3#{checksum}");
    assert_eq!(graph.packages[&dependency].source_id, source_id);
    assert_eq!(graph.packages[&dependency].features, ["enabled"]);
    assert!(graph.packages[&dependency].build_script);
    assert!(graph.edges.contains(&("workspace:localhold".to_owned(), source_id)));
}

#[test]
fn invalid_target_is_fatal() {
    assert!(validate_target("definitely-not-a-valid-rust-target").is_err());
}

#[test]
fn native_target_requires_exact_rustc_host_triple() {
    assert_eq!(
        parse_host_target("rustc 1.97.0\nhost: x86_64-unknown-linux-gnu\n").expect("host target"),
        "x86_64-unknown-linux-gnu"
    );
    assert!(parse_host_target("rustc 1.97.0\n").is_err());
    assert!(require_native_target("definitely-not-the-native-host").is_err());
}
