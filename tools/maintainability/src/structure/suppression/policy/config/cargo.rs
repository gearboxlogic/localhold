use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::parse_nul_paths;

type CargoAllowKey = (String, String, String);

pub(super) fn scan_cargo_allows(workspace: &Path) -> Result<BTreeSet<CargoAllowKey>> {
    let mut observed = BTreeSet::new();
    for manifest in tracked_manifests(workspace)? {
        let path = workspace.join(&manifest);
        if fs::symlink_metadata(&path)
            .with_context(|| format!("inspect Cargo manifest {}", path.display()))?
            .file_type()
            .is_symlink()
        {
            bail!("Cargo lint manifest cannot be a symlink: {manifest}");
        }
        let parsed = read_manifest(&path)?;
        let Some(lints) = parsed.get("lints").and_then(toml::Value::as_table) else {
            continue;
        };
        record_manifest_allows(workspace, &manifest, &parsed, lints, &mut observed)?;
    }
    Ok(observed)
}

fn record_manifest_allows(workspace: &Path, manifest: &str, parsed: &toml::Table, lints: &toml::Table, observed: &mut BTreeSet<CargoAllowKey>) -> Result<()> {
    if let Some(marker) = lints.get("workspace") {
        if marker.as_bool() != Some(true) {
            bail!("Cargo lint workspace inheritance marker in {manifest} must be true");
        }
        let inherited = find_workspace_lints(workspace, manifest, parsed)?;
        record_lint_families(manifest, &inherited, observed)?;
    }
    let local = lints.iter().filter(|(family, _)| family.as_str() != "workspace");
    record_lint_families(manifest, local, observed)
}

fn record_lint_families<'a>(manifest: &str, families: impl IntoIterator<Item = (&'a String, &'a toml::Value)>, observed: &mut BTreeSet<CargoAllowKey>) -> Result<()> {
    for (family, settings) in families {
        let settings = settings.as_table().with_context(|| format!("Cargo lint family {family:?} in {manifest} is not a table"))?;
        for (lint, setting) in settings {
            if lint_level(setting).with_context(|| format!("parse Cargo lint {family}::{lint} in {manifest}"))? == "allow" {
                observed.insert((manifest.to_owned(), family.clone(), lint.clone()));
            }
        }
    }
    Ok(())
}

fn find_workspace_lints(workspace: &Path, manifest: &str, parsed: &toml::Table) -> Result<toml::Table> {
    if let Some(lints) = workspace_lints(parsed, manifest)? {
        return Ok(lints);
    }
    if let Some(explicit) = explicit_workspace_manifest(manifest, parsed)? {
        let path = workspace.join(&explicit);
        let workspace_manifest = read_manifest(&path)?;
        return workspace_lints(&workspace_manifest, &explicit)?.with_context(|| format!("explicit Cargo workspace {explicit} selected by {manifest} does not define a workspace"));
    }
    let manifest_path = Path::new(manifest);
    let parent = manifest_path.parent().unwrap_or_else(|| Path::new(""));
    for ancestor in parent.ancestors().skip(1) {
        let candidate = ancestor.join("Cargo.toml");
        let path = workspace.join(&candidate);
        if !path.is_file() {
            continue;
        }
        let parsed = read_manifest(&path)?;
        if let Some(lints) = workspace_lints(&parsed, &candidate.to_string_lossy())? {
            return Ok(lints);
        }
    }
    bail!("Cargo manifest {manifest} inherits workspace lints but no enclosing workspace defines them")
}

fn explicit_workspace_manifest(manifest: &str, parsed: &toml::Table) -> Result<Option<String>> {
    let Some(package) = parsed.get("package") else {
        return Ok(None);
    };
    let package = package.as_table().with_context(|| format!("Cargo package in {manifest} is not a table"))?;
    let Some(explicit) = package.get("workspace") else {
        return Ok(None);
    };
    let explicit = explicit
        .as_str()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("Cargo package.workspace in {manifest} must be a non-empty string"))?;
    let parent = Path::new(manifest).parent().unwrap_or_else(|| Path::new(""));
    let root = resolve_repository_relative(parent, Path::new(explicit)).with_context(|| format!("resolve Cargo package.workspace {explicit:?} in {manifest}"))?;
    let workspace_manifest = root.join("Cargo.toml");
    workspace_manifest
        .to_str()
        .map(str::to_owned)
        .map(Some)
        .with_context(|| format!("explicit Cargo workspace path in {manifest} is not UTF-8"))
}

fn resolve_repository_relative(base: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.is_absolute() {
        bail!("absolute workspace paths are unsupported");
    }
    let mut resolved = base.to_path_buf();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => resolved.push(value),
            Component::ParentDir if resolved.pop() => {}
            Component::ParentDir => bail!("workspace path escapes the repository"),
            Component::RootDir | Component::Prefix(_) => bail!("absolute workspace paths are unsupported"),
        }
    }
    Ok(resolved)
}

fn workspace_lints(parsed: &toml::Table, manifest: &str) -> Result<Option<toml::Table>> {
    let Some(workspace) = parsed.get("workspace") else {
        return Ok(None);
    };
    let workspace = workspace.as_table().with_context(|| format!("Cargo workspace in {manifest} is not a table"))?;
    let lints = workspace.get("lints").map_or(Ok(toml::Table::new()), |lints| {
        lints.as_table().cloned().with_context(|| format!("Cargo workspace lints in {manifest} are not a table"))
    })?;
    Ok(Some(lints))
}

fn read_manifest(path: &Path) -> Result<toml::Table> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect Cargo manifest {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("Cargo lint manifest must be a regular non-symlink file: {}", path.display());
    }
    let source = fs::read_to_string(path).with_context(|| format!("read Cargo manifest {}", path.display()))?;
    source.parse::<toml::Table>().with_context(|| format!("parse Cargo manifest {}", path.display()))
}

pub(super) fn tracked_manifests(workspace: &Path) -> Result<Vec<String>> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "Cargo.toml", ":(glob)**/Cargo.toml"])
        .output()
        .context("list tracked and proposed Cargo manifests")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing Cargo lint manifests");
    }
    parse_nul_paths(&output.stdout, |path| path.ends_with("Cargo.toml"))
}

fn lint_level(value: &toml::Value) -> Result<&str> {
    if let Some(level) = value.as_str() {
        return Ok(level);
    }
    value
        .as_table()
        .and_then(|table| table.get("level"))
        .and_then(toml::Value::as_str)
        .context("lint setting must be a string or a table with a string level")
}
