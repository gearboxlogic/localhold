use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::parse_nul_paths;

type CargoAllowKey = (String, String, String, i64);
type CargoLintKey = (String, String, String);
type CargoManifests = BTreeMap<String, toml::Table>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CargoLintLevel {
    Allow,
    Warn,
    Deny,
    Forbid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CargoLintSetting {
    level: CargoLintLevel,
    priority: i64,
}

pub(super) fn scan_cargo_allows(workspace: &Path) -> Result<BTreeSet<CargoAllowKey>> {
    Ok(scan_cargo_lints(&load_current_manifests(workspace)?)?
        .into_iter()
        .filter_map(|((manifest, family, lint), setting)| (setting.level == CargoLintLevel::Allow).then_some((manifest, family, lint, setting.priority)))
        .collect())
}

pub(in crate::structure::suppression::policy) fn compare_cargo_lint_levels_previous_revision(workspace: &Path, revision: &str) -> Result<()> {
    let current = scan_cargo_lints(&load_current_manifests(workspace)?)?;
    let previous = scan_cargo_lints(&load_revision_manifests(workspace, revision)?)?;
    compare_enabled_lints(&current, &previous)
}

fn load_current_manifests(workspace: &Path) -> Result<CargoManifests> {
    let mut manifests = BTreeMap::new();
    for manifest in tracked_manifests(workspace)? {
        let path = workspace.join(&manifest);
        if fs::symlink_metadata(&path)
            .with_context(|| format!("inspect Cargo manifest {}", path.display()))?
            .file_type()
            .is_symlink()
        {
            bail!("Cargo lint manifest cannot be a symlink: {manifest}");
        }
        manifests.insert(manifest, read_manifest(&path)?);
    }
    Ok(manifests)
}

fn load_revision_manifests(workspace: &Path, revision: &str) -> Result<CargoManifests> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-tree", "-r", "-z", "--name-only", revision])
        .output()
        .context("list Cargo manifests in maintainability base revision")?;
    if !output.status.success() {
        bail!("git ls-tree failed while listing Cargo lint manifests in maintainability base revision");
    }
    let paths = parse_nul_paths(&output.stdout, |path| path == "Cargo.toml" || path.ends_with("/Cargo.toml"))?;
    paths
        .into_iter()
        .map(|manifest| {
            let object = format!("{revision}:{manifest}");
            let output = crate::structure::revision::git_command()
                .current_dir(workspace)
                .args(["show", "--no-ext-diff", &object])
                .output()
                .with_context(|| format!("read Cargo manifest {manifest} from maintainability base revision"))?;
            if !output.status.success() {
                bail!("Cargo manifest {manifest} is unreadable in maintainability base revision");
            }
            let source = std::str::from_utf8(&output.stdout).with_context(|| format!("Cargo manifest {manifest} in maintainability base revision is not UTF-8"))?;
            let parsed = source
                .parse::<toml::Table>()
                .with_context(|| format!("parse Cargo manifest {manifest} from maintainability base revision"))?;
            Ok((manifest, parsed))
        })
        .collect()
}

fn scan_cargo_lints(manifests: &CargoManifests) -> Result<BTreeMap<CargoLintKey, CargoLintSetting>> {
    let mut observed = BTreeMap::new();
    for (manifest, parsed) in manifests {
        let Some(lints) = parsed.get("lints").and_then(toml::Value::as_table) else {
            continue;
        };
        record_manifest_lints(manifests, manifest, parsed, lints, &mut observed)?;
    }
    Ok(observed)
}

fn record_manifest_lints(
    manifests: &CargoManifests,
    manifest: &str,
    parsed: &toml::Table,
    lints: &toml::Table,
    observed: &mut BTreeMap<CargoLintKey, CargoLintSetting>,
) -> Result<()> {
    if let Some(marker) = lints.get("workspace") {
        if marker.as_bool() != Some(true) {
            bail!("Cargo lint workspace inheritance marker in {manifest} must be true");
        }
        let inherited = find_workspace_lints(manifests, manifest, parsed)?;
        record_lint_families(manifest, &inherited, observed)?;
    }
    let local = lints.iter().filter(|(family, _)| family.as_str() != "workspace");
    record_lint_families(manifest, local, observed)
}

fn record_lint_families<'a>(
    manifest: &str,
    families: impl IntoIterator<Item = (&'a String, &'a toml::Value)>,
    observed: &mut BTreeMap<CargoLintKey, CargoLintSetting>,
) -> Result<()> {
    for (family, settings) in families {
        let settings = settings.as_table().with_context(|| format!("Cargo lint family {family:?} in {manifest} is not a table"))?;
        for (lint, setting) in settings {
            let (level, priority) = lint_setting(setting).with_context(|| format!("parse Cargo lint {family}::{lint} in {manifest}"))?;
            let key = (manifest.to_owned(), family.clone(), lint.clone());
            if observed.insert(key, CargoLintSetting { level, priority }).is_some() {
                bail!("Cargo lint {family}::{lint} is configured more than once for {manifest}");
            }
        }
    }
    Ok(())
}

fn find_workspace_lints(manifests: &CargoManifests, manifest: &str, parsed: &toml::Table) -> Result<toml::Table> {
    if let Some(lints) = workspace_lints(parsed, manifest)? {
        return Ok(lints);
    }
    if let Some(explicit) = explicit_workspace_manifest(manifest, parsed)? {
        let workspace_manifest = manifests
            .get(&explicit)
            .with_context(|| format!("explicit Cargo workspace {explicit} selected by {manifest} is not in the audited manifest inventory"))?;
        return workspace_lints(workspace_manifest, &explicit)?.with_context(|| format!("explicit Cargo workspace {explicit} selected by {manifest} does not define a workspace"));
    }
    let manifest_path = Path::new(manifest);
    let parent = manifest_path.parent().unwrap_or_else(|| Path::new(""));
    for ancestor in parent.ancestors().skip(1) {
        let candidate = ancestor.join("Cargo.toml");
        let candidate = candidate.to_string_lossy();
        let Some(parsed) = manifests.get(candidate.as_ref()) else {
            continue;
        };
        if let Some(lints) = workspace_lints(parsed, &candidate)? {
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
        .iter()
        .map(|component| component.to_str())
        .collect::<Option<Vec<_>>>()
        .map(|components| components.join("/"))
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

fn lint_setting(value: &toml::Value) -> Result<(CargoLintLevel, i64)> {
    if let Some(level) = value.as_str() {
        return Ok((parse_lint_level(level)?, 0));
    }
    let table = value.as_table().context("lint setting must be a string or a table")?;
    let level = table.get("level").and_then(toml::Value::as_str).context("lint setting table must contain a string level")?;
    let priority = table
        .get("priority")
        .map_or(Ok(0), |priority| priority.as_integer().context("lint priority must be an integer"))?;
    Ok((parse_lint_level(level)?, priority))
}

fn parse_lint_level(level: &str) -> Result<CargoLintLevel> {
    match level {
        "allow" => Ok(CargoLintLevel::Allow),
        "warn" => Ok(CargoLintLevel::Warn),
        "deny" => Ok(CargoLintLevel::Deny),
        "forbid" => Ok(CargoLintLevel::Forbid),
        _ => bail!("unsupported Cargo lint level {level:?}"),
    }
}

fn compare_enabled_lints(current: &BTreeMap<CargoLintKey, CargoLintSetting>, previous: &BTreeMap<CargoLintKey, CargoLintSetting>) -> Result<()> {
    for (key, previous) in previous.iter().filter(|(_, setting)| setting.level != CargoLintLevel::Allow) {
        let current = current.get(key).with_context(|| format!("enabled Cargo lint {key:?} cannot be removed"))?;
        if current.level < previous.level {
            bail!("enabled Cargo lint {key:?} cannot be weakened from {:?} to {:?}", previous.level, current.level);
        }
        if current.priority != previous.priority {
            bail!("enabled Cargo lint {key:?} priority cannot change from {} to {}", previous.priority, current.priority);
        }
    }
    Ok(())
}
