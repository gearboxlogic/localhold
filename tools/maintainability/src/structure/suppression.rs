use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use self::source::SourceScanner;
use super::classify::Inventory;

mod direct;
mod modules;
mod policy;
mod source;
mod targets;
#[cfg(test)]
mod tests;
pub(in crate::structure::suppression) use direct::reject_direct_source_suppressions;
pub(super) use policy::SuppressionPolicy;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceCategory {
    Production,
    Test,
    Benchmark,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceSuppression {
    pub id: String,
    pub path: String,
    pub component: String,
    pub item: String,
    pub scope: String,
    pub signature: Option<String>,
    pub target: Option<String>,
    pub category: SourceCategory,
    pub level: String,
    pub lint: String,
    pub reason: String,
    pub macro_carried: bool,
    pub occurrence: usize,
    pub fingerprint: String,
}

impl SourceSuppression {
    fn stable_id(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"localhold-lint-suppression-v2");
        for field in [
            self.component.as_str(),
            self.item.as_str(),
            self.scope.as_str(),
            self.signature.as_deref().unwrap_or("<no-signature>"),
            self.target.as_deref().unwrap_or("<no-target>"),
            self.category.as_str(),
            self.level.as_str(),
            self.lint.as_str(),
            self.reason.as_str(),
            self.fingerprint.as_str(),
        ] {
            digest.update(field.len().to_string().as_bytes());
            digest.update(b":");
            digest.update(field.as_bytes());
        }
        digest.update([u8::from(self.macro_carried)]);
        format!("source.{:x}", digest.finalize())
    }
}

impl SourceCategory {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Test => "test",
            Self::Benchmark => "benchmark",
        }
    }
}

pub fn scan_workspace(workspace: &Path, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<Vec<SourceSuppression>> {
    let target_sources = modules::expand_target_sources(workspace, targets::root_package_target_sources(workspace)?, |path| component_paths.contains_key(path))?;
    scan_with(inventory, component_paths, &target_sources, |path| {
        let source_path = workspace.join(path);
        fs::read_to_string(&source_path).with_context(|| format!("read suppression source {}", source_path.display()))
    })
}

pub fn scan_revision(workspace: &Path, revision: &str, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<Vec<SourceSuppression>> {
    let targets::RevisionTargets { roots, rust_sources } = targets::revision_root_package_target_sources(workspace, revision)?;
    let target_sources = modules::expand_revision_target_sources(workspace, revision, roots, &rust_sources, |path| component_paths.contains_key(path))?;
    scan_with(inventory, component_paths, &target_sources, |path| {
        let object = format!("{revision}:{path}");
        let output = crate::structure::revision::git_command()
            .current_dir(workspace)
            .args(["show", "--no-ext-diff", &object])
            .output()
            .with_context(|| format!("read suppression source {path:?} from revision"))?;
        if !output.status.success() {
            anyhow::bail!("cannot read suppression source {path:?} from revision {revision}");
        }
        String::from_utf8(output.stdout).with_context(|| format!("suppression source {path:?} from revision is not UTF-8"))
    })
}

pub(super) fn reject_tooling_suppressions(workspace: &Path) -> Result<()> {
    let tools_root = fs::canonicalize(workspace.join("tools")).context("resolve maintainer-tool source root")?;
    let tooling_paths = tooling_paths(workspace)?;
    let manifests = tooling_paths
        .iter()
        .filter(|path| Path::new(path).file_name().and_then(|name| name.to_str()) == Some("Cargo.toml"))
        .cloned()
        .collect::<Vec<_>>();
    let target_paths = targets::tooling_target_sources(workspace, &manifests)?;
    let scan_paths = tooling_paths
        .into_iter()
        .filter(|path| Path::new(path).extension().and_then(|extension| extension.to_str()) == Some("rs"))
        .chain(target_paths)
        .collect::<BTreeSet<_>>();
    for path in scan_paths {
        let absolute = workspace.join(&path);
        let metadata = fs::symlink_metadata(&absolute).with_context(|| format!("inspect maintainer-tool source {}", absolute.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("maintainer-tool Rust source must be a regular non-symlink file: {path:?}");
        }
        let canonical = fs::canonicalize(&absolute).with_context(|| format!("resolve maintainer-tool source {}", absolute.display()))?;
        if !canonical.starts_with(&tools_root) {
            bail!("maintainer-tool Rust source escapes the tools root: {path:?}");
        }
        let source = fs::read_to_string(&absolute).with_context(|| format!("read maintainer-tool source {}", absolute.display()))?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse maintainer-tool source {path}"))?;
        let sites = SourceScanner::scan(&path, "maintainer-tooling", SourceCategory::Production, &syntax)?;
        if let Some(site) = sites.first() {
            bail!(
                "maintainer-tool Rust must remain suppression-free; remove source suppression {} from {:?}",
                site.id,
                site.path
            );
        }
    }
    Ok(())
}

fn scan_with(
    inventory: &Inventory,
    component_paths: &BTreeMap<&str, &str>,
    target_sources: &modules::ExpandedSources,
    mut read_source: impl FnMut(&str) -> Result<String>,
) -> Result<Vec<SourceSuppression>> {
    let measurements = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    let paths = component_paths
        .keys()
        .copied()
        .map(str::to_owned)
        .chain(target_sources.categories.keys().cloned())
        .collect::<BTreeSet<_>>();
    let mut syntax = BTreeMap::new();
    for path in paths {
        let source = read_source(&path)?;
        let parsed = syn::parse_file(&source).with_context(|| format!("parse suppression source {path}"))?;
        syntax.insert(path, parsed);
    }
    let external_targets = source::external_target_maps(&target_sources.relations, &syntax)?;
    let mut sites = Vec::new();
    for (&path, &component) in component_paths {
        let measurement = measurements
            .get(path)
            .with_context(|| format!("suppression inventory path {path:?} has no structural measurement"))?;
        let category = target_sources.categories.get(path).copied().unwrap_or_else(|| {
            if path.starts_with("benches/") {
                SourceCategory::Benchmark
            } else if path.starts_with("tests/") || measurement.physical_lines > 0 && measurement.production_lines == 0 {
                SourceCategory::Test
            } else {
                SourceCategory::Production
            }
        });
        let parsed = syntax.get(path).with_context(|| format!("suppression source cache is missing {path:?}"))?;
        let targets = external_targets.get(path).cloned().unwrap_or_default();
        sites.extend(SourceScanner::scan_with_external_targets(path, component, category, parsed, &targets)?);
    }
    for (path, &category) in &target_sources.categories {
        if component_paths.contains_key(path.as_str()) {
            continue;
        }
        let parsed = syntax.get(path).with_context(|| format!("Cargo target suppression source cache is missing {path:?}"))?;
        let targets = external_targets.get(path).cloned().unwrap_or_default();
        let component = target_sources.target_component(path)?;
        sites.extend(SourceScanner::scan_with_external_targets(path, &component, category, parsed, &targets)?);
    }
    sites.sort();
    Ok(sites)
}

fn tooling_paths(workspace: &Path) -> Result<Vec<String>> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "tools"])
        .output()
        .context("list maintainer-tool Rust sources")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing maintainer-tool Rust sources");
    }
    let mut paths = Vec::new();
    for raw in output.stdout.split(|byte| *byte == b'\0').filter(|path| !path.is_empty()) {
        let path = std::str::from_utf8(raw).context("maintainer-tool source path is not UTF-8")?;
        let relative = Path::new(path);
        if relative.is_absolute() || !relative.starts_with("tools") || relative.components().any(|component| !matches!(component, Component::Normal(_))) {
            bail!("maintainer-tool path must be normalized under tools/: {path:?}");
        }
        paths.push(path.to_owned());
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}
