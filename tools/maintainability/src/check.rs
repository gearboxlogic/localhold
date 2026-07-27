use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::manifest::{DependencyPin, RootDependencySpec, UnsafeManifest};
use crate::scan;

type RequiredLint<'a> = (&'a str, &'a str, &'a str, i64);

#[derive(Debug, Deserialize)]
struct Lockfile {
    package: Vec<LockedPackage>,
}

#[derive(Clone, Debug, Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

pub fn run(workspace: &Path, manifest: &UnsafeManifest) -> Result<()> {
    let actual = scan::scan_workspace(workspace, manifest.roots())?;
    manifest.compare_sites(&actual)?;
    verify_cargo_contract(workspace, manifest)?;
    verify_dependency_packages(workspace, manifest)
}

fn verify_cargo_contract(workspace: &Path, manifest: &UnsafeManifest) -> Result<()> {
    verify_no_workspace_cargo_config(workspace)?;
    let path = workspace.join("Cargo.toml");
    let source = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let cargo: toml::Value = toml::from_str(&source).with_context(|| format!("parse {}", path.display()))?;
    let required_lints: Vec<_> = manifest.required_lints().collect();
    for (table, name, level, priority) in &required_lints {
        verify_lint(&cargo, table, name, level, *priority)?;
    }
    verify_lint_precedence(&cargo, &required_lints)?;

    let root_dependency_specs = manifest.root_dependency_specs()?;
    for (name, required) in &root_dependency_specs {
        let value = cargo
            .get("dependencies")
            .and_then(|dependencies| dependencies.get(name))
            .with_context(|| format!("Cargo.toml is missing unsafe-contract dependency {name}"))?;
        let configured = parse_root_dependency(value)?;
        if &configured != *required {
            bail!("Cargo.toml dependency {name} changed: expected {required:?}, found {configured:?}");
        }
    }
    let reviewed_root_dependencies = root_dependency_specs.keys().copied().collect();
    let protected_dependencies = manifest.dependency_packages()?.keys().copied().collect();
    verify_dependency_routes(&cargo, &protected_dependencies, &reviewed_root_dependencies)?;
    verify_cargo_target_paths(&cargo)?;
    Ok(())
}

fn verify_no_workspace_cargo_config(workspace: &Path) -> Result<()> {
    for relative in [".cargo/config.toml", ".cargo/config"] {
        let path = workspace.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(_) => bail!("{relative} is not supported because repository Cargo configuration can override safety lint flags"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        }
    }
    Ok(())
}

fn verify_lint(cargo: &toml::Value, table: &str, name: &str, required_level: &str, required_priority: i64) -> Result<()> {
    let value = cargo
        .get("lints")
        .and_then(|lints| lints.get(table))
        .and_then(|lints| lints.get(name))
        .with_context(|| format!("Cargo.toml must configure [lints.{table}] {name}"))?;
    let (level, priority) = lint_setting(value).with_context(|| format!("parse Cargo.toml lint {table}.{name}"))?;
    if level != required_level || priority != required_priority {
        bail!("Cargo.toml lint {table}.{name} must remain level {required_level:?} at priority {required_priority}, found level {level:?} at priority {priority}");
    }
    Ok(())
}

fn lint_setting(value: &toml::Value) -> Result<(&str, i64)> {
    if let Some(level) = value.as_str() {
        return Ok((level, 0));
    }
    let table = value.as_table().context("lint setting must be a level string or table")?;
    let level = table.get("level").and_then(toml::Value::as_str).context("lint setting table needs a string level")?;
    let priority = table
        .get("priority")
        .map_or(Ok(0), |priority| priority.as_integer().context("lint priority must be an integer"))?;
    Ok((level, priority))
}

fn verify_lint_precedence(cargo: &toml::Value, required: &[RequiredLint<'_>]) -> Result<()> {
    let tables: BTreeSet<_> = required.iter().map(|(table, _, _, _)| *table).collect();
    for table_name in tables {
        let required_names: BTreeSet<_> = required.iter().filter_map(|(table, name, _, _)| (*table == table_name).then_some(*name)).collect();
        let reserved_priority = required
            .iter()
            .filter_map(|(table, _, _, priority)| (*table == table_name).then_some(*priority))
            .min()
            .context("required lint table has no reserved priority")?;
        let configured = cargo
            .get("lints")
            .and_then(|lints| lints.get(table_name))
            .and_then(toml::Value::as_table)
            .with_context(|| format!("Cargo.toml must configure [lints.{table_name}]"))?;
        for (name, value) in configured {
            if required_names.contains(name.as_str()) {
                continue;
            }
            let (_, priority) = lint_setting(value).with_context(|| format!("parse Cargo.toml lint {table_name}.{name}"))?;
            if priority >= reserved_priority {
                bail!("Cargo.toml lint {table_name}.{name} priority {priority} can override safety lints reserved at priority {reserved_priority}");
            }
        }
    }
    Ok(())
}

fn verify_cargo_target_paths(cargo: &toml::Value) -> Result<()> {
    if let Some(build) = cargo.get("package").and_then(|package| package.get("build")) {
        match build {
            toml::Value::Boolean(false) => {}
            toml::Value::String(path) => verify_audited_target_path("package.build", path)?,
            _ => bail!("Cargo.toml package.build must be false or an audited Rust path"),
        }
    }
    if let Some(library) = cargo.get("lib")
        && let Some(path) = library.get("path")
    {
        verify_audited_target_path("lib.path", path.as_str().context("Cargo.toml lib.path must be a string")?)?;
    }
    for target in ["bin", "example", "test", "bench"] {
        let Some(entries) = cargo.get(target) else {
            continue;
        };
        for entry in entries.as_array().with_context(|| format!("Cargo.toml [[{target}]] must be an array"))? {
            if let Some(path) = entry.get("path") {
                verify_audited_target_path(
                    &format!("{target}.path"),
                    path.as_str().with_context(|| format!("Cargo.toml {target}.path must be a string"))?,
                )?;
            }
        }
    }
    Ok(())
}

fn verify_audited_target_path(field: &str, value: &str) -> Result<()> {
    let path = Path::new(value);
    let mut components = path.components();
    let first = components.next();
    let audited_root = matches!(
        first,
        Some(Component::Normal(root)) if matches!(root.to_str(), Some("src" | "tests" | "benches" | "examples"))
    );
    if !audited_root || path.extension().and_then(|extension| extension.to_str()) != Some("rs") || components.any(|component| !matches!(component, Component::Normal(_))) {
        bail!("Cargo.toml {field} {value:?} must be a normalized Rust path under an audited source root");
    }
    Ok(())
}

fn verify_dependency_routes(cargo: &toml::Value, protected: &BTreeSet<&str>, reviewed_root: &BTreeSet<&str>) -> Result<()> {
    verify_dependency_table(cargo.get("dependencies"), "dependencies", protected, Some(reviewed_root))?;
    for section in ["build-dependencies", "dev-dependencies"] {
        verify_dependency_table(cargo.get(section), section, protected, None)?;
    }
    if let Some(targets) = cargo.get("target") {
        for (selector, target) in targets.as_table().context("Cargo.toml target must be a table")? {
            let target = target.as_table().with_context(|| format!("Cargo.toml target {selector:?} must be a table"))?;
            for section in ["dependencies", "build-dependencies", "dev-dependencies"] {
                verify_dependency_table(target.get(section), &format!("target.{selector:?}.{section}"), protected, None)?;
            }
        }
    }
    verify_dependency_feature_routes(cargo, protected)
}

fn verify_dependency_table(value: Option<&toml::Value>, label: &str, protected: &BTreeSet<&str>, reviewed_root: Option<&BTreeSet<&str>>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    for (key, declaration) in value.as_table().with_context(|| format!("Cargo.toml {label} must be a table"))? {
        if declaration.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
            bail!("Cargo.toml {label}.{key} uses unsupported workspace dependency inheritance, which can hide package and feature routes");
        }
        let package = dependency_package_name(key, declaration).with_context(|| format!("parse Cargo.toml {label}.{key}"))?;
        let is_reviewed_root = reviewed_root.is_some_and(|reviewed| key == package && reviewed.contains(package));
        if protected.contains(package) && !is_reviewed_root {
            bail!("Cargo.toml {label}.{key} adds an unreviewed route to unsafe-contract dependency {package}");
        }
    }
    Ok(())
}

fn dependency_package_name<'a>(key: &'a str, declaration: &'a toml::Value) -> Result<&'a str> {
    if declaration.is_str() {
        return Ok(key);
    }
    let table = declaration.as_table().context("dependency declaration must be a version string or table")?;
    table
        .get("package")
        .map_or(Ok(key), |package| package.as_str().context("dependency package rename must be a string"))
}

fn verify_dependency_feature_routes(cargo: &toml::Value, required: &BTreeSet<&str>) -> Result<()> {
    let Some(features) = cargo.get("features") else {
        return Ok(());
    };
    for (feature, members) in features.as_table().context("Cargo.toml features must be a table")? {
        for member in members.as_array().with_context(|| format!("Cargo.toml feature {feature:?} must be an array"))? {
            let member = member.as_str().with_context(|| format!("Cargo.toml feature {feature:?} entries must be strings"))?;
            if dependency_feature_target(member).is_some_and(|dependency| required.contains(dependency)) {
                bail!("Cargo.toml feature {feature:?} forwards unreviewed feature route {member:?} to an unsafe-contract dependency");
            }
        }
    }
    Ok(())
}

fn dependency_feature_target(member: &str) -> Option<&str> {
    if let Some(dependency) = member.strip_prefix("dep:") {
        return Some(dependency);
    }
    let (dependency, _) = member.split_once('/')?;
    Some(dependency.strip_suffix('?').unwrap_or(dependency))
}

fn parse_root_dependency(value: &toml::Value) -> Result<RootDependencySpec> {
    if let Some(version) = value.as_str() {
        return Ok(RootDependencySpec {
            version: version.to_owned(),
            default_features: true,
            features: Vec::new(),
        });
    }
    let table = value.as_table().context("unsafe-contract root dependency must be a version string or table")?;
    let allowed = BTreeSet::from(["default-features", "features", "version"]);
    let keys: BTreeSet<_> = table.keys().map(String::as_str).collect();
    if !keys.is_subset(&allowed) {
        bail!("unsafe-contract root dependency has unsupported keys: {:?}", keys.difference(&allowed).collect::<Vec<_>>());
    }
    let version = table
        .get("version")
        .and_then(toml::Value::as_str)
        .context("unsafe-contract root dependency needs a version")?;
    let default_features = table
        .get("default-features")
        .map_or(Ok(true), |value| value.as_bool().context("unsafe-contract default-features must be Boolean"))?;
    let features = table.get("features").map_or(Ok(Vec::new()), |value| {
        value
            .as_array()
            .context("unsafe-contract features must be an array")?
            .iter()
            .map(|feature| feature.as_str().map(str::to_owned).context("unsafe-contract feature must be a string"))
            .collect()
    })?;
    Ok(RootDependencySpec {
        version: version.to_owned(),
        default_features,
        features,
    })
}

fn verify_dependency_packages(workspace: &Path, manifest: &UnsafeManifest) -> Result<()> {
    let path = workspace.join("Cargo.lock");
    let source = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let lockfile: Lockfile = toml::from_str(&source).with_context(|| format!("parse {}", path.display()))?;
    let required = manifest.dependency_packages()?;
    let required_names: BTreeSet<_> = required.keys().copied().collect();
    let mut observed: BTreeMap<&str, Vec<&LockedPackage>> = BTreeMap::new();
    for package in &lockfile.package {
        if required_names.contains(package.name.as_str()) {
            observed.entry(&package.name).or_default().push(package);
        }
    }
    compare_dependency_packages(&observed, &required)
}

fn compare_dependency_packages(observed: &BTreeMap<&str, Vec<&LockedPackage>>, required: &BTreeMap<&str, &DependencyPin>) -> Result<()> {
    for (name, pin) in required {
        let packages = observed.get(name).with_context(|| format!("Cargo.lock is missing unsafe-contract dependency {name}"))?;
        if packages.len() != 1 {
            bail!("Cargo.lock must contain exactly one unsafe-contract dependency {name}, found {}", packages.len());
        }
        let package = packages[0];
        if package.version != pin.version || package.source.as_deref() != Some(pin.source.as_str()) || package.checksum.as_deref() != Some(pin.checksum.as_str()) {
            bail!("unsafe-contract dependency {name} changed: expected {pin:?}, found {package:?}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
