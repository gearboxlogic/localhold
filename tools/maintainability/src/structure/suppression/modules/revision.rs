use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{expand_sources, normalized_module_path, select_external_module};
use crate::structure::suppression::SourceCategory;

pub(in crate::structure::suppression) fn expand_revision_target_sources(
    workspace: &Path,
    revision: &str,
    roots: BTreeMap<String, SourceCategory>,
    rust_sources: &BTreeSet<String>,
    is_structural: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, SourceCategory>> {
    expand_sources(
        roots,
        is_structural,
        |path| read_revision_source(workspace, revision, path),
        |base, name| resolve_revision_module(base, name, rust_sources),
        |path| checked_revision_module_path(path, rust_sources),
    )
}

fn read_revision_source(workspace: &Path, revision: &str, path: &str) -> Result<String> {
    let object = format!("{revision}:{path}");
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["show", "--no-ext-diff", &object])
        .output()
        .with_context(|| format!("read Cargo target module source {path:?} from revision"))?;
    if !output.status.success() {
        bail!("cannot read Cargo target module source {path:?} from revision {revision}");
    }
    String::from_utf8(output.stdout).with_context(|| format!("Cargo target module source {path:?} from revision is not UTF-8"))
}

fn resolve_revision_module(module_base: &Path, name: &str, rust_sources: &BTreeSet<String>) -> Result<PathBuf> {
    select_external_module(module_base, name, |path| {
        let path = normalized_module_path(path)?;
        Ok(rust_sources.contains(&path))
    })
}

fn checked_revision_module_path(relative: &Path, rust_sources: &BTreeSet<String>) -> Result<String> {
    let relative = normalized_module_path(relative)?;
    if !rust_sources.contains(&relative) {
        bail!("Cargo target module source is absent or not a regular Rust file in the comparison revision: {relative:?}");
    }
    Ok(relative)
}
