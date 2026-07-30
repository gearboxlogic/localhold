use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::SourceCategory;

#[derive(Clone, Copy)]
struct DeclaredTargetKind {
    name: &'static str,
    directory: &'static str,
    category: SourceCategory,
}

pub(in crate::structure::suppression) struct RevisionTargets {
    pub(in crate::structure::suppression) roots: BTreeMap<String, SourceCategory>,
    pub(in crate::structure::suppression) rust_sources: BTreeSet<String>,
}

pub(in crate::structure::suppression) fn revision_root_package_target_sources(workspace: &Path, revision: &str) -> Result<RevisionTargets> {
    validate_revision(revision)?;
    let rust_sources = revision_rust_sources(workspace, revision)?;
    let manifest = revision_manifest(workspace, revision)?;
    let roots = manifest_target_sources(&manifest, &rust_sources)?;
    Ok(RevisionTargets { roots, rust_sources })
}

fn revision_rust_sources(workspace: &Path, revision: &str) -> Result<BTreeSet<String>> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["ls-tree", "-r", "-z", "--full-tree", revision])
        .output()
        .context("list Rust sources in suppression comparison revision")?;
    if !output.status.success() {
        bail!("git ls-tree failed while listing suppression comparison sources");
    }
    let mut sources = BTreeSet::new();
    for record in output.stdout.split(|byte| *byte == b'\0').filter(|record| !record.is_empty()) {
        let record = std::str::from_utf8(record).context("suppression comparison tree entry is not UTF-8")?;
        let (metadata, path) = record.split_once('\t').context("suppression comparison tree entry has no path")?;
        if Path::new(path).extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let fields = metadata.split(' ').collect::<Vec<_>>();
        if fields.len() != 3 || fields[0] != "100644" && fields[0] != "100755" || fields[1] != "blob" {
            bail!("suppression comparison Rust source must be a regular Git blob: {path:?}");
        }
        validate_source_path(path)?;
        sources.insert(path.to_owned());
    }
    Ok(sources)
}

fn revision_manifest(workspace: &Path, revision: &str) -> Result<String> {
    let object = format!("{revision}:Cargo.toml");
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["show", "--no-ext-diff", &object])
        .output()
        .context("read package manifest from suppression comparison revision")?;
    if !output.status.success() {
        bail!("suppression comparison revision has no readable Cargo.toml");
    }
    String::from_utf8(output.stdout).context("suppression comparison Cargo.toml is not UTF-8")
}

fn manifest_target_sources(manifest: &str, known: &BTreeSet<String>) -> Result<BTreeMap<String, SourceCategory>> {
    let manifest: toml::Value = toml::from_str(manifest).context("parse suppression comparison Cargo.toml")?;
    let package = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .context("suppression comparison Cargo.toml must define a package")?;
    let package_name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .context("suppression comparison package must have a string name")?;
    let mut roots = BTreeMap::new();
    add_library_target(&manifest, package, known, &mut roots)?;
    add_automatic_targets(package, known, &mut roots)?;
    for kind in [
        DeclaredTargetKind {
            name: "bin",
            directory: "src/bin",
            category: SourceCategory::Production,
        },
        DeclaredTargetKind {
            name: "example",
            directory: "examples",
            category: SourceCategory::Production,
        },
        DeclaredTargetKind {
            name: "test",
            directory: "tests",
            category: SourceCategory::Test,
        },
        DeclaredTargetKind {
            name: "bench",
            directory: "benches",
            category: SourceCategory::Benchmark,
        },
    ] {
        add_declared_targets(&manifest, kind, package_name, known, &mut roots)?;
    }
    add_build_target(package, known, &mut roots)?;
    Ok(roots)
}

fn add_library_target(manifest: &toml::Value, package: &toml::map::Map<String, toml::Value>, known: &BTreeSet<String>, roots: &mut BTreeMap<String, SourceCategory>) -> Result<()> {
    if let Some(target) = manifest.get("lib") {
        let target = target.as_table().context("suppression comparison library target must be a table")?;
        let path = target.get("path").map_or(Ok("src/lib.rs"), |value| {
            value.as_str().context("suppression comparison library target path must be a string")
        })?;
        return insert_target(roots, known, path, SourceCategory::Production);
    }
    if package_auto_target(package, "autolib")? && known.contains("src/lib.rs") {
        insert_target(roots, known, "src/lib.rs", SourceCategory::Production)?;
    }
    Ok(())
}

fn add_automatic_targets(package: &toml::map::Map<String, toml::Value>, known: &BTreeSet<String>, roots: &mut BTreeMap<String, SourceCategory>) -> Result<()> {
    if package_auto_target(package, "autobins")? {
        if known.contains("src/main.rs") {
            insert_target(roots, known, "src/main.rs", SourceCategory::Production)?;
        }
        add_conventional_targets(roots, known, "src/bin", SourceCategory::Production)?;
    }
    for (setting, directory, category) in [
        ("autoexamples", "examples", SourceCategory::Production),
        ("autotests", "tests", SourceCategory::Test),
        ("autobenches", "benches", SourceCategory::Benchmark),
    ] {
        if package_auto_target(package, setting)? {
            add_conventional_targets(roots, known, directory, category)?;
        }
    }
    Ok(())
}

fn add_conventional_targets(roots: &mut BTreeMap<String, SourceCategory>, known: &BTreeSet<String>, directory: &str, category: SourceCategory) -> Result<()> {
    for path in known.iter().filter(|path| is_conventional_target(path, directory)) {
        insert_target(roots, known, path, category)?;
    }
    Ok(())
}

fn is_conventional_target(path: &str, directory: &str) -> bool {
    let Ok(relative) = Path::new(path).strip_prefix(directory) else {
        return false;
    };
    let components = relative.components().collect::<Vec<_>>();
    matches!(components.as_slice(), [Component::Normal(file)] if Path::new(file).extension().is_some_and(|extension| extension == "rs"))
        || matches!(components.as_slice(), [Component::Normal(_), Component::Normal(file)] if *file == "main.rs")
}

fn add_declared_targets(
    manifest: &toml::Value,
    kind: DeclaredTargetKind,
    package_name: &str,
    known: &BTreeSet<String>,
    roots: &mut BTreeMap<String, SourceCategory>,
) -> Result<()> {
    let Some(targets) = manifest.get(kind.name) else {
        return Ok(());
    };
    let targets = targets
        .as_array()
        .with_context(|| format!("suppression comparison {} targets must be an array", kind.name))?;
    for target in targets {
        let target = target.as_table().with_context(|| format!("suppression comparison {} target must be a table", kind.name))?;
        let path = declared_target_path(target, kind, package_name, known)?;
        insert_target(roots, known, &path, kind.category)?;
    }
    Ok(())
}

fn declared_target_path(target: &toml::map::Map<String, toml::Value>, kind: DeclaredTargetKind, package_name: &str, known: &BTreeSet<String>) -> Result<String> {
    if let Some(path) = target.get("path") {
        return path
            .as_str()
            .with_context(|| format!("suppression comparison {} target path must be a string", kind.name))
            .map(str::to_owned);
    }
    let name = target
        .get("name")
        .and_then(toml::Value::as_str)
        .with_context(|| format!("suppression comparison {} target without a path must have a string name", kind.name))?;
    let mut candidates = Vec::new();
    if kind.name == "bin" && name == package_name {
        candidates.push("src/main.rs".to_owned());
    }
    candidates.push(format!("{}/{name}.rs", kind.directory));
    candidates.push(format!("{}/{name}/main.rs", kind.directory));
    let matches = candidates.into_iter().filter(|path| known.contains(path)).collect::<Vec<_>>();
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!("suppression comparison {} target has no inferred source", kind.name),
        _ => bail!("suppression comparison {} target matches multiple inferred sources", kind.name),
    }
}

fn add_build_target(package: &toml::map::Map<String, toml::Value>, known: &BTreeSet<String>, roots: &mut BTreeMap<String, SourceCategory>) -> Result<()> {
    let path = match package.get("build") {
        Some(toml::Value::Boolean(false)) => return Ok(()),
        Some(toml::Value::Boolean(true)) | None => "build.rs",
        Some(toml::Value::String(path)) => path,
        Some(_) => bail!("suppression comparison package build target must be a boolean or string"),
    };
    if package.get("build").is_some() || known.contains(path) {
        insert_target(roots, known, path, SourceCategory::Production)?;
    }
    Ok(())
}

fn insert_target(roots: &mut BTreeMap<String, SourceCategory>, known: &BTreeSet<String>, path: &str, category: SourceCategory) -> Result<()> {
    validate_source_path(path)?;
    if !known.contains(path) {
        bail!("suppression comparison Cargo target source is missing: {path:?}");
    }
    if roots.insert(path.to_owned(), category).is_some_and(|existing| existing != category) {
        bail!("suppression comparison Cargo target source has conflicting categories: {path:?}");
    }
    Ok(())
}

fn package_auto_target(package: &toml::map::Map<String, toml::Value>, key: &str) -> Result<bool> {
    package.get(key).map_or(Ok(true), |value| {
        value.as_bool().with_context(|| format!("suppression comparison package {key} must be a boolean"))
    })
}

fn validate_source_path(path: &str) -> Result<()> {
    let path = Path::new(path);
    if path.is_absolute()
        || path.components().any(|component| !matches!(component, Component::Normal(_)))
        || path.extension().and_then(|extension| extension.to_str()) != Some("rs")
        || path.to_str().is_none_or(|path| path.contains('\\'))
    {
        bail!("suppression comparison Cargo target must be a normalized relative Rust path");
    }
    Ok(())
}

fn validate_revision(revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("suppression comparison revision must be a full Git commit hash");
    }
    Ok(())
}
