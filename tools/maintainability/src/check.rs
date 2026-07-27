use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::manifest::{DependencyPin, RootDependencySpec, UnsafeManifest};
use crate::scan;

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
    let path = workspace.join("Cargo.toml");
    let source = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let cargo: toml::Value = toml::from_str(&source).with_context(|| format!("parse {}", path.display()))?;
    for (table, name, required) in manifest.required_lints() {
        verify_lint(&cargo, table, name, required)?;
    }
    for (name, required) in manifest.root_dependency_specs()? {
        let value = cargo
            .get("dependencies")
            .and_then(|dependencies| dependencies.get(name))
            .with_context(|| format!("Cargo.toml is missing unsafe-contract dependency {name}"))?;
        let configured = parse_root_dependency(value)?;
        if &configured != required {
            bail!("Cargo.toml dependency {name} changed: expected {required:?}, found {configured:?}");
        }
    }
    verify_cargo_target_paths(&cargo)?;
    Ok(())
}

fn verify_lint(cargo: &toml::Value, table: &str, name: &str, required: &str) -> Result<()> {
    let configured = cargo
        .get("lints")
        .and_then(|lints| lints.get(table))
        .and_then(|lints| lints.get(name))
        .and_then(lint_level)
        .with_context(|| format!("Cargo.toml must configure [lints.{table}] {name}"))?;
    if configured != required {
        bail!("Cargo.toml lint {table}.{name} must remain {required:?}, found {configured:?}");
    }
    Ok(())
}

fn lint_level(value: &toml::Value) -> Option<&str> {
    value.as_str().or_else(|| value.get("level").and_then(toml::Value::as_str))
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
mod tests {
    use std::collections::BTreeMap;

    use crate::manifest::DependencyPin;

    use super::{LockedPackage, compare_dependency_packages, lint_level, parse_root_dependency, verify_audited_target_path, verify_lint};

    #[test]
    fn lint_level_supports_cargo_string_and_table_forms() {
        let string: toml::Value = toml::from_str::<toml::Value>("value = 'deny'").expect("TOML").get("value").expect("value").clone();
        let table: toml::Value = toml::from_str::<toml::Value>("value = { level = 'deny', priority = -1 }")
            .expect("TOML")
            .get("value")
            .expect("value")
            .clone();
        assert_eq!(lint_level(&string), Some("deny"));
        assert_eq!(lint_level(&table), Some("deny"));
    }

    #[test]
    fn required_lint_rejects_missing_or_weakened_configuration() {
        let cargo: toml::Value = toml::from_str(
            "
            [lints.rust]
            unsafe_code = 'warn'
            ",
        )
        .expect("TOML");
        assert!(verify_lint(&cargo, "rust", "unsafe_code", "deny").is_err());
        assert!(verify_lint(&cargo, "rust", "unsafe_op_in_unsafe_fn", "deny").is_err());
    }

    #[test]
    fn dependency_contract_rejects_source_checksum_version_and_multiplicity_drift() {
        let pin = DependencyPin {
            name: "sqlite-vec".to_owned(),
            version: "0.1.9".to_owned(),
            source: "registry".to_owned(),
            checksum: "checksum".to_owned(),
        };
        let required = BTreeMap::from([("sqlite-vec", &pin)]);
        let package = LockedPackage {
            name: "sqlite-vec".to_owned(),
            version: "0.1.9".to_owned(),
            source: Some("registry".to_owned()),
            checksum: Some("checksum".to_owned()),
        };
        assert!(compare_dependency_packages(&BTreeMap::new(), &required).is_err());
        assert!(compare_dependency_packages(&BTreeMap::from([("sqlite-vec", vec![&package, &package])]), &required).is_err());
        assert!(compare_dependency_packages(&BTreeMap::from([("sqlite-vec", vec![&package])]), &required).is_ok());

        for drifted in [
            LockedPackage {
                version: "0.2.0".to_owned(),
                ..package.clone()
            },
            LockedPackage {
                source: Some("git".to_owned()),
                ..package.clone()
            },
            LockedPackage {
                checksum: Some("changed".to_owned()),
                ..package
            },
        ] {
            assert!(compare_dependency_packages(&BTreeMap::from([("sqlite-vec", vec![&drifted])]), &required).is_err());
        }
    }

    #[test]
    fn root_dependency_contract_rejects_feature_and_route_drift() {
        let string: toml::Value = toml::from_str::<toml::Value>("value = '0.1'").expect("TOML").get("value").expect("value").clone();
        let parsed = parse_root_dependency(&string).expect("string dependency");
        assert_eq!(parsed.version, "0.1");
        assert!(parsed.default_features);
        assert!(parsed.features.is_empty());

        let table: toml::Value = toml::from_str::<toml::Value>("value = { version = '0.40', default-features = false, features = ['backup', 'bundled'] }")
            .expect("TOML")
            .get("value")
            .expect("value")
            .clone();
        let parsed = parse_root_dependency(&table).expect("table dependency");
        assert!(!parsed.default_features);
        assert_eq!(parsed.features, ["backup", "bundled"]);

        let unsupported: toml::Value = toml::from_str::<toml::Value>("value = { version = '0.40', path = '../other' }")
            .expect("TOML")
            .get("value")
            .expect("value")
            .clone();
        assert!(parse_root_dependency(&unsupported).is_err());
    }

    #[test]
    fn cargo_target_paths_cannot_escape_audited_roots() {
        for allowed in ["src/main.rs", "tests/contract.rs", "benches/latency.rs", "examples/client.rs"] {
            assert!(verify_audited_target_path("target.path", allowed).is_ok());
        }
        for rejected in ["build.rs", "outside.rs", "src/../outside.rs", "/absolute.rs", "src/not-rust.txt"] {
            assert!(verify_audited_target_path("target.path", rejected).is_err());
        }
    }
}
