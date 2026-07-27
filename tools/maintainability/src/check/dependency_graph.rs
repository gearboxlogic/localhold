use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::manifest::DependencyPin;

pub(super) fn verify(metadata: &[u8], expected: &BTreeMap<&str, &DependencyPin>) -> Result<()> {
    let metadata: CargoMetadata = serde_json::from_slice(metadata).context("parse Cargo metadata for unsafe-contract dependency graph")?;
    let graph = GraphIndex::new(&metadata)?;
    let protected_ids = protected_package_ids(&metadata, expected)?;

    for (name, pin) in expected {
        verify_pin(name, pin, protected_ids[*name], &graph, &protected_ids)?;
    }
    Ok(())
}

fn protected_package_ids<'a>(metadata: &'a CargoMetadata, expected: &BTreeMap<&str, &DependencyPin>) -> Result<BTreeMap<String, &'a str>> {
    let mut protected_ids = BTreeMap::new();
    for (name, pin) in expected {
        let packages: Vec<_> = metadata
            .packages
            .iter()
            .filter(|package| package.name == **name && package.version == pin.version && package.source.as_deref() == Some(pin.source.as_str()))
            .collect();
        let [package] = packages.as_slice() else {
            bail!("Cargo metadata must contain exactly one pinned unsafe-contract dependency {name}, found {}", packages.len());
        };
        protected_ids.insert((*name).to_owned(), package.id.as_str());
    }
    Ok(protected_ids)
}

fn verify_pin(name: &str, pin: &DependencyPin, package_id: &str, graph: &GraphIndex<'_>, protected_ids: &BTreeMap<String, &str>) -> Result<()> {
    let node = graph
        .nodes_by_id
        .get(package_id)
        .with_context(|| format!("Cargo metadata is missing the resolve node for unsafe-contract dependency {name}"))?;
    let observed_features: BTreeSet<_> = node.features.iter().map(String::as_str).collect();
    if observed_features.len() != node.features.len() {
        bail!("Cargo metadata repeats a resolved feature for unsafe-contract dependency {name}");
    }
    let expected_features: BTreeSet<_> = pin.resolved_features.iter().map(String::as_str).collect();
    if observed_features != expected_features {
        bail!("resolved features changed for unsafe-contract dependency {name}: expected {expected_features:?}, found {observed_features:?}");
    }

    let mut observed_routes = BTreeSet::new();
    for parent in &graph.resolve.nodes {
        if parent.dependencies.iter().any(|dependency| dependency.package == package_id) {
            observed_routes.insert(parent.id.as_str());
        }
    }
    let expected_routes: BTreeSet<_> = pin
        .incoming_routes
        .iter()
        .map(|route| {
            if route == "root" {
                Ok(graph.root)
            } else {
                protected_ids
                    .get(route)
                    .copied()
                    .with_context(|| format!("unsafe-contract dependency {name} has unknown incoming route {route:?}"))
            }
        })
        .collect::<Result<_>>()?;
    if observed_routes == expected_routes {
        return Ok(());
    }
    bail!(
        "incoming routes changed for unsafe-contract dependency {name}: expected {:?}, found {:?}",
        route_labels(&expected_routes, graph.root, &graph.packages_by_id),
        route_labels(&observed_routes, graph.root, &graph.packages_by_id)
    );
}

struct GraphIndex<'a> {
    resolve: &'a CargoResolve,
    root: &'a str,
    packages_by_id: BTreeMap<&'a str, &'a CargoPackage>,
    nodes_by_id: BTreeMap<&'a str, &'a CargoNode>,
}

impl<'a> GraphIndex<'a> {
    fn new(metadata: &'a CargoMetadata) -> Result<Self> {
        let resolve = metadata.resolve.as_ref().context("Cargo metadata is missing the resolved dependency graph")?;
        let root = resolve.root.as_deref().context("Cargo metadata is missing the root package ID")?;
        let mut packages_by_id = BTreeMap::new();
        for package in &metadata.packages {
            if packages_by_id.insert(package.id.as_str(), package).is_some() {
                bail!("Cargo metadata repeats a package ID");
            }
        }
        let mut nodes_by_id = BTreeMap::new();
        for node in &resolve.nodes {
            if nodes_by_id.insert(node.id.as_str(), node).is_some() {
                bail!("Cargo metadata repeats a resolve node ID");
            }
            if !packages_by_id.contains_key(node.id.as_str()) {
                bail!("Cargo metadata resolve node has no package");
            }
        }
        if !nodes_by_id.contains_key(root) {
            bail!("Cargo metadata root package has no resolve node");
        }
        Ok(Self {
            resolve,
            root,
            packages_by_id,
            nodes_by_id,
        })
    }
}

fn route_labels(routes: &BTreeSet<&str>, root: &str, packages: &BTreeMap<&str, &CargoPackage>) -> Vec<String> {
    routes
        .iter()
        .map(|route| {
            if *route == root {
                return "root".to_owned();
            }
            packages
                .get(route)
                .map_or_else(|| "<unknown-package>".to_owned(), |package| format!("{}@{}", package.name, package.version))
        })
        .collect()
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    resolve: Option<CargoResolve>,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
}

#[derive(Deserialize)]
struct CargoResolve {
    root: Option<String>,
    nodes: Vec<CargoNode>,
}

#[derive(Deserialize)]
struct CargoNode {
    id: String,
    features: Vec<String>,
    #[serde(rename = "deps")]
    dependencies: Vec<CargoDependency>,
}

#[derive(Deserialize)]
struct CargoDependency {
    #[serde(rename = "pkg")]
    package: String,
}

#[cfg(test)]
mod tests {
    use super::verify;
    use crate::manifest::DependencyPin;
    use std::collections::BTreeMap;

    #[test]
    fn resolved_dependency_contract_rejects_feature_and_route_drift() {
        let rusqlite = pin("rusqlite", &["backup", "bundled"], &["root"]);
        let sqlite_vec = pin("sqlite-vec", &[], &["root"]);
        let sqlite_sys = pin("libsqlite3-sys", &["bundled"], &["rusqlite"]);
        let expected = BTreeMap::from([("libsqlite3-sys", &sqlite_sys), ("rusqlite", &rusqlite), ("sqlite-vec", &sqlite_vec)]);

        assert!(verify(&metadata(&["backup", "bundled"], false), &expected).is_ok());
        assert!(verify(&metadata(&["backup", "bundled", "modern_sqlite"], false), &expected).is_err());
        assert!(verify(&metadata(&["backup", "bundled"], true), &expected).is_err());
    }

    fn pin(name: &str, features: &[&str], routes: &[&str]) -> DependencyPin {
        DependencyPin {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            source: "registry".to_owned(),
            checksum: "a".repeat(64),
            resolved_features: features.iter().map(|feature| (*feature).to_owned()).collect(),
            incoming_routes: routes.iter().map(|route| (*route).to_owned()).collect(),
        }
    }

    fn metadata(rusqlite_features: &[&str], extra_route: bool) -> Vec<u8> {
        let mut root_dependencies = vec![serde_json::json!({"pkg": "rusqlite"}), serde_json::json!({"pkg": "sqlite-vec"})];
        if extra_route {
            root_dependencies.push(serde_json::json!({"pkg": "bridge"}));
        }
        let mut nodes = vec![
            serde_json::json!({"id": "root", "features": [], "deps": root_dependencies}),
            serde_json::json!({"id": "rusqlite", "features": rusqlite_features, "deps": [{"pkg": "libsqlite3-sys"}]}),
            serde_json::json!({"id": "sqlite-vec", "features": [], "deps": []}),
            serde_json::json!({"id": "libsqlite3-sys", "features": ["bundled"], "deps": []}),
        ];
        let mut packages = vec![
            serde_json::json!({"id": "root", "name": "root-package", "version": "1.0.0", "source": null}),
            serde_json::json!({"id": "rusqlite", "name": "rusqlite", "version": "1.0.0", "source": "registry"}),
            serde_json::json!({"id": "sqlite-vec", "name": "sqlite-vec", "version": "1.0.0", "source": "registry"}),
            serde_json::json!({"id": "libsqlite3-sys", "name": "libsqlite3-sys", "version": "1.0.0", "source": "registry"}),
        ];
        if extra_route {
            packages.push(serde_json::json!({"id": "bridge", "name": "bridge", "version": "1.0.0", "source": "registry"}));
            nodes.push(serde_json::json!({"id": "bridge", "features": [], "deps": [{"pkg": "rusqlite"}]}));
        }
        serde_json::to_vec(&serde_json::json!({
            "packages": packages,
            "resolve": {"root": "root", "nodes": nodes}
        }))
        .expect("Cargo metadata fixture")
    }
}
