use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use self::source::SourceScanner;
use super::classify::Inventory;

mod modules;
mod policy;
mod source;
mod targets;
#[cfg(test)]
mod tests;
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
    let mut target_sources = BTreeMap::new();
    for (path, category) in modules::expand_target_sources(workspace, targets::root_package_target_sources(workspace)?, |path| component_paths.contains_key(path))? {
        if component_paths.contains_key(path.as_str()) || revision_contains_path(workspace, revision, &path)? {
            target_sources.insert(path, category);
        }
    }
    scan_with(inventory, component_paths, &target_sources, |path| {
        let object = format!("{revision}:{path}");
        let output = Command::new("git")
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
    targets::reject_tooling_target_escapes(workspace, &manifests)?;
    for path in tooling_paths
        .into_iter()
        .filter(|path| Path::new(path).extension().and_then(|extension| extension.to_str()) == Some("rs"))
    {
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
    target_sources: &BTreeMap<String, SourceCategory>,
    mut read_source: impl FnMut(&str) -> Result<String>,
) -> Result<Vec<SourceSuppression>> {
    let measurements = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    let mut sites = Vec::new();
    for (&path, &component) in component_paths {
        let measurement = measurements
            .get(path)
            .with_context(|| format!("suppression inventory path {path:?} has no structural measurement"))?;
        let category = if path.starts_with("benches/") {
            SourceCategory::Benchmark
        } else if path.starts_with("tests/") || measurement.physical_lines > 0 && measurement.production_lines == 0 {
            SourceCategory::Test
        } else {
            SourceCategory::Production
        };
        let source = read_source(path)?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse suppression source {path}"))?;
        sites.extend(SourceScanner::scan(path, component, category, &syntax)?);
    }
    for (path, &category) in target_sources {
        if component_paths.contains_key(path.as_str()) {
            continue;
        }
        let source = read_source(path)?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse Cargo target suppression source {path}"))?;
        sites.extend(SourceScanner::scan(path, "cargo-target", category, &syntax)?);
    }
    sites.sort();
    Ok(sites)
}

fn revision_contains_path(workspace: &Path, revision: &str, path: &str) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["ls-tree", "--name-only", "-z", revision, "--", path])
        .output()
        .with_context(|| format!("inspect Cargo target source {path:?} in revision {revision}"))?;
    if !output.status.success() {
        bail!("git ls-tree failed while inspecting Cargo target source {path:?} in revision {revision}");
    }
    if output.stdout.is_empty() {
        return Ok(false);
    }
    let expected = format!("{path}\0");
    if output.stdout != expected.as_bytes() {
        bail!("git ls-tree returned an unexpected Cargo target source path");
    }
    Ok(true)
}

fn tooling_paths(workspace: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
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
