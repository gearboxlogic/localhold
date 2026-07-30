use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use syn::ext::IdentExt as _;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Meta, Token};

use super::SourceCategory;
use super::targets::TargetRoots;
use crate::structure::syntax::{ProductionCfgContext, cfg_attributes_can_be_enabled, production_cfg_context};

mod identities;
mod revision;
pub(super) use revision::expand_revision_target_sources;

pub(super) struct ExpandedSources {
    pub(super) categories: BTreeMap<String, SourceCategory>,
    pub(super) relations: BTreeMap<(String, String), String>,
    pub(super) target_identities: BTreeMap<String, BTreeSet<String>>,
}

impl ExpandedSources {
    pub(super) fn target_component(&self, path: &str) -> Result<String> {
        let identities = self
            .target_identities
            .get(path)
            .with_context(|| format!("Cargo target source {path:?} has no target identity"))?;
        Ok(identities::component(identities))
    }
}

struct DiscoveredModule {
    base: PathBuf,
    name: String,
    explicit_source: Option<PathBuf>,
    child_base: PathBuf,
    category: SourceCategory,
    item: String,
}

pub(super) fn expand_target_sources(workspace: &Path, roots: TargetRoots, is_structural: impl Fn(&str) -> bool) -> Result<ExpandedSources> {
    expand_sources(
        roots,
        is_structural,
        |path| fs::read_to_string(workspace.join(path)).with_context(|| format!("read Cargo target module source {path}")),
        |base, name| resolve_external_module(workspace, base, name),
        |path| checked_module_path(workspace, path),
    )
}

fn expand_sources(
    roots: TargetRoots,
    is_structural: impl Fn(&str) -> bool,
    mut read_source: impl FnMut(&str) -> Result<String>,
    resolve_module: impl Fn(&Path, &str) -> Result<PathBuf>,
    check_path: impl Fn(&Path) -> Result<String>,
) -> Result<ExpandedSources> {
    let TargetRoots {
        categories: mut sources,
        identities: root_identities,
    } = roots;
    let mut relations = BTreeMap::new();
    let mut pending = sources
        .iter()
        .map(|(path, &category)| {
            let base = Path::new(path).parent().unwrap_or_else(|| Path::new("")).to_path_buf();
            (path.clone(), base, category)
        })
        .collect::<VecDeque<_>>();
    while let Some((path, module_base, category)) = pending.pop_front() {
        let source = read_source(&path)?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse Cargo target module source {path}"))?;
        let explicit_path_base = Path::new(&path).parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let discovered = ModuleCollector::collect(module_base, explicit_path_base, category, is_structural(&path), &syntax)?;
        for discovered in discovered {
            let module = match discovered.explicit_source {
                Some(module) => module,
                None => resolve_module(&discovered.base, &discovered.name)?,
            };
            let module = check_path(&module)?;
            let relation_key = (path.clone(), discovered.item);
            if relations.insert(relation_key.clone(), module.clone()).is_some_and(|existing| existing != module) {
                bail!("Cargo target module relation {relation_key:?} resolves ambiguously");
            }
            if !register_source(&mut sources, &module, discovered.category)? {
                continue;
            }
            pending.push_back((module, discovered.child_base, discovered.category));
        }
    }
    let target_identities = identities::propagate(&root_identities, &relations);
    Ok(ExpandedSources {
        categories: sources,
        relations,
        target_identities,
    })
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
    explicit_path_base: PathBuf,
    category: SourceCategory,
    structural: bool,
    production_context: Option<ProductionCfgContext>,
    item_path: Vec<String>,
    discovered: Vec<DiscoveredModule>,
    error: Option<anyhow::Error>,
}

impl ModuleCollector {
    fn collect(module_base: PathBuf, explicit_path_base: PathBuf, category: SourceCategory, structural: bool, syntax: &syn::File) -> Result<Vec<DiscoveredModule>> {
        let production_context = (category == SourceCategory::Production).then(ProductionCfgContext::default);
        let mut collector = Self {
            module_base,
            explicit_path_base,
            category,
            structural,
            production_context,
            item_path: Vec::new(),
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
        match cfg_attributes_can_be_enabled(&module.attrs) {
            Ok(false) => return,
            Ok(true) => {}
            Err(error) => {
                self.error = Some(error);
                return;
            }
        }
        let explicit_path = match direct_module_path(&module.attrs) {
            Ok(Some(_)) if !self.structural => {
                self.error = Some(anyhow::anyhow!(
                    "explicit module paths in non-structural Cargo targets are unsupported; add the target to structural governance"
                ));
                return;
            }
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
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
        let mut item_path = self.item_path.clone();
        item_path.push(name.clone());
        let item = item_path.join("::");
        let explicit_source = explicit_path.map(|path| self.explicit_path_base.join(path));
        let child_base = explicit_source.as_deref().map_or_else(|| self.module_base.join(&name), child_module_base);
        if let Some((_, items)) = &module.content {
            let previous_base = std::mem::replace(&mut self.module_base, child_base.clone());
            let previous_explicit_base = std::mem::replace(&mut self.explicit_path_base, child_base);
            let previous_category = std::mem::replace(&mut self.category, category);
            let previous_context = std::mem::replace(&mut self.production_context, production_context);
            let previous_item_path = std::mem::replace(&mut self.item_path, item_path);
            for item in items {
                self.visit_item(item);
            }
            self.item_path = previous_item_path;
            self.production_context = previous_context;
            self.category = previous_category;
            self.explicit_path_base = previous_explicit_base;
            self.module_base = previous_base;
            return;
        }
        self.discovered.push(DiscoveredModule {
            base: self.module_base.clone(),
            name,
            explicit_source,
            child_base,
            category,
            item,
        });
    }

    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if self.structural {
            return;
        }
        if !item.mac.path.is_ident("macro_rules") {
            self.error = Some(anyhow::anyhow!(
                "item macros in non-structural Cargo targets are unsupported because their module graph is opaque"
            ));
            return;
        }
        visit::visit_item_macro(self, item);
    }
}

pub(super) fn audited_module_paths(meta: &Meta) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_audited_module_paths(meta, &mut paths)?;
    Ok(paths)
}

fn collect_audited_module_paths(meta: &Meta, paths: &mut Vec<PathBuf>) -> Result<()> {
    if meta_has_normalized_ident(meta, "path") {
        let Meta::NameValue(value) = meta else {
            bail!("explicit Rust module path must use a string literal");
        };
        let syn::Expr::Lit(expression) = &value.value else {
            bail!("explicit Rust module path must use a string literal");
        };
        let syn::Lit::Str(value) = &expression.lit else {
            bail!("explicit Rust module path must use a string literal");
        };
        let path = PathBuf::from(value.value());
        normalized_module_path(&path).context("explicit Rust module path must name a normalized .rs source in the audited source tree")?;
        paths.push(path);
        return Ok(());
    }
    if !meta_has_normalized_ident(meta, "cfg_attr") {
        return Ok(());
    }
    let Meta::List(list) = meta else {
        return Ok(());
    };
    let nested = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .context("parse cfg_attr while resolving an audited Cargo target module")?;
    for meta in nested.iter().skip(1) {
        collect_audited_module_paths(meta, paths)?;
    }
    Ok(())
}

fn meta_has_normalized_ident(meta: &Meta, expected: &str) -> bool {
    meta.path().get_ident().is_some_and(|ident| ident.unraw() == expected)
}

fn direct_module_path(attributes: &[syn::Attribute]) -> Result<Option<PathBuf>> {
    let mut direct = Vec::new();
    for attribute in attributes {
        let paths = audited_module_paths(&attribute.meta)?;
        if !paths.is_empty() && !meta_has_normalized_ident(&attribute.meta, "path") {
            bail!("conditional explicit module paths are unsupported by suppression governance");
        }
        direct.extend(paths);
    }
    direct.sort();
    direct.dedup();
    match direct.as_slice() {
        [] => Ok(None),
        [path] => Ok(Some(path.clone())),
        _ => bail!("Cargo target module has multiple explicit source paths"),
    }
}

fn child_module_base(source: &Path) -> PathBuf {
    if source.file_name().and_then(|name| name.to_str()) == Some("mod.rs") {
        source.parent().unwrap_or_else(|| Path::new("")).to_path_buf()
    } else {
        source.with_extension("")
    }
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
