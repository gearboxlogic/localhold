use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub(super) fn root_package_target_sources(workspace: &Path) -> Result<BTreeSet<String>> {
    let workspace = fs::canonicalize(workspace).context("resolve workspace for Cargo target inventory")?;
    let manifest = fs::canonicalize(workspace.join("Cargo.toml")).context("resolve root package manifest for Cargo target inventory")?;
    let output = Command::new("cargo")
        .current_dir(&workspace)
        .args(["metadata", "--no-deps", "--locked", "--format-version", "1", "--manifest-path"])
        .arg(&manifest)
        .output()
        .context("run Cargo metadata for suppression target inventory")?;
    if !output.status.success() {
        bail!("Cargo metadata failed while listing suppression target sources");
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).context("parse Cargo metadata for suppression target inventory")?;
    let packages = metadata.get("packages").and_then(Value::as_array).context("Cargo metadata has no package list")?;
    let matching = packages
        .iter()
        .filter(|package| package_manifest(package).is_ok_and(|path| path == manifest))
        .collect::<Vec<_>>();
    let [package] = matching.as_slice() else {
        bail!("Cargo metadata must contain exactly one root package");
    };
    let targets = package.get("targets").and_then(Value::as_array).context("root package metadata has no target list")?;
    let mut sources = BTreeSet::new();
    for target in targets {
        let source = target.get("src_path").and_then(Value::as_str).context("root package target has no source path")?;
        let path = checked_target_path(&workspace, Path::new(source))?;
        validate_target_kinds(target)?;
        sources.insert(path);
    }
    Ok(sources)
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

fn validate_target_kinds(target: &Value) -> Result<()> {
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
    Ok(())
}
