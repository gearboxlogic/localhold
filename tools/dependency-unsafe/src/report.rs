use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::{GeneratedArtifacts, append_json_lines, json_line, json_lines, pretty_json};
use crate::cargo_env::CargoEnvironment;
use crate::cargo_graph::{self, DependencyPackage, ResolvedGraph};
use crate::config::{AuditConfig, Classification, ClassificationPolicy, PlatformConfig};
use crate::scan;

#[derive(Debug, Serialize)]
struct Manifest {
    schema_version: u32,
    generator: Generator,
    platform: ReportPlatform,
    inputs: Inputs,
    source_records: usize,
    configurations: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Generator {
    name: &'static str,
    version: String,
    cargo: String,
    rustc: String,
}

#[derive(Debug, Serialize)]
struct ReportPlatform {
    name: String,
    target: String,
}

#[derive(Debug, Serialize)]
struct Inputs {
    #[serde(rename = "audit_tool_lock_sha256")]
    audit_tool_lock: String,
    #[serde(rename = "audit_tool_source_sha256")]
    audit_tool_source: String,
}

#[derive(Debug, Serialize)]
struct SourceRow {
    record: &'static str,
    id: String,
    name: String,
    version: String,
    checksum: String,
    unsafe_present: bool,
    exposure_present: bool,
    targets: TargetKinds,
    #[serde(skip_serializing_if = "Option::is_none")]
    classification: Option<Classification>,
    signals: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TargetKinds {
    build_script: bool,
    proc_macro: bool,
}

#[derive(Debug, Serialize)]
struct ConfigurationHeader {
    record: &'static str,
    id: String,
    target: String,
    profile: String,
    feature_mode: &'static str,
    requested_features: Vec<String>,
    edge_kinds: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct ConfigurationPackage {
    record: &'static str,
    source_id: String,
    enabled_features: Vec<String>,
    dependency_path: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DependencyAdjacency {
    record: &'static str,
    from: String,
    to: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BaselineSource {
    id: String,
}

pub struct GenerateRequest<'a> {
    pub workspace: &'a Path,
    pub cargo: &'a CargoEnvironment,
    pub classifications_path: &'a Path,
    pub matrix: &'a AuditConfig,
    pub classifications: &'a ClassificationPolicy,
    pub platform: &'a PlatformConfig,
    pub require_classifications: bool,
    pub tool_version: &'a str,
}

pub fn generate(request: &GenerateRequest<'_>) -> Result<GeneratedArtifacts> {
    let vendor = scan::vendor(request.workspace, request.cargo)?;
    let (graphs, packages) = resolve_configurations(request, request.cargo, vendor.path())?;
    let source_rows = source_rows(request, vendor.path(), &packages)?;
    build_artifacts(request, &graphs, &source_rows)
}

pub fn validate_classification_coverage(workspace: &Path, matrix: &AuditConfig, classifications: &ClassificationPolicy) -> Result<bool> {
    let mut observed = BTreeSet::new();
    for platform in &matrix.platforms {
        let path = workspace.join(&platform.baseline).join("sources.jsonl");
        if !path.is_file() {
            return Ok(false);
        }
        let source = fs::read_to_string(&path).with_context(|| format!("read baseline sources {}", path.display()))?;
        for (index, line) in source.lines().enumerate() {
            let row: BaselineSource = serde_json::from_str(line).with_context(|| format!("parse baseline source {} line {}", path.display(), index + 1))?;
            observed.insert(row.id);
        }
    }
    validate_coverage_sets(&classifications.source_ids(), &observed)?;
    Ok(true)
}

fn validate_coverage_sets(classified: &BTreeSet<String>, observed: &BTreeSet<String>) -> Result<()> {
    if classified == observed {
        return Ok(());
    }
    let unclassified: Vec<_> = observed.difference(classified).take(10).cloned().collect();
    let orphaned: Vec<_> = classified.difference(observed).take(10).cloned().collect();
    bail!(
        "classification coverage differs from the union of native baselines; \
         unclassified={unclassified:?}, orphaned={orphaned:?}"
    )
}

type ResolvedConfigurations = (Vec<ResolvedGraph>, BTreeMap<String, DependencyPackage>);

fn resolve_configurations(request: &GenerateRequest<'_>, cargo: &CargoEnvironment, vendor: &Path) -> Result<ResolvedConfigurations> {
    let mut graphs = Vec::new();
    let mut packages = BTreeMap::<String, DependencyPackage>::new();
    for configuration in &request.platform.configurations {
        let graph =
            cargo_graph::resolve(request.workspace, cargo, vendor, request.platform, configuration).with_context(|| format!("resolve configuration {}", configuration.id))?;
        for package in graph.packages.values() {
            match packages.get(&package.source_id) {
                Some(existing) if existing.name != package.name || existing.version != package.version || existing.checksum != package.checksum => {
                    bail!("source identity collision for {}", package.source_id);
                }
                Some(_) => {}
                None => {
                    packages.insert(package.source_id.clone(), package.clone());
                }
            }
        }
        graphs.push(graph);
    }
    Ok((graphs, packages))
}

fn source_rows(request: &GenerateRequest<'_>, vendor_root: &Path, packages: &BTreeMap<String, DependencyPackage>) -> Result<Vec<SourceRow>> {
    let mut rows = Vec::new();
    for package in packages.values() {
        let assessment = scan::assess(vendor_root, package)?;
        let classification = request.classifications.classification(&package.source_id);
        if let Some(row) = build_source_row(package, &assessment, classification, request.require_classifications, request.classifications_path)? {
            rows.push(row);
        }
    }
    Ok(rows)
}

fn build_source_row(
    package: &DependencyPackage,
    assessment: &scan::SourceAssessment,
    classification: Option<Classification>,
    require_classification: bool,
    classifications_path: &Path,
) -> Result<Option<SourceRow>> {
    let signals = reported_signals(assessment, package);
    let exposure_present = !signals.is_empty();
    if require_classification && exposure_present && classification.is_none() {
        bail!(
            "exposed package {} {} is unclassified; add a reviewed entry to {}",
            package.name,
            package.version,
            classifications_path.display()
        );
    }
    if !exposure_present && classification.is_some() {
        bail!("classification for {} is stale because the selected source has no exposure signal", package.name);
    }
    if !exposure_present {
        return Ok(None);
    }
    Ok(Some(SourceRow {
        record: "source",
        id: package.source_id.clone(),
        name: package.name.clone(),
        version: package.version.clone(),
        checksum: package.checksum.clone(),
        unsafe_present: assessment.rust_unsafe_present,
        exposure_present,
        targets: TargetKinds {
            build_script: package.build_script,
            proc_macro: package.proc_macro,
        },
        classification,
        signals,
    }))
}

fn reported_signals(assessment: &scan::SourceAssessment, package: &DependencyPackage) -> Vec<String> {
    let mut signals = assessment.signals.clone();
    if package.build_script {
        signals.insert("build-script".to_owned());
    }
    if package.proc_macro {
        signals.insert("proc-macro".to_owned());
    }
    signals.into_iter().collect()
}

fn build_artifacts(request: &GenerateRequest<'_>, graphs: &[ResolvedGraph], source_rows: &[SourceRow]) -> Result<GeneratedArtifacts> {
    let exposed: BTreeSet<_> = source_rows.iter().map(|source| source.id.clone()).collect();
    let configuration_names: Vec<_> = graphs.iter().map(|graph| graph.configuration_id.clone()).collect();
    let manifest = Manifest {
        schema_version: 1,
        generator: Generator {
            name: "localhold-dependency-unsafe",
            version: request.tool_version.to_owned(),
            cargo: request.matrix.cargo_version.clone(),
            rustc: request.matrix.rustc_version.clone(),
        },
        platform: ReportPlatform {
            name: request.platform.name.clone(),
            target: request.platform.target.clone(),
        },
        inputs: Inputs {
            audit_tool_lock: sha256_file(&request.workspace.join("tools/dependency-unsafe/Cargo.lock"))?,
            audit_tool_source: hash_tool_source(request.workspace)?,
        },
        source_records: source_rows.len(),
        configurations: configuration_names,
    };
    let mut files = BTreeMap::from([
        ("manifest.json".to_owned(), pretty_json(&manifest)?),
        ("sources.jsonl".to_owned(), json_lines(source_rows)?),
    ]);
    for graph in graphs {
        let bytes = configuration_lines(graph, request.platform, &exposed)?;
        files.insert(format!("{}.jsonl", graph.configuration_id), bytes);
    }
    Ok(GeneratedArtifacts::new(files))
}

fn configuration_lines(graph: &ResolvedGraph, platform: &PlatformConfig, exposed: &BTreeSet<String>) -> Result<Vec<u8>> {
    let header = ConfigurationHeader {
        record: "configuration",
        id: graph.configuration_id.clone(),
        target: platform.target.clone(),
        profile: graph.profile.clone(),
        feature_mode: if graph.all_features {
            "all-features"
        } else if graph.requested_features.is_empty() {
            "default"
        } else {
            "explicit"
        },
        requested_features: graph.requested_features.clone(),
        edge_kinds: if graph.include_dev { vec!["normal", "build", "dev"] } else { vec!["normal", "build"] },
    };
    let paths = canonical_paths(graph)?;
    let packages = graph
        .packages
        .values()
        .filter(|package| exposed.contains(&package.source_id))
        .map(|package| {
            Ok(ConfigurationPackage {
                record: "package",
                source_id: package.source_id.clone(),
                enabled_features: package.features.clone(),
                dependency_path: paths
                    .get(&package.source_id)
                    .cloned()
                    .with_context(|| format!("no root path reaches {}", package.source_id))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut adjacency = BTreeMap::<String, Vec<String>>::new();
    for (from, to) in &graph.edges {
        adjacency.entry(from.clone()).or_default().push(to.clone());
    }
    let dependencies: Vec<_> = adjacency
        .into_iter()
        .map(|(from, mut to)| {
            to.sort();
            to.dedup();
            DependencyAdjacency { record: "dependencies", from, to }
        })
        .collect();
    let mut bytes = json_line(&header)?;
    append_json_lines(&mut bytes, &packages)?;
    append_json_lines(&mut bytes, &dependencies)?;
    Ok(bytes)
}

type DependencyPaths = BTreeMap<String, Vec<String>>;

fn canonical_paths(graph: &ResolvedGraph) -> Result<DependencyPaths> {
    let mut outgoing = BTreeMap::<String, Vec<String>>::new();
    for (from, to) in &graph.edges {
        outgoing.entry(from.clone()).or_default().push(to.clone());
    }
    for dependencies in outgoing.values_mut() {
        dependencies.sort();
        dependencies.dedup();
    }
    let mut paths = BTreeMap::from([(graph.root_label.clone(), vec![graph.root_label.clone()])]);
    let mut queue = VecDeque::from([graph.root_label.clone()]);
    while let Some(current) = queue.pop_front() {
        let current_path = paths.get(&current).cloned().with_context(|| format!("missing path for queued package {current}"))?;
        for dependency in outgoing.get(&current).into_iter().flatten() {
            if paths.contains_key(dependency) {
                continue;
            }
            let mut path = current_path.clone();
            path.push(dependency.clone());
            paths.insert(dependency.clone(), path);
            queue.push_back(dependency.clone());
        }
    }
    let expected: BTreeSet<_> = graph.packages.values().map(|package| package.source_id.clone()).collect();
    let observed: BTreeSet<_> = paths.keys().filter(|package| *package != &graph.root_label).cloned().collect();
    if expected != observed {
        bail!("dependency graph contains unreachable or missing packages");
    }
    Ok(paths)
}

fn hash_tool_source(workspace: &Path) -> Result<String> {
    let root = workspace.join("tools/dependency-unsafe");
    let mut files = vec![root.join("Cargo.toml")];
    collect_top_level_rust_sources(&root, &mut files)?;
    let source = root.join("src");
    collect_source_inputs(&source, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for path in files {
        let relative = path.strip_prefix(&root).with_context(|| format!("tool source {} escaped root", path.display()))?;
        hasher.update(relative.to_string_lossy().replace('\\', "/").as_bytes());
        hasher.update([0]);
        hasher.update(fs::read(&path).with_context(|| format!("read tool source {}", path.display()))?);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_top_level_rust_sources(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry?;
        let file_type = entry.file_type().with_context(|| format!("inspect {}", entry.path().display()))?;
        let is_rust_source = entry.path().extension().and_then(|extension| extension.to_str()) == Some("rs");
        if file_type.is_symlink() && is_rust_source {
            bail!("audit tool root contains symlink {}", entry.path().display());
        }
        if file_type.is_file() && is_rust_source {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn collect_source_inputs(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry?;
        let file_type = entry.file_type().with_context(|| format!("inspect {}", entry.path().display()))?;
        if file_type.is_symlink() {
            bail!("audit tool source contains symlink {}", entry.path().display());
        }
        if file_type.is_dir() {
            if entry.file_name() == "tests" {
                continue;
            }
            collect_source_inputs(&entry.path(), files)?;
        } else if file_type.is_file() {
            files.push(entry.path());
        } else {
            bail!("audit tool source contains unsupported entry {}", entry.path().display());
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
#[path = "tests/report.rs"]
mod tests;
