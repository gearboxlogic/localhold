use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::syntax::{TestLineCollector, item_is_test_only, normalized_ident, production_internal_imports, reject_module_path_overrides};

mod module_macro;
use module_macro::{audit_reviewed_macro_definitions, record_item_macro, safe_macro_definitions};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FileMeasurement {
    pub path: String,
    pub physical_lines: usize,
    pub production_lines: usize,
    pub test_lines: usize,
    pub production_internal_imports: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Inventory {
    pub files: Vec<FileMeasurement>,
}

struct ParsedSource {
    source: String,
    syntax: syn::File,
}

#[derive(Debug)]
struct ModuleEdge {
    source: String,
    target: String,
    test_only: bool,
}

struct TreeEntry {
    object_id: String,
    path: String,
}

struct SourceReachability {
    test_only: BTreeSet<String>,
    composition_only: BTreeSet<String>,
}

pub fn scan_workspace(workspace: &Path, roots: &[String]) -> Result<Inventory> {
    reject_untracked_rust_sources(workspace, "examples")?;
    let mut sources = BTreeMap::new();
    for root in roots {
        collect_sources(workspace, &workspace.join(root), &mut sources)?;
    }
    let target_roots = workspace_target_roots(workspace, sources.keys())?;
    measure_sources_with_roots(sources, &target_roots, true)
}

pub fn scan_revision(workspace: &Path, revision: &str, roots: &[String]) -> Result<Inventory> {
    validate_revision(revision)?;
    let output = Command::new("git")
        .current_dir(workspace)
        .arg("ls-tree")
        .arg("-r")
        .arg("-z")
        .arg(revision)
        .arg("--")
        .args(roots)
        .output()
        .context("list baseline Rust sources")?;
    if !output.status.success() {
        bail!("git ls-tree failed for structural baseline {revision}");
    }
    let entries = parse_tree_entries(&output.stdout)?;
    let sources = read_tree_sources(workspace, &entries)?;
    let manifest = read_revision_manifest(workspace, revision)?;
    let target_roots = target_roots(&manifest, sources.keys())?;
    measure_sources_with_roots(sources, &target_roots, false)
}

fn read_revision_manifest(workspace: &Path, revision: &str) -> Result<String> {
    let object = format!("{revision}:Cargo.toml");
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["show", "--no-ext-diff", &object])
        .output()
        .context("read package manifest from structural revision")?;
    if !output.status.success() {
        bail!("structural revision {revision} has no readable Cargo.toml");
    }
    String::from_utf8(output.stdout).context("package manifest from structural revision is not UTF-8")
}

fn parse_tree_entries(listing: &[u8]) -> Result<Vec<TreeEntry>> {
    let mut entries = Vec::new();
    for record in listing.split(|byte| *byte == b'\0').filter(|record| !record.is_empty()) {
        let record = std::str::from_utf8(record).context("baseline source listing is not UTF-8")?;
        let (metadata, path) = record.split_once('\t').context("baseline source listing record has no path separator")?;
        let fields = metadata.split(' ').collect::<Vec<_>>();
        if fields.len() != 3 || fields[1] != "blob" {
            bail!("baseline source listing contains invalid object metadata");
        }
        let object_id = fields[2];
        if !matches!(object_id.len(), 40 | 64) || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("baseline source listing contains an invalid object ID");
        }
        if Path::new(path).extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        validate_relative_rust_path(path)?;
        entries.push(TreeEntry {
            object_id: object_id.to_owned(),
            path: path.to_owned(),
        });
    }
    Ok(entries)
}

fn read_tree_sources(workspace: &Path, entries: &[TreeEntry]) -> Result<BTreeMap<String, String>> {
    let mut child = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start baseline source reader")?;
    {
        let mut input = child.stdin.take().context("baseline source reader has no input")?;
        for entry in entries {
            writeln!(input, "{}", entry.object_id).context("request baseline source object")?;
        }
    }
    let output = child.wait_with_output().context("wait for baseline source reader")?;
    if !output.status.success() {
        bail!("git cat-file failed while reading structural baseline sources");
    }
    parse_batch_sources(&output.stdout, entries)
}

fn parse_batch_sources(output: &[u8], entries: &[TreeEntry]) -> Result<BTreeMap<String, String>> {
    let mut cursor = Cursor::new(output);
    let mut sources = BTreeMap::new();
    for entry in entries {
        let mut header = String::new();
        cursor.read_line(&mut header).with_context(|| format!("read baseline source header for {}", entry.path))?;
        let fields = header.trim_end_matches('\n').split(' ').collect::<Vec<_>>();
        if fields.len() != 3 || fields[0] != entry.object_id || fields[1] != "blob" {
            bail!("unexpected baseline source header for {}", entry.path);
        }
        let size = fields[2].parse::<usize>().with_context(|| format!("parse baseline source size for {}", entry.path))?;
        let mut bytes = vec![0; size];
        cursor.read_exact(&mut bytes).with_context(|| format!("read baseline source {}", entry.path))?;
        let mut terminator = [0];
        cursor
            .read_exact(&mut terminator)
            .with_context(|| format!("read baseline source terminator for {}", entry.path))?;
        if terminator != *b"\n" {
            bail!("baseline source {} has an invalid batch terminator", entry.path);
        }
        let source = String::from_utf8(bytes).with_context(|| format!("baseline Rust source {} is not UTF-8", entry.path))?;
        if sources.insert(entry.path.clone(), source).is_some() {
            bail!("baseline source listing repeats {}", entry.path);
        }
    }
    if cursor.position() != output.len() as u64 {
        bail!("baseline source reader returned unexpected trailing data");
    }
    Ok(sources)
}

fn collect_sources(workspace: &Path, root: &Path, sources: &mut BTreeMap<String, String>) -> Result<()> {
    let metadata = fs::symlink_metadata(root).with_context(|| format!("inspect structural source root {}", root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("structural source root must be a real directory: {}", root.display());
    }
    let mut entries = fs::read_dir(root)
        .with_context(|| format!("read structural source directory {}", root.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().with_context(|| format!("inspect structural source entry {}", path.display()))?;
        if file_type.is_symlink() {
            bail!("structural source roots cannot contain symlinks: {}", path.display());
        }
        if file_type.is_dir() {
            collect_sources(workspace, &path, sources)?;
        } else if file_type.is_file() && path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            let relative = normalized_relative_path(workspace, &path)?;
            let source = fs::read_to_string(&path).with_context(|| format!("read structural Rust source {}", path.display()))?;
            sources.insert(relative, source);
        }
    }
    Ok(())
}

#[cfg(test)]
fn measure_sources(sources: BTreeMap<String, String>) -> Result<Inventory> {
    let production_roots = conventional_production_roots(sources.keys(), true, true);
    let composition_roots = conventional_composition_roots(production_roots.iter(), true);
    let library_roots = conventional_library_roots(production_roots.iter(), true);
    let target_roots = TargetRoots {
        production: production_roots,
        test: BTreeSet::new(),
        composition: composition_roots,
        libraries: library_roots,
        rust_2015_absolute_paths: false,
    };
    measure_sources_with_roots(sources, &target_roots, true)
}

fn measure_sources_with_roots(sources: BTreeMap<String, String>, target_roots: &TargetRoots, require_reviewed_expansions: bool) -> Result<Inventory> {
    let TargetRoots {
        production: production_roots,
        test: explicit_test_roots,
        composition: composition_roots,
        libraries: library_roots,
        rust_2015_absolute_paths,
    } = target_roots;
    let parsed = parse_sources(sources)?;
    let reachability = classify_source_reachability(&parsed, production_roots, explicit_test_roots, composition_roots, library_roots)?;
    let mut files = Vec::with_capacity(parsed.len());
    for (path, parsed) in parsed {
        let physical_lines = physical_line_count(&parsed.source);
        let file_is_test_only = reachability.test_only.contains(&path);
        let mut collector = TestLineCollector::new(physical_lines);
        let (test_lines, production_internal_imports) = if file_is_test_only {
            (physical_lines, Vec::new())
        } else {
            collector.visit_file(&parsed.syntax)?;
            (
                collector.test_line_count(),
                if !path.starts_with("src/") || reachability.composition_only.contains(&path) || path.starts_with("src/server/") || path.starts_with("src/ui/") {
                    Vec::new()
                } else {
                    production_internal_imports(
                        &parsed.syntax,
                        &path,
                        library_root_for_source(&path, library_roots),
                        *rust_2015_absolute_paths,
                        require_reviewed_expansions,
                    )
                    .map_err(|error| anyhow::anyhow!("{error} in {path}"))?
                },
            )
        };
        files.push(FileMeasurement {
            path,
            physical_lines,
            production_lines: physical_lines.saturating_sub(test_lines),
            test_lines,
            production_internal_imports,
        });
    }
    Ok(Inventory { files })
}

fn library_root_for_source<'a>(source: &str, library_roots: &'a BTreeSet<String>) -> Option<&'a str> {
    library_roots
        .iter()
        .find(|root| Path::new(root.as_str()).parent().is_some_and(|parent| Path::new(source).starts_with(parent)))
        .map(String::as_str)
}

fn parse_sources(sources: BTreeMap<String, String>) -> Result<BTreeMap<String, ParsedSource>> {
    sources
        .into_iter()
        .map(|(path, source)| {
            let syntax = syn::parse_file(&source).with_context(|| format!("parse structural Rust source {path}"))?;
            Ok((path, ParsedSource { source, syntax }))
        })
        .collect()
}

fn classify_source_reachability(
    parsed: &BTreeMap<String, ParsedSource>,
    production_roots: &BTreeSet<String>,
    explicit_test_roots: &BTreeSet<String>,
    composition_roots: &BTreeSet<String>,
    library_roots: &BTreeSet<String>,
) -> Result<SourceReachability> {
    let known: BTreeSet<_> = parsed.keys().cloned().collect();
    let mut graph = ModuleGraph {
        known: &known,
        edges: Vec::new(),
        opaque_macro_sources: BTreeSet::new(),
    };
    for (path, source) in parsed {
        audit_reviewed_macro_definitions(&source.syntax).with_context(|| format!("audit reviewed macro definitions in {path}"))?;
        if production_roots.contains(path) || explicit_test_roots.contains(path) {
            let crate_root_dir = Path::new(path).parent().context("Cargo target root has no parent directory")?;
            graph.collect(path, &source.syntax.items, crate_root_dir, false)?;
        } else {
            graph.collect(path, &source.syntax.items, &module_directory(path)?, false)?;
        }
    }
    let opaque_macro_sources = graph.opaque_macro_sources;
    let edges = graph.edges;
    let library_reachable = production_reachable_from(&edges, library_roots);
    if let Some(source) = opaque_macro_sources.intersection(&library_reachable).next() {
        bail!("opaque production item macros cannot safely define module edges in library source {source}");
    }
    if let Some(overlap) = explicit_test_roots.intersection(&library_reachable).next() {
        bail!("test or benchmark target must not also be reachable from a library target: {overlap}");
    }
    if let Some(overlap) = composition_roots.intersection(&library_reachable).next() {
        bail!("composition target must not also be reachable from a library target: {overlap}");
    }
    let composition_reachable = production_reachable_from(&edges, composition_roots);
    let incoming: BTreeSet<_> = edges.iter().map(|edge| edge.target.as_str()).collect();
    let mut production_reachable = production_roots.clone();
    let mut test_reachable: BTreeSet<_> = known.iter().filter(|path| path.starts_with("tests/") || path.starts_with("benches/")).cloned().collect();
    test_reachable.extend(explicit_test_roots.iter().cloned());
    for path in known
        .iter()
        .filter(|path| path.starts_with("src/") && !incoming.contains(path.as_str()) && !explicit_test_roots.contains(*path))
    {
        production_reachable.insert(path.clone());
    }

    loop {
        propagate_reachability(&edges, &mut production_reachable, &mut test_reachable);
        let unknown = known
            .iter()
            .filter(|path| !production_reachable.contains(*path) && !test_reachable.contains(*path))
            .cloned()
            .collect::<Vec<_>>();
        if unknown.is_empty() {
            break;
        }
        for path in unknown {
            production_reachable.insert(path);
        }
    }
    Ok(SourceReachability {
        test_only: test_reachable.difference(&production_reachable).cloned().collect(),
        composition_only: composition_reachable.difference(&library_reachable).cloned().collect(),
    })
}

fn production_reachable_from(edges: &[ModuleEdge], roots: &BTreeSet<String>) -> BTreeSet<String> {
    let mut reachable = roots.clone();
    loop {
        let mut changed = false;
        for edge in edges {
            if !edge.test_only && reachable.contains(&edge.source) {
                changed |= reachable.insert(edge.target.clone());
            }
        }
        if !changed {
            return reachable;
        }
    }
}

fn propagate_reachability(edges: &[ModuleEdge], production: &mut BTreeSet<String>, test: &mut BTreeSet<String>) {
    loop {
        let mut changed = false;
        for edge in edges {
            changed |= match (production.contains(&edge.source), edge.test_only) {
                (true, true) => test.insert(edge.target.clone()),
                (true, false) => production.insert(edge.target.clone()),
                (false, _) => false,
            };
            let source_is_test = test.contains(&edge.source);
            if source_is_test {
                changed |= test.insert(edge.target.clone());
            }
        }
        if !changed {
            break;
        }
    }
}

struct TargetRoots {
    production: BTreeSet<String>,
    test: BTreeSet<String>,
    composition: BTreeSet<String>,
    libraries: BTreeSet<String>,
    rust_2015_absolute_paths: bool,
}

fn workspace_target_roots<'a>(workspace: &Path, known: impl Iterator<Item = &'a String>) -> Result<TargetRoots> {
    let manifest_path = workspace.join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path).with_context(|| format!("read package manifest {}", manifest_path.display()))?;
    target_roots(&manifest, known)
}

fn target_roots<'a>(manifest: &str, known: impl Iterator<Item = &'a String>) -> Result<TargetRoots> {
    let known = known.cloned().collect::<BTreeSet<_>>();
    let manifest: toml::Value = toml::from_str(manifest).context("parse package manifest")?;
    validate_testing_feature_is_isolated(&manifest)?;
    let auto_library = package_auto_target(&manifest, "autolib")?;
    let auto_binaries = package_auto_target(&manifest, "autobins")?;
    let rust_2015_absolute_paths = package_uses_rust_2015_paths(&manifest)?;
    let mut roots = conventional_production_roots(known.iter(), auto_library, auto_binaries);
    let mut composition = conventional_composition_roots(roots.iter(), auto_binaries);
    let mut libraries = conventional_library_roots(roots.iter(), auto_library);
    if let Some(library) = manifest.get("lib") {
        if !auto_library && library.get("path").is_none() {
            bail!("declared library target must set path when package autolib is false");
        }
        if let Some(path) = add_declared_target_root(library, "library", &known, &mut roots)? {
            if path.starts_with("src/server/") || path.starts_with("src/ui/") {
                bail!("package library target cannot use an exempt server/UI directory as its crate root: {path}");
            }
            roots.retain(|candidate| !libraries.contains(candidate) || candidate == &path);
            libraries.clear();
            libraries.insert(path);
        }
    }
    if let Some(binaries) = manifest.get("bin") {
        let binaries = binaries.as_array().context("package bin targets must be an array of tables")?;
        for binary in binaries {
            if !auto_binaries && binary.get("path").is_none() {
                bail!("declared binary targets must set path when package autobins is false");
            }
            if let Some(path) = add_declared_target_root(binary, "binary", &known, &mut roots)? {
                composition.insert(path);
            }
        }
    }
    let examples = validate_auxiliary_target_roots(&manifest, "example", &known)?;
    roots.extend(examples.iter().cloned());
    composition.extend(examples);
    let mut test = BTreeSet::new();
    for kind in ["test", "bench"] {
        test.extend(validate_auxiliary_target_roots(&manifest, kind, &known)?);
    }
    Ok(TargetRoots {
        production: roots,
        test,
        composition,
        libraries,
        rust_2015_absolute_paths,
    })
}

fn package_auto_target(manifest: &toml::Value, key: &str) -> Result<bool> {
    let package = manifest.get("package").context("package manifest must define [package]")?;
    let package = package.as_table().context("package manifest [package] must be a table")?;
    package
        .get(key)
        .map_or(Ok(true), |value| value.as_bool().with_context(|| format!("package {key} must be a boolean")))
}

fn package_uses_rust_2015_paths(manifest: &toml::Value) -> Result<bool> {
    let package = manifest.get("package").context("package manifest must define [package]")?;
    let package = package.as_table().context("package manifest [package] must be a table")?;
    let Some(edition) = package.get("edition") else {
        return Ok(true);
    };
    let edition = edition.as_str().context("package edition must be a string")?;
    match edition {
        "2015" => Ok(true),
        "2018" | "2021" | "2024" => Ok(false),
        _ => bail!("unsupported package edition {edition:?} for production import classification"),
    }
}

fn validate_testing_feature_is_isolated(manifest: &toml::Value) -> Result<()> {
    let Some(features) = manifest.get("features") else {
        return Ok(());
    };
    let features = features.as_table().context("package features must be a table")?;
    for (feature, members) in features {
        let members = members.as_array().with_context(|| format!("package feature {feature:?} must be an array"))?;
        for member in members {
            let member = member.as_str().with_context(|| format!("package feature {feature:?} member must be a string"))?;
            if feature != "testing" && member == "testing" {
                bail!("package feature {feature:?} must not enable the test-only \"testing\" feature");
            }
        }
    }
    Ok(())
}

fn conventional_production_roots<'a>(known: impl Iterator<Item = &'a String>, auto_library: bool, auto_binaries: bool) -> BTreeSet<String> {
    known
        .filter(|path| auto_library && path.as_str() == "src/lib.rs" || auto_binaries && is_conventional_binary_root(path))
        .cloned()
        .collect()
}

fn conventional_composition_roots<'a>(known: impl Iterator<Item = &'a String>, auto_binaries: bool) -> BTreeSet<String> {
    known.filter(|path| auto_binaries && is_conventional_binary_root(path)).cloned().collect()
}

fn conventional_library_roots<'a>(known: impl Iterator<Item = &'a String>, auto_library: bool) -> BTreeSet<String> {
    known.filter(|path| auto_library && path.as_str() == "src/lib.rs").cloned().collect()
}

fn is_conventional_binary_root(path: &str) -> bool {
    if path == "src/main.rs" {
        return true;
    }
    let Some(relative) = path.strip_prefix("src/bin/") else {
        return false;
    };
    let mut components = relative.split('/');
    match (components.next(), components.next(), components.next()) {
        (Some(file), None, None) => Path::new(file).extension().is_some_and(|extension| extension == "rs"),
        (Some(_), Some("main.rs"), None) => true,
        _ => false,
    }
}

fn add_declared_target_root(target: &toml::Value, kind: &str, known: &BTreeSet<String>, roots: &mut BTreeSet<String>) -> Result<Option<String>> {
    let target = target.as_table().with_context(|| format!("package {kind} target must be a table"))?;
    let Some(path) = target.get("path") else {
        return Ok(None);
    };
    let path = path.as_str().with_context(|| format!("package {kind} target path must be a string"))?;
    validate_relative_rust_path(path)?;
    if !path.starts_with("src/") {
        bail!("package {kind} production target must remain under src/: {path}");
    }
    if !known.contains(path) {
        bail!("package {kind} production target is missing from the structural source inventory: {path}");
    }
    roots.insert(path.to_owned());
    Ok(Some(path.to_owned()))
}

fn validate_auxiliary_target_roots(manifest: &toml::Value, kind: &str, known: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    let Some(targets) = manifest.get(kind) else {
        return Ok(BTreeSet::new());
    };
    let targets = targets.as_array().with_context(|| format!("package {kind} targets must be an array of tables"))?;
    let mut roots = BTreeSet::new();
    for target in targets {
        let target = target.as_table().with_context(|| format!("package {kind} target must be a table"))?;
        let Some(path) = target.get("path") else {
            continue;
        };
        let path = path.as_str().with_context(|| format!("package {kind} target path must be a string"))?;
        validate_relative_rust_path(path)?;
        if !known.contains(path) {
            bail!("package {kind} target is outside the structural source inventory: {path}");
        }
        roots.insert(path.to_owned());
    }
    Ok(roots)
}

fn reject_untracked_rust_sources(workspace: &Path, root_name: &str) -> Result<()> {
    let root = workspace.join(root_name);
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("untracked source root must be absent or a real directory: {}", root.display());
        }
        Ok(_) => reject_rust_sources_in_directory(&root, root_name),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect untracked source root {}", root.display())),
    }
}

fn reject_rust_sources_in_directory(directory: &Path, root_name: &str) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("read untracked source directory {}", directory.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().with_context(|| format!("inspect untracked source entry {}", path.display()))?;
        if file_type.is_symlink() {
            bail!("untracked source roots cannot contain symlinks: {}", path.display());
        }
        if file_type.is_dir() {
            reject_rust_sources_in_directory(&path, root_name)?;
        } else if file_type.is_file() && path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            bail!("Rust sources under untracked {root_name}/ are forbidden during the structural freeze: {}", path.display());
        }
    }
    Ok(())
}

struct ModuleGraph<'a> {
    known: &'a BTreeSet<String>,
    edges: Vec<ModuleEdge>,
    opaque_macro_sources: BTreeSet<String>,
}

struct ModuleScope<'a> {
    source_path: &'a str,
    module_dir: &'a Path,
    parent_test_only: bool,
    inherited_macros: &'a BTreeSet<String>,
}

impl ModuleGraph<'_> {
    fn collect(&mut self, source_path: &str, items: &[syn::Item], module_dir: &Path, parent_test_only: bool) -> Result<()> {
        self.collect_with_macros(
            items,
            &ModuleScope {
                source_path,
                module_dir,
                parent_test_only,
                inherited_macros: &BTreeSet::new(),
            },
        )
    }

    fn collect_with_macros(&mut self, items: &[syn::Item], scope: &ModuleScope<'_>) -> Result<()> {
        let safe_macros = safe_macro_definitions(items, scope.parent_test_only, scope.inherited_macros)?;
        for item in items {
            if record_item_macro(item, scope.parent_test_only, scope.source_path, &safe_macros, &mut self.opaque_macro_sources)? {
                continue;
            }
            let syn::Item::Mod(module) = item else {
                continue;
            };
            reject_module_path_overrides(&module.attrs)?;
            let test_only = scope.parent_test_only || item_is_test_only(item)?;
            let module_name = normalized_ident(&module.ident);
            if let Some((_, nested)) = &module.content {
                let nested_dir = scope.module_dir.join(&module_name);
                self.collect_with_macros(
                    nested,
                    &ModuleScope {
                        source_path: scope.source_path,
                        module_dir: &nested_dir,
                        parent_test_only: test_only,
                        inherited_macros: &safe_macros,
                    },
                )?;
                continue;
            }
            let target =
                resolve_module(scope.module_dir, &module_name, self.known).with_context(|| format!("resolve module {} declared by {}", module.ident, scope.source_path))?;
            if let Some(target) = target {
                self.edges.push(ModuleEdge {
                    source: scope.source_path.to_owned(),
                    target,
                    test_only,
                });
            }
        }
        Ok(())
    }
}

fn resolve_module(module_dir: &Path, name: &str, known: &BTreeSet<String>) -> Result<Option<String>> {
    let candidates = [module_dir.join(format!("{name}.rs")), module_dir.join(name).join("mod.rs")];
    let matches = candidates
        .iter()
        .map(|path| normalized_path(path))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| known.contains(path))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [path] => Ok(Some(path.clone())),
        _ => bail!("module {name:?} has both file and directory module sources"),
    }
}

fn module_directory(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    let parent = path.parent().context("Rust source path has no parent")?;
    let stem = path.file_stem().and_then(|stem| stem.to_str()).context("Rust source path has no UTF-8 stem")?;
    Ok(if matches!(stem, "lib" | "main" | "mod") {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    })
}

fn physical_line_count(source: &str) -> usize {
    source.bytes().filter(|byte| *byte == b'\n').count() + usize::from(!source.is_empty() && !source.ends_with('\n'))
}

fn normalized_relative_path(workspace: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(workspace).context("structural source escaped workspace")?;
    normalized_path(relative)
}

fn normalized_path(path: &Path) -> Result<String> {
    if path.is_absolute() || path.components().any(|component| !matches!(component, Component::Normal(_))) {
        bail!("structural Rust path must be normalized and relative: {}", path.display());
    }
    let value = path.to_str().context("structural Rust path is not UTF-8")?.replace('\\', "/");
    validate_relative_rust_path(&value)?;
    Ok(value)
}

fn validate_relative_rust_path(path: &str) -> Result<()> {
    let path = Path::new(path);
    if path.is_absolute()
        || path.extension().and_then(|extension| extension.to_str()) != Some("rs")
        || path.components().any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("structural source path must be a normalized relative Rust path: {}", path.display());
    }
    Ok(())
}

fn validate_revision(revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("structural baseline revision must be a full Git commit hash");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
