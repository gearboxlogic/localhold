use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};

use super::{SourceCategory, modules};
use crate::structure::suppression::source::SourceScanner;

pub(in crate::structure::suppression) fn reject_direct_source_suppressions(workspace: &Path, candidates: &BTreeSet<String>) -> Result<()> {
    let canonical_workspace = fs::canonicalize(workspace).context("resolve workspace for directly compiled Rust sources")?;
    let mut roots = BTreeMap::new();
    for candidate in candidates {
        validate_direct_source_path(candidate)?;
        let absolute = workspace.join(candidate);
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                bail!("directly compiled Rust source must exist during policy validation: {candidate:?}");
            }
            Err(error) => return Err(error).with_context(|| format!("inspect directly compiled Rust source {}", absolute.display())),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("directly compiled Rust source must be a regular non-symlink file: {candidate:?}");
        }
        let canonical = fs::canonicalize(&absolute).with_context(|| format!("resolve directly compiled Rust source {}", absolute.display()))?;
        let relative = canonical
            .strip_prefix(&canonical_workspace)
            .with_context(|| format!("directly compiled Rust source escapes the repository: {}", canonical.display()))?;
        if relative != Path::new(candidate) {
            bail!("directly compiled Rust source cannot traverse symlinked path components: {candidate:?}");
        }
        roots.insert(candidate.clone(), SourceCategory::Production);
    }
    let sources = modules::expand_target_sources(workspace, roots, |_| false)?;
    for (path, category) in sources {
        let absolute = workspace.join(&path);
        let source = fs::read_to_string(&absolute).with_context(|| format!("read directly compiled Rust source {}", absolute.display()))?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse directly compiled Rust source {path}"))?;
        if let Some(site) = SourceScanner::scan(&path, "direct-rustc", category, &syntax)?.first() {
            bail!(
                "directly compiled Rust must remain suppression-free; remove source suppression {} from {:?}",
                site.id,
                site.path
            );
        }
    }
    Ok(())
}

fn validate_direct_source_path(candidate: &str) -> Result<()> {
    let path = Path::new(candidate);
    if path.is_absolute()
        || path.components().any(|component| !matches!(component, Component::Normal(_)))
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("rs"))
        || candidate.contains('\\')
    {
        bail!("directly compiled Rust source must use a normalized repository-relative .rs path: {candidate:?}");
    }
    Ok(())
}
