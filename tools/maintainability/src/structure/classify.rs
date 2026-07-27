use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::syntax::{TestLineCollector, item_is_test_only, reject_module_path_overrides};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FileMeasurement {
    pub path: String,
    pub physical_lines: usize,
    pub production_lines: usize,
    pub test_lines: usize,
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

pub fn scan_workspace(workspace: &Path, roots: &[String]) -> Result<Inventory> {
    reject_untracked_rust_sources(workspace, "examples")?;
    let mut sources = BTreeMap::new();
    for root in roots {
        collect_sources(workspace, &workspace.join(root), &mut sources)?;
    }
    let target_roots = workspace_target_roots(workspace, sources.keys())?;
    measure_sources_with_roots(sources, &target_roots.production, &target_roots.test)
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
    measure_sources_with_roots(sources, &target_roots.production, &target_roots.test)
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
    let production_roots = conventional_production_roots(sources.keys());
    measure_sources_with_roots(sources, &production_roots, &BTreeSet::new())
}

fn measure_sources_with_roots(sources: BTreeMap<String, String>, production_roots: &BTreeSet<String>, explicit_test_roots: &BTreeSet<String>) -> Result<Inventory> {
    let parsed = parse_sources(sources)?;
    let test_only_files = discover_test_only_files(&parsed, production_roots, explicit_test_roots)?;
    let mut files = Vec::with_capacity(parsed.len());
    for (path, parsed) in parsed {
        let physical_lines = physical_line_count(&parsed.source);
        let test_lines = if test_only_files.contains(&path) {
            physical_lines
        } else {
            let mut collector = TestLineCollector::new(physical_lines);
            collector.visit_file(&parsed.syntax)?;
            collector.test_line_count()
        };
        files.push(FileMeasurement {
            path,
            physical_lines,
            production_lines: physical_lines.saturating_sub(test_lines),
            test_lines,
        });
    }
    Ok(Inventory { files })
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

fn discover_test_only_files(parsed: &BTreeMap<String, ParsedSource>, production_roots: &BTreeSet<String>, explicit_test_roots: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    let known: BTreeSet<_> = parsed.keys().cloned().collect();
    let mut graph = ModuleGraph { known: &known, edges: Vec::new() };
    for (path, source) in parsed {
        let module_dir = module_directory(path)?;
        graph.collect(path, &source.syntax.items, &module_dir, false)?;
        if production_roots.contains(path) || explicit_test_roots.contains(path) {
            let crate_root_dir = Path::new(path).parent().context("Cargo target root has no parent directory")?;
            if crate_root_dir != module_dir {
                graph.collect(path, &source.syntax.items, crate_root_dir, false)?;
            }
        }
    }
    let edges = graph.edges;
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
    Ok(test_reachable.difference(&production_reachable).cloned().collect())
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
    let mut roots = conventional_production_roots(known.iter());
    if let Some(library) = manifest.get("lib") {
        add_declared_target_root(library, "library", &known, &mut roots)?;
    }
    if let Some(binaries) = manifest.get("bin") {
        let binaries = binaries.as_array().context("package bin targets must be an array of tables")?;
        for binary in binaries {
            add_declared_target_root(binary, "binary", &known, &mut roots)?;
        }
    }
    roots.extend(validate_auxiliary_target_roots(&manifest, "example", &known)?);
    let mut test = BTreeSet::new();
    for kind in ["test", "bench"] {
        test.extend(validate_auxiliary_target_roots(&manifest, kind, &known)?);
    }
    Ok(TargetRoots { production: roots, test })
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

fn conventional_production_roots<'a>(known: impl Iterator<Item = &'a String>) -> BTreeSet<String> {
    known
        .filter(|path| matches!(path.as_str(), "src/lib.rs" | "src/main.rs") || path.starts_with("src/bin/"))
        .cloned()
        .collect()
}

fn add_declared_target_root(target: &toml::Value, kind: &str, known: &BTreeSet<String>, roots: &mut BTreeSet<String>) -> Result<()> {
    let target = target.as_table().with_context(|| format!("package {kind} target must be a table"))?;
    let Some(path) = target.get("path") else {
        return Ok(());
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
    Ok(())
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
}

impl ModuleGraph<'_> {
    fn collect(&mut self, source_path: &str, items: &[syn::Item], module_dir: &Path, parent_test_only: bool) -> Result<()> {
        for item in items {
            let syn::Item::Mod(module) = item else {
                continue;
            };
            reject_module_path_overrides(&module.attrs)?;
            let test_only = parent_test_only || item_is_test_only(item)?;
            if let Some((_, nested)) = &module.content {
                self.collect(source_path, nested, &module_dir.join(module.ident.to_string()), test_only)?;
                continue;
            }
            let target = resolve_module(module_dir, &module.ident.to_string(), self.known).with_context(|| format!("resolve module {} declared by {source_path}", module.ident))?;
            if let Some(target) = target {
                self.edges.push(ModuleEdge {
                    source: source_path.to_owned(),
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
