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
use crate::structure::syntax::{ProductionCfgContext, production_cfg_context};

mod revision;
pub(super) use revision::expand_revision_target_sources;

type DiscoveredModule = (PathBuf, String, PathBuf, SourceCategory);

pub(super) fn expand_target_sources(workspace: &Path, roots: BTreeMap<String, SourceCategory>, is_structural: impl Fn(&str) -> bool) -> Result<BTreeMap<String, SourceCategory>> {
    expand_sources(
        roots,
        is_structural,
        |path| fs::read_to_string(workspace.join(path)).with_context(|| format!("read Cargo target module source {path}")),
        |base, name| resolve_external_module(workspace, base, name),
        |path| checked_module_path(workspace, path),
    )
}

fn expand_sources(
    roots: BTreeMap<String, SourceCategory>,
    is_structural: impl Fn(&str) -> bool,
    mut read_source: impl FnMut(&str) -> Result<String>,
    resolve_module: impl Fn(&Path, &str) -> Result<PathBuf>,
    check_path: impl Fn(&Path) -> Result<String>,
) -> Result<BTreeMap<String, SourceCategory>> {
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
        let source = read_source(&path)?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse Cargo target module source {path}"))?;
        let discovered = ModuleCollector::collect(module_base, category, &syntax)?;
        for (base, name, child_base, module_category) in discovered {
            let module = resolve_module(&base, &name)?;
            let module = check_path(&module)?;
            if !register_source(&mut sources, &module, module_category)? {
                continue;
            }
            if !is_structural(&module) {
                pending.push_back((module, child_base, module_category));
            }
        }
    }
    Ok(sources)
}

fn register_source(sources: &mut BTreeMap<String, SourceCategory>, module: &str, category: SourceCategory) -> Result<bool> {
    match sources.get(module).copied() {
        None => {
            sources.insert(module.to_owned(), category);
            Ok(true)
        }
        Some(existing) if existing == category || existing == SourceCategory::Production => Ok(false),
        Some(_) if category == SourceCategory::Production => {
            sources.insert(module.to_owned(), category);
            Ok(true)
        }
        Some(_) => bail!("Cargo target module source {module:?} has conflicting governance categories"),
    }
}

struct ModuleCollector {
    module_base: PathBuf,
    category: SourceCategory,
    production_context: Option<ProductionCfgContext>,
    discovered: Vec<DiscoveredModule>,
    error: Option<anyhow::Error>,
}

impl ModuleCollector {
    fn collect(module_base: PathBuf, category: SourceCategory, syntax: &syn::File) -> Result<Vec<DiscoveredModule>> {
        let production_context = (category == SourceCategory::Production).then(ProductionCfgContext::default);
        let mut collector = Self {
            module_base,
            category,
            production_context,
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

impl<'ast> Visit<'ast> for ModuleCollector {
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
        let production_context = match &self.production_context {
            Some(inherited) => match production_cfg_context(&module.attrs, inherited) {
                Ok(context) => context,
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            },
            None => None,
        };
        let category = if self.category == SourceCategory::Production && production_context.is_none() {
            SourceCategory::Test
        } else {
            self.category
        };
        let name = module.ident.unraw().to_string();
        let child_base = self.module_base.join(&name);
        if let Some((_, items)) = &module.content {
            let previous = std::mem::replace(&mut self.module_base, child_base);
            let previous_category = std::mem::replace(&mut self.category, category);
            let previous_context = std::mem::replace(&mut self.production_context, production_context);
            for item in items {
                self.visit_item(item);
            }
            self.production_context = previous_context;
            self.category = previous_category;
            self.module_base = previous;
            return;
        }
        self.discovered.push((self.module_base.clone(), name, child_base, category));
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
    select_external_module(module_base, name, |path| {
        workspace.join(path).try_exists().with_context(|| format!("inspect Cargo target module {}", path.display()))
    })
}

fn select_external_module(module_base: &Path, name: &str, exists: impl Fn(&Path) -> Result<bool>) -> Result<PathBuf> {
    let flat = module_base.join(format!("{name}.rs"));
    let nested = module_base.join(name).join("mod.rs");
    let flat_exists = exists(&flat)?;
    let nested_exists = exists(&nested)?;
    match (flat_exists, nested_exists) {
        (true, false) => Ok(flat),
        (false, true) => Ok(nested),
        (true, true) => bail!("Cargo target module {name:?} has both flat and nested source files"),
        (false, false) => bail!("Cargo target module {name:?} has no auditable source file"),
    }
}

fn checked_module_path(workspace: &Path, relative: &Path) -> Result<String> {
    let normalized = normalized_module_path(relative)?;
    let relative = Path::new(&normalized);
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
    Ok(normalized)
}

fn normalized_module_path(relative: &Path) -> Result<String> {
    if relative.is_absolute()
        || relative.components().any(|component| !matches!(component, Component::Normal(_)))
        || relative.extension().and_then(|extension| extension.to_str()) != Some("rs")
    {
        bail!("Cargo target module source must be a normalized relative Rust path");
    }
    Ok(relative.to_str().context("Cargo target module source path is not UTF-8")?.replace('\\', "/"))
}
