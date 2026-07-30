use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use syn::ext::IdentExt as _;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Meta, Token};

use super::SourceCategory;

type DiscoveredModule = (PathBuf, PathBuf);

pub(super) fn expand_target_sources(workspace: &Path, roots: BTreeMap<String, SourceCategory>, is_structural: impl Fn(&str) -> bool) -> Result<BTreeMap<String, SourceCategory>> {
    let mut sources = roots;
    let mut pending = sources
        .iter()
        .filter(|(path, _)| !is_structural(path))
        .map(|(path, &category)| {
            let base = Path::new(path).parent().unwrap_or_else(|| Path::new("")).to_path_buf();
            (path.clone(), base, category)
        })
        .collect::<VecDeque<_>>();
    while let Some((path, module_base, category)) = pending.pop_front() {
        let source = fs::read_to_string(workspace.join(&path)).with_context(|| format!("read Cargo target module source {path}"))?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse Cargo target module source {path}"))?;
        let discovered = ModuleCollector::collect(workspace, module_base, &syntax)?;
        for (module, child_base) in discovered {
            let module = checked_module_path(workspace, &module)?;
            if !register_source(&mut sources, &module, category)? {
                continue;
            }
            if !is_structural(&module) {
                pending.push_back((module, child_base, category));
            }
        }
    }
    Ok(sources)
}

fn register_source(sources: &mut BTreeMap<String, SourceCategory>, module: &str, category: SourceCategory) -> Result<bool> {
    if let Some(existing) = sources.get(module) {
        if *existing != category {
            bail!("Cargo target module source {module:?} has conflicting governance categories");
        }
        return Ok(false);
    }
    sources.insert(module.to_owned(), category);
    Ok(true)
}

struct ModuleCollector<'a> {
    workspace: &'a Path,
    module_base: PathBuf,
    discovered: Vec<DiscoveredModule>,
    error: Option<anyhow::Error>,
}

impl ModuleCollector<'_> {
    fn collect(workspace: &Path, module_base: PathBuf, syntax: &syn::File) -> Result<Vec<DiscoveredModule>> {
        let mut collector = ModuleCollector {
            workspace,
            module_base,
            discovered: Vec::new(),
            error: None,
        };
        collector.visit_file(syntax);
        if let Some(error) = collector.error {
            return Err(error);
        }
        Ok(collector.discovered)
    }
}

impl<'ast> Visit<'ast> for ModuleCollector<'_> {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if self.error.is_some() {
            return;
        }
        for attribute in &module.attrs {
            match contains_path_override(&attribute.meta) {
                Ok(true) => {
                    self.error = Some(anyhow::anyhow!(
                        "explicit module paths in non-structural Cargo targets are unsupported; add the target to structural governance"
                    ));
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        let name = module.ident.unraw().to_string();
        let child_base = self.module_base.join(&name);
        if let Some((_, items)) = &module.content {
            let previous = std::mem::replace(&mut self.module_base, child_base);
            for item in items {
                self.visit_item(item);
            }
            self.module_base = previous;
            return;
        }
        match resolve_external_module(self.workspace, &self.module_base, &name) {
            Ok(path) => self.discovered.push((path, child_base)),
            Err(error) => self.error = Some(error),
        }
    }

    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if !item.mac.path.is_ident("macro_rules") {
            self.error = Some(anyhow::anyhow!(
                "item macros in non-structural Cargo targets are unsupported because their module graph is opaque"
            ));
            return;
        }
        visit::visit_item_macro(self, item);
    }
}

fn contains_path_override(meta: &Meta) -> Result<bool> {
    if meta.path().is_ident("path") {
        return Ok(true);
    }
    if !meta.path().is_ident("cfg_attr") {
        return Ok(false);
    }
    let Meta::List(list) = meta else {
        return Ok(false);
    };
    let nested = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .context("parse cfg_attr while resolving a non-structural Cargo target module")?;
    for meta in nested.iter().skip(1) {
        if contains_path_override(meta)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn resolve_external_module(workspace: &Path, module_base: &Path, name: &str) -> Result<PathBuf> {
    let flat = module_base.join(format!("{name}.rs"));
    let nested = module_base.join(name).join("mod.rs");
    let flat_exists = workspace
        .join(&flat)
        .try_exists()
        .with_context(|| format!("inspect Cargo target module {}", flat.display()))?;
    let nested_exists = workspace
        .join(&nested)
        .try_exists()
        .with_context(|| format!("inspect Cargo target module {}", nested.display()))?;
    match (flat_exists, nested_exists) {
        (true, false) => Ok(flat),
        (false, true) => Ok(nested),
        (true, true) => bail!("Cargo target module {name:?} has both flat and nested source files"),
        (false, false) => bail!("Cargo target module {name:?} has no auditable source file"),
    }
}

fn checked_module_path(workspace: &Path, relative: &Path) -> Result<String> {
    if relative.is_absolute()
        || relative.components().any(|component| !matches!(component, Component::Normal(_)))
        || relative.extension().and_then(|extension| extension.to_str()) != Some("rs")
    {
        bail!("Cargo target module source must be a normalized relative Rust path");
    }
    let absolute = workspace.join(relative);
    let metadata = fs::symlink_metadata(&absolute).with_context(|| format!("inspect Cargo target module source {}", absolute.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("Cargo target module source must be a regular non-symlink file: {}", relative.display());
    }
    let workspace = fs::canonicalize(workspace).context("resolve workspace for Cargo target module inventory")?;
    let canonical = fs::canonicalize(&absolute).with_context(|| format!("resolve Cargo target module source {}", absolute.display()))?;
    let canonical_relative = canonical
        .strip_prefix(&workspace)
        .with_context(|| format!("Cargo target module source escapes the root package: {}", canonical.display()))?;
    if canonical_relative != relative {
        bail!("Cargo target module source cannot traverse symlinked path components");
    }
    Ok(relative.to_str().context("Cargo target module source path is not UTF-8")?.replace('\\', "/"))
}
