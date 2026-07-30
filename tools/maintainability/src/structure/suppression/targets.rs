use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::SourceCategory;

mod revision;
pub(super) use revision::{RevisionTargets, revision_root_package_target_sources};

#[derive(Default)]
pub(super) struct TargetRoots {
    pub(super) categories: BTreeMap<String, SourceCategory>,
    pub(super) identities: BTreeMap<String, BTreeSet<String>>,
}

impl TargetRoots {
    pub(super) fn insert(&mut self, path: String, category: SourceCategory, identity: String) -> Result<()> {
        match self.categories.get(&path).copied() {
            None => {
                self.categories.insert(path.clone(), category);
            }
            Some(existing) if existing == category || existing == SourceCategory::Production => {}
            Some(_) if category == SourceCategory::Production => {
                self.categories.insert(path.clone(), category);
            }
            Some(_) => bail!("Cargo target source {path:?} has conflicting governance categories"),
        }
        self.identities.entry(path).or_default().insert(identity);
        Ok(())
    }
}

pub(super) fn root_package_target_sources(workspace: &Path) -> Result<TargetRoots> {
    let workspace = fs::canonicalize(workspace).context("resolve workspace for Cargo target inventory")?;
    let manifest = fs::canonicalize(workspace.join("Cargo.toml")).context("resolve root package manifest for Cargo target inventory")?;
    let metadata = cargo_metadata(&workspace, &manifest)?;
    let targets = package_targets(&metadata, &manifest)?;
    let mut sources = TargetRoots::default();
    for target in targets {
        let source = target.get("src_path").and_then(Value::as_str).context("root package target has no source path")?;
        let path = checked_target_path(&workspace, Path::new(source))?;
        let kind = target_kind(target)?;
        let category = target_category(target, kind)?;
        sources.insert(path.clone(), category, format!("{kind}:{path}"))?;
    }
    Ok(sources)
}

pub(super) fn tooling_target_sources(workspace: &Path, manifests: &[String]) -> Result<TargetRoots> {
    let workspace = fs::canonicalize(workspace).context("resolve workspace for maintainer Cargo target inventory")?;
    let tools = fs::canonicalize(workspace.join("tools")).context("resolve tools root for maintainer Cargo target inventory")?;
    let mut sources = TargetRoots::default();
    for relative in manifests {
        let manifest = fs::canonicalize(workspace.join(relative)).with_context(|| format!("resolve maintainer manifest {relative}"))?;
        if !manifest.starts_with(&tools) {
            bail!("maintainer Cargo manifest escapes the tools root: {relative:?}");
        }
        let metadata = cargo_metadata(&workspace, &manifest)?;
        for target in package_targets(&metadata, &manifest)? {
            let source = target.get("src_path").and_then(Value::as_str).context("maintainer Cargo target has no source path")?;
            let source = Path::new(source);
            if !source.is_absolute() {
                bail!("maintainer Cargo target source path must be absolute");
            }
            let source_metadata = fs::symlink_metadata(source).with_context(|| format!("inspect maintainer Cargo target source {}", source.display()))?;
            if source_metadata.file_type().is_symlink() || !source_metadata.is_file() {
                bail!("maintainer Cargo target source must be a regular non-symlink file: {}", source.display());
            }
            let source = fs::canonicalize(source).with_context(|| format!("resolve maintainer Cargo target source {}", source.display()))?;
            if !source.starts_with(&tools) {
                bail!("maintainer Cargo target source escapes the tools root: {}", source.display());
            }
            let relative = source
                .strip_prefix(&workspace)
                .with_context(|| format!("maintainer Cargo target source escapes the workspace: {}", source.display()))?;
            if relative.components().any(|component| !matches!(component, Component::Normal(_))) {
                bail!("maintainer Cargo target source path is not normalized: {}", relative.display());
            }
            let relative = relative.to_str().context("maintainer Cargo target source path is not UTF-8")?.replace('\\', "/");
            sources.insert(relative.clone(), SourceCategory::Production, format!("maintainer:{relative}"))?;
        }
    }
    Ok(sources)
}

fn cargo_metadata(workspace: &Path, manifest: &Path) -> Result<Value> {
    let output = crate::structure::revision::cargo_command()
        .current_dir(workspace)
        .args(["metadata", "--no-deps", "--locked", "--format-version", "1", "--manifest-path"])
        .arg(manifest)
        .output()
        .context("run Cargo metadata for suppression target inventory")?;
    if !output.status.success() {
        bail!("Cargo metadata failed while listing suppression target sources");
    }
    serde_json::from_slice(&output.stdout).context("parse Cargo metadata for suppression target inventory")
}

fn package_targets<'a>(metadata: &'a Value, manifest: &Path) -> Result<&'a [Value]> {
    let packages = metadata.get("packages").and_then(Value::as_array).context("Cargo metadata has no package list")?;
    let matching = packages
        .iter()
        .filter(|package| package_manifest(package).is_ok_and(|path| path == manifest))
        .collect::<Vec<_>>();
    let [package] = matching.as_slice() else {
        bail!("Cargo metadata must contain exactly one selected package");
    };
    let package_id = package.get("id").and_then(Value::as_str).context("selected Cargo package has no ID")?;
    let workspace_members = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .context("Cargo metadata has no workspace member list")?
        .iter()
        .map(|member| member.as_str().context("Cargo workspace member ID is not a string"))
        .collect::<Result<Vec<_>>>()?;
    if workspace_members.as_slice() != [package_id] {
        bail!("additional Cargo workspace packages are unsupported by suppression governance");
    }
    package
        .get("targets")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .context("selected package metadata has no target list")
}

fn package_manifest(package: &Value) -> Result<PathBuf> {
    let path = package.get("manifest_path").and_then(Value::as_str).context("Cargo package has no manifest path")?;
    fs::canonicalize(path).with_context(|| format!("resolve Cargo package manifest {path}"))
}

fn checked_target_path(workspace: &Path, path: &Path) -> Result<String> {
    if !path.is_absolute() {
        bail!("Cargo target source path must be absolute");
    }
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect Cargo target source {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("Cargo target source must be a regular non-symlink file: {}", path.display());
    }
    let canonical = fs::canonicalize(path).with_context(|| format!("resolve Cargo target source {}", path.display()))?;
    let relative = canonical
        .strip_prefix(workspace)
        .with_context(|| format!("Cargo target source escapes the root package: {}", canonical.display()))?;
    if relative.components().any(|component| !matches!(component, Component::Normal(_))) || relative.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        bail!("Cargo target source must be a normalized Rust path in the root package");
    }
    Ok(relative.to_str().context("Cargo target source path is not UTF-8")?.replace('\\', "/"))
}

fn target_kind(target: &Value) -> Result<&'static str> {
    let kinds = target
        .get("kind")
        .and_then(Value::as_array)
        .context("root package target has no kind list")?
        .iter()
        .map(|kind| kind.as_str().context("root package target kind is not a string"))
        .collect::<Result<Vec<_>>>()?;
    if kinds.is_empty() {
        bail!("root package target kind list is empty");
    }
    for kind in ["bench", "test", "example", "custom-build", "bin"] {
        if kinds.contains(&kind) {
            return Ok(kind);
        }
    }
    kinds
        .iter()
        .all(|kind| matches!(*kind, "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"))
        .then_some("lib")
        .context("root package target has an unsupported kind")
}

fn target_category(target: &Value, kind: &str) -> Result<SourceCategory> {
    Ok(match kind {
        "bench" => SourceCategory::Benchmark,
        "test" => SourceCategory::Test,
        "bin" if target_requires_testing(target)? => SourceCategory::Test,
        _ => SourceCategory::Production,
    })
}

fn target_requires_testing(target: &Value) -> Result<bool> {
    let Some(required) = target.get("required-features") else {
        return Ok(false);
    };
    let required = required.as_array().context("root package target required-features must be a list")?;
    required.iter().try_fold(false, |requires_testing, feature| {
        let feature = feature.as_str().context("root package target required-feature is not a string")?;
        Ok(requires_testing || feature == "testing")
    })
}
