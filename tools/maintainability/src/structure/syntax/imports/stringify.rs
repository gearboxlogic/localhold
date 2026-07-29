use std::collections::BTreeSet;

use anyhow::{Context, Result};
use syn::{Block, File, Item, ItemUse, Stmt};

use super::super::{ProductionCfgContext, item_attributes, normalized_ident, production_cfg_context};
use super::resolution::flatten_use_tree;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct BuiltinStringifyAlias {
    pub(super) module: Vec<String>,
    pub(super) name: String,
    pub(super) cfg: ProductionCfgContext,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ModuleImportShadow {
    pub(super) module: Vec<String>,
    pub(super) name: String,
    pub(super) cfg: ProductionCfgContext,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct BlockBuiltinStringifyAlias {
    pub(super) name: String,
    pub(super) cfg: ProductionCfgContext,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct MacroShadow {
    pub(super) name: String,
    pub(super) cfg: ProductionCfgContext,
}

pub(super) struct BlockStringifyImports {
    pub(super) aliases: BTreeSet<BlockBuiltinStringifyAlias>,
    pub(super) shadows: BTreeSet<MacroShadow>,
}

pub(super) struct ModuleStringifyImports {
    pub(super) aliases: BTreeSet<BuiltinStringifyAlias>,
    pub(super) shadows: BTreeSet<ModuleImportShadow>,
}

pub(super) fn binding_is_fully_builtin(invocation_cfg: &ProductionCfgContext, builtin_cfgs: &[&ProductionCfgContext], shadow_cfgs: &[&ProductionCfgContext]) -> Option<bool> {
    let shadowed = shadow_cfgs.iter().any(|shadow| invocation_cfg.conjoin(shadow).is_some());
    let compatible_builtins = builtin_cfgs.iter().copied().filter(|builtin| invocation_cfg.conjoin(builtin).is_some()).collect::<Vec<_>>();
    if compatible_builtins.is_empty() && !shadowed {
        return None;
    }
    let mut uncovered = vec![invocation_cfg.clone()];
    for builtin in compatible_builtins {
        uncovered = uncovered.into_iter().filter_map(|region| region.excluding(builtin)).collect();
    }
    Some(!shadowed && uncovered.is_empty())
}

pub(super) fn stringify_imports_in_block(block: &Block, inherited_cfg: &ProductionCfgContext) -> Result<BlockStringifyImports> {
    let mut aliases = BTreeSet::new();
    let mut shadows = BTreeSet::new();
    for statement in &block.stmts {
        let Stmt::Item(Item::Use(item)) = statement else {
            continue;
        };
        let Some(cfg) = production_cfg_context(&item.attrs, inherited_cfg)? else {
            continue;
        };
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for path in paths.into_iter().filter(|path| path.segments.last().is_some_and(|segment| segment != "*")) {
            let name = path.alias.clone().or_else(|| path.segments.last().cloned()).context("block import has no bound name")?;
            if is_builtin_stringify_import(item, &path.segments) {
                aliases.insert(BlockBuiltinStringifyAlias { name, cfg: cfg.clone() });
            } else {
                shadows.insert(MacroShadow { name, cfg: cfg.clone() });
            }
        }
    }
    Ok(BlockStringifyImports { aliases, shadows })
}

pub(super) fn collect_module_stringify_imports(file: &File, module: &[String], inherited_cfg: &ProductionCfgContext) -> Result<ModuleStringifyImports> {
    let mut imports = ModuleStringifyImports {
        aliases: BTreeSet::new(),
        shadows: BTreeSet::new(),
    };
    collect_module_stringify_imports_in_items(&file.items, module, inherited_cfg, &mut imports)?;
    Ok(imports)
}

fn collect_module_stringify_imports_in_items(items: &[Item], module: &[String], inherited_cfg: &ProductionCfgContext, imports: &mut ModuleStringifyImports) -> Result<()> {
    for item in items {
        let Some(cfg) = production_cfg_context(item_attributes(item)?, inherited_cfg)? else {
            continue;
        };
        match item {
            Item::Use(item) => collect_module_stringify_imports_from_use(item, module, &cfg, imports)?,
            Item::Mod(item) => {
                if let Some((_, nested)) = &item.content {
                    let mut nested_module = module.to_vec();
                    nested_module.push(normalized_ident(&item.ident));
                    collect_module_stringify_imports_in_items(nested, &nested_module, &cfg, imports)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn collect_module_stringify_imports_from_use(item: &ItemUse, module: &[String], cfg: &ProductionCfgContext, imports: &mut ModuleStringifyImports) -> Result<()> {
    let mut paths = Vec::new();
    flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
    for path in paths.into_iter().filter(|path| path.segments.last().is_some_and(|segment| segment != "*")) {
        let name = path.alias.or_else(|| path.segments.last().cloned()).context("module import has no bound name")?;
        if is_builtin_stringify_import(item, &path.segments) {
            imports.aliases.insert(BuiltinStringifyAlias {
                module: module.to_vec(),
                name,
                cfg: cfg.clone(),
            });
        } else {
            imports.shadows.insert(ModuleImportShadow {
                module: module.to_vec(),
                name,
                cfg: cfg.clone(),
            });
        }
    }
    Ok(())
}

fn is_builtin_stringify_import(item: &ItemUse, segments: &[String]) -> bool {
    item.leading_colon.is_some()
        && matches!(
            segments,
            [root, imported] if matches!(root.as_str(), "core" | "std") && imported == "stringify"
        )
}

pub(super) fn is_explicit_builtin_stringify(node: &syn::Macro) -> bool {
    node.path.leading_colon.is_some()
        && node.path.segments.len() == 2
        && matches!(normalized_ident(&node.path.segments[0].ident).as_str(), "core" | "std")
        && normalized_ident(&node.path.segments[1].ident) == "stringify"
}
