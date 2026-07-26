use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cargo_env::{CargoEnvironment, pinned_rustc_command};
use crate::config::{AuditConfig, GraphConfig, PlatformConfig};

const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";

type PackageEdges = BTreeSet<(String, String)>;

#[derive(Clone, Debug)]
pub struct DependencyPackage {
    pub source_id: String,
    pub name: String,
    pub version: String,
    pub checksum: String,
    pub features: Vec<String>,
    pub build_script: bool,
    pub proc_macro: bool,
}

#[derive(Debug)]
pub struct ResolvedGraph {
    pub configuration_id: String,
    pub profile: String,
    pub requested_features: Vec<String>,
    pub all_features: bool,
    pub include_dev: bool,
    pub root_label: String,
    pub packages: BTreeMap<String, DependencyPackage>,
    pub edges: PackageEdges,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    resolve: Option<Resolve>,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    #[serde(default)]
    targets: Vec<Target>,
}

#[derive(Debug, Deserialize)]
struct Target {
    #[serde(default)]
    kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Resolve {
    root: Option<String>,
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: String,
    #[serde(default)]
    deps: Vec<NodeDependency>,
}

#[derive(Debug, Deserialize)]
struct NodeDependency {
    pkg: String,
    #[serde(default)]
    dep_kinds: Vec<DependencyKind>,
}

#[derive(Debug, Deserialize)]
struct DependencyKind {
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Lockfile {
    version: u32,
    package: Vec<LockedPackage>,
}

#[derive(Debug, Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

struct TreeInventory {
    root_id: String,
    features: BTreeMap<String, BTreeSet<String>>,
    edges: PackageEdges,
}

struct TreeRow {
    depth: usize,
    package: String,
    features: BTreeSet<String>,
}

pub fn verify_toolchain(cargo: &CargoEnvironment, config: &AuditConfig) -> Result<()> {
    let mut cargo_command = cargo.cargo_command()?;
    verify_version(&mut cargo_command, "cargo", &["--version"], &config.cargo_version)?;
    let mut rustc_command = cargo.rustc_command();
    verify_version(&mut rustc_command, "rustc", &["--version"], &config.rustc_version)
}

pub fn require_native_target(target: &str) -> Result<()> {
    let output = pinned_rustc_command()?.arg("-vV").output().context("inspect the native Rust host target")?;
    require_success("rustc host target inspection", &output)?;
    let text = std::str::from_utf8(&output.stdout).context("rustc verbose version output was not UTF-8")?;
    let host = parse_host_target(text)?;
    if host != target {
        bail!("native audit target mismatch: matrix requires {target:?}, rustc host is {host:?}");
    }
    Ok(())
}

fn parse_host_target(verbose_version: &str) -> Result<&str> {
    verbose_version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| !host.is_empty())
        .context("rustc verbose version output omitted the host target")
}

pub fn resolve(workspace: &Path, cargo: &CargoEnvironment, platform: &PlatformConfig, configuration: &GraphConfig) -> Result<ResolvedGraph> {
    validate_target(&platform.target)?;
    let metadata_args = metadata_arguments(platform, configuration);
    let metadata_output = run_cargo(workspace, cargo, &metadata_args, "cargo metadata")?;
    let metadata: Metadata = serde_json::from_slice(&metadata_output.stdout).context("parse cargo metadata JSON")?;
    let lockfile = load_lockfile(workspace)?;

    let tree_args = tree_arguments(platform, configuration);
    let tree_output = run_cargo(workspace, cargo, &tree_args, "cargo tree")?;
    graph_from_tree(metadata, &lockfile, configuration, &tree_output.stdout)
}

fn graph_from_tree(metadata: Metadata, lockfile: &Lockfile, configuration: &GraphConfig, tree_output: &[u8]) -> Result<ResolvedGraph> {
    let resolve = metadata.resolve.context("cargo metadata omitted resolve graph")?;
    let metadata_root = resolve.root.context("cargo metadata omitted the workspace root")?;
    let packages_by_id: BTreeMap<_, _> = metadata.packages.into_iter().map(|package| (package.id.clone(), package)).collect();
    let nodes_by_id: BTreeMap<_, _> = resolve.nodes.into_iter().map(|node| (node.id.clone(), node)).collect();
    let display_index = display_index(&packages_by_id);
    let tree = parse_tree(tree_output, &display_index)?;
    if tree.root_id != metadata_root {
        bail!("cargo tree root {:?} differs from metadata root {:?}", tree.root_id, metadata_root);
    }
    validate_tree_edges(&tree.edges, &nodes_by_id, configuration.include_dev)?;

    let root_package = packages_by_id.get(&metadata_root).context("resolve root has no package metadata")?;
    if root_package.source.is_some() {
        bail!("workspace root unexpectedly has an external source");
    }
    let root_label = format!("workspace:{}", root_package.name);
    let packages = stable_packages(&tree.features, &metadata_root, &packages_by_id, lockfile)?;
    let edges = stable_edges(&tree.edges, &metadata_root, &root_label, &packages)?;
    Ok(ResolvedGraph {
        configuration_id: configuration.id.clone(),
        profile: configuration.profile.clone(),
        requested_features: configuration.features.clone(),
        all_features: configuration.all_features,
        include_dev: configuration.include_dev,
        root_label,
        packages,
        edges,
    })
}

fn parse_tree(bytes: &[u8], display_index: &BTreeMap<String, Vec<String>>) -> Result<TreeInventory> {
    let text = std::str::from_utf8(bytes).context("cargo tree output was not UTF-8")?;
    let mut stack = Vec::<String>::new();
    let mut root_id = None;
    let mut features = BTreeMap::<String, BTreeSet<String>>::new();
    let mut edges = BTreeSet::new();
    for line in text.lines() {
        if line.is_empty() {
            bail!("cargo tree emitted an empty row");
        }
        let row = parse_tree_row(line, display_index)?;
        if row.depth > stack.len() {
            bail!("cargo tree depth jumped at row {line:?}");
        }
        if row.depth == 0 {
            if root_id.is_some() {
                bail!("cargo tree emitted multiple roots");
            }
            root_id = Some(row.package.clone());
        } else {
            let parent = stack.get(row.depth - 1).with_context(|| format!("cargo tree row {line:?} has no parent"))?;
            edges.insert((parent.clone(), row.package.clone()));
        }
        stack.truncate(row.depth);
        stack.push(row.package.clone());
        features.entry(row.package).or_default().extend(row.features);
    }
    Ok(TreeInventory {
        root_id: root_id.context("cargo tree emitted no root")?,
        features,
        edges,
    })
}

fn parse_tree_row(line: &str, display_index: &BTreeMap<String, Vec<String>>) -> Result<TreeRow> {
    let (package_text, feature_text) = line.rsplit_once('|').with_context(|| format!("cargo tree row lacks a feature separator: {line:?}"))?;
    let leading_digits = package_text.bytes().take_while(u8::is_ascii_digit).count();
    let mut matches = Vec::new();
    for split in 1..=leading_digits {
        let depth = package_text[..split].parse::<usize>().with_context(|| format!("invalid cargo tree depth in {line:?}"))?;
        let display = &package_text[split..];
        for (prefix, ids) in display_index {
            let recognized = display == prefix || display.strip_prefix(prefix).is_some_and(|suffix| suffix.starts_with(' '));
            if recognized {
                matches.extend(ids.iter().map(|id| (depth, id.clone())));
            }
        }
    }
    if matches.len() != 1 {
        bail!("unrecognized or ambiguous cargo tree row {line:?}");
    }
    let features = feature_text.split(',').filter(|feature| !feature.is_empty()).map(str::to_owned).collect();
    let (depth, package) = matches.pop().context("tree row match disappeared")?;
    Ok(TreeRow { depth, package, features })
}

fn display_index(packages: &BTreeMap<String, Package>) -> BTreeMap<String, Vec<String>> {
    let mut index = BTreeMap::<String, Vec<String>>::new();
    for package in packages.values() {
        index.entry(format!("{} v{}", package.name, package.version)).or_default().push(package.id.clone());
    }
    index
}

fn validate_tree_edges(edges: &PackageEdges, nodes: &BTreeMap<String, Node>, include_dev: bool) -> Result<()> {
    for (from, to) in edges {
        let source = nodes.get(from).with_context(|| format!("tree package {from} has no metadata node"))?;
        let valid = source
            .deps
            .iter()
            .any(|dependency| dependency.pkg == *to && dependency_is_selected(dependency, include_dev));
        if !valid {
            bail!("cargo tree edge {from} -> {to} is absent from metadata");
        }
    }
    Ok(())
}

fn stable_packages(
    features: &BTreeMap<String, BTreeSet<String>>,
    root: &str,
    packages_by_id: &BTreeMap<String, Package>,
    lockfile: &Lockfile,
) -> Result<BTreeMap<String, DependencyPackage>> {
    let mut packages = BTreeMap::new();
    for (cargo_id, enabled_features) in features {
        if cargo_id == root {
            continue;
        }
        let package = packages_by_id.get(cargo_id).with_context(|| format!("tree package {cargo_id} has no metadata"))?;
        if package.source.as_deref() != Some(CRATES_IO_SOURCE) {
            bail!("unsupported dependency source {:?} for {} {}", package.source, package.name, package.version);
        }
        let checksum = lockfile.checksum(package)?;
        let source_id = format!("crates.io:{}@{}#{checksum}", package.name, package.version);
        packages.insert(
            cargo_id.clone(),
            DependencyPackage {
                source_id,
                name: package.name.clone(),
                version: package.version.clone(),
                checksum,
                features: enabled_features.iter().cloned().collect(),
                build_script: package.targets.iter().any(|target| target.kind.iter().any(|kind| kind == "custom-build")),
                proc_macro: package.targets.iter().any(|target| target.kind.iter().any(|kind| kind == "proc-macro")),
            },
        );
    }
    Ok(packages)
}

fn stable_edges(cargo_edges: &PackageEdges, root: &str, root_label: &str, packages: &BTreeMap<String, DependencyPackage>) -> Result<PackageEdges> {
    let mut edges = BTreeSet::new();
    for (from, to) in cargo_edges {
        if to == root {
            continue;
        }
        let stable_to = packages.get(to).with_context(|| format!("edge target {to} was not selected"))?.source_id.clone();
        let stable_from = if from == root {
            root_label.to_owned()
        } else {
            packages.get(from).with_context(|| format!("edge source {from} was not selected"))?.source_id.clone()
        };
        edges.insert((stable_from, stable_to));
    }
    Ok(edges)
}

impl Lockfile {
    fn checksum(&self, package: &Package) -> Result<String> {
        let matches: Vec<_> = self
            .package
            .iter()
            .filter(|locked| locked.name == package.name && locked.version == package.version && locked.source == package.source)
            .collect();
        if matches.len() != 1 {
            bail!("Cargo.lock has {} rows for {} {} from {:?}", matches.len(), package.name, package.version, package.source);
        }
        matches[0]
            .checksum
            .clone()
            .with_context(|| format!("registry package {} {} has no Cargo.lock checksum", package.name, package.version))
    }
}

fn load_lockfile(workspace: &Path) -> Result<Lockfile> {
    let path = workspace.join("Cargo.lock");
    let source = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let lockfile: Lockfile = toml::from_str(&source).with_context(|| format!("parse {}", path.display()))?;
    if lockfile.version != 4 {
        bail!("unsupported Cargo.lock format {}", lockfile.version);
    }
    Ok(lockfile)
}

fn metadata_arguments(_platform: &PlatformConfig, configuration: &GraphConfig) -> Vec<String> {
    let mut arguments = vec![
        "metadata".to_owned(),
        "--locked".to_owned(),
        "--offline".to_owned(),
        "--format-version".to_owned(),
        "1".to_owned(),
    ];
    add_feature_arguments(&mut arguments, configuration);
    arguments
}

fn tree_arguments(platform: &PlatformConfig, configuration: &GraphConfig) -> Vec<String> {
    let edges = if configuration.include_dev { "normal,build,dev" } else { "normal,build" };
    let mut arguments = vec![
        "tree".to_owned(),
        "--locked".to_owned(),
        "--offline".to_owned(),
        "--target".to_owned(),
        platform.target.clone(),
        "--edges".to_owned(),
        edges.to_owned(),
        "--prefix".to_owned(),
        "depth".to_owned(),
        "--charset".to_owned(),
        "ascii".to_owned(),
        "--no-dedupe".to_owned(),
        "--format".to_owned(),
        "{p}|{f}".to_owned(),
    ];
    add_feature_arguments(&mut arguments, configuration);
    arguments
}

fn add_feature_arguments(arguments: &mut Vec<String>, configuration: &GraphConfig) {
    if configuration.all_features {
        arguments.push("--all-features".to_owned());
    } else if !configuration.features.is_empty() {
        arguments.push("--features".to_owned());
        arguments.push(configuration.features.join(","));
    }
}

fn dependency_is_selected(dependency: &NodeDependency, include_dev: bool) -> bool {
    dependency.dep_kinds.iter().any(|kind| match kind.kind.as_deref() {
        None | Some("normal" | "build") => true,
        Some("dev") => include_dev,
        Some(_) => false,
    })
}

fn validate_target(target: &str) -> Result<()> {
    let output = pinned_rustc_command()?
        .args(["--print", "cfg", "--target", target])
        .output()
        .context("validate requested Rust target")?;
    require_success("rustc target validation", &output)
}

fn verify_version(command: &mut Command, program: &str, arguments: &[&str], expected: &str) -> Result<()> {
    let output = command.args(arguments).output().with_context(|| format!("run {program} {}", arguments.join(" ")))?;
    require_success(program, &output)?;
    let actual = std::str::from_utf8(&output.stdout)
        .with_context(|| format!("{program} version output was not UTF-8"))?
        .trim();
    if actual != expected {
        bail!("{program} version mismatch: expected {expected:?}, found {actual:?}");
    }
    Ok(())
}

fn run_cargo(workspace: &Path, cargo: &CargoEnvironment, arguments: &[String], purpose: &str) -> Result<Output> {
    let output = cargo
        .cargo_command()?
        .args(arguments)
        .args(["--manifest-path".as_ref(), workspace.join("Cargo.toml").as_os_str()])
        .output()
        .with_context(|| format!("run {purpose}"))?;
    require_success(purpose, &output)?;
    Ok(output)
}

fn require_success(purpose: &str, output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("{purpose} exited {}: {stderr}", output.status);
}

#[cfg(test)]
#[path = "tests/cargo_graph.rs"]
mod tests;
