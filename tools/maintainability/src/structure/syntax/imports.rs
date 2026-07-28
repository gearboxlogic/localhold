use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;
use syn::visit::{self, Visit};
use syn::{
    Arm, Attribute, BareFnArg, BareVariadic, Block, Expr, Field, FieldPat, FieldValue, File, FnArg, ForeignItem, ForeignItemFn, ForeignItemStatic, GenericParam, Generics,
    ImplItem, ImplItemConst, ImplItemFn, ImplItemType, Item, ItemConst, ItemEnum, ItemExternCrate, ItemFn, ItemImpl, ItemMacro, ItemMod, ItemStatic, ItemStruct, ItemTrait,
    ItemType, ItemUnion, ItemUse, Local, Pat, Path as SynPath, Stmt, StmtMacro, TraitItem, TraitItemConst, TraitItemFn, TraitItemType, Variadic, Variant, Visibility,
};

use crate::scan::{reviewed_attribute_expansion, reviewed_macro_expansion, syntax_fingerprint};

use super::{
    ProductionCfgContext, expr_attributes, fn_arg_attributes, foreign_item_attributes, generic_param_attributes, impl_item_attributes, item_attributes, normalized_ident,
    pat_attributes, production_cfg_context, trait_item_attributes,
};

mod concrete;
mod macro_definitions;
mod reexports;
mod resolution;
mod tokens;
pub use concrete::{ConcreteStoreCounts, ConcreteStoreSignatureSite, ConcreteStoreSignatureSites, ConcreteStoreSites};
use concrete::{ConcreteStoreInventory, context_fingerprint, is_concrete_store_name};
use macro_definitions::contains_production_concrete_store;
use reexports::{PendingPublicReexport, UseResolution, resolve_public_reexport_aliases};
use resolution::{StringScan, UsePath, flatten_use_tree, resolve_path, restricted_attribute_identifier, restricted_token_identifier, source_module};
use tokens::resolving_tokens;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BuiltinStringifyAlias {
    module: Vec<String>,
    name: String,
    cfg: ProductionCfgContext,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BlockBuiltinStringifyAlias {
    name: String,
    cfg: ProductionCfgContext,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MacroShadow {
    name: String,
    cfg: ProductionCfgContext,
}

struct BlockStringifyImports {
    aliases: BTreeSet<BlockBuiltinStringifyAlias>,
    shadows: BTreeSet<MacroShadow>,
}

#[derive(Clone, Copy)]
enum FieldExposure {
    Struct(bool),
    Enum(bool),
    Union(bool),
}

type BuiltinStringifyAliases = BTreeSet<BuiltinStringifyAlias>;

#[derive(Default)]
pub struct ProductionSyntaxFacts {
    pub module: Vec<String>,
    pub internal_imports: Vec<String>,
    pub public_reexports: Vec<PublicReexportEvidence>,
    pub concrete_stores: ConcreteStoreCounts,
    pub public_concrete_store_structs: ConcreteStoreSites,
    pub concrete_store_sites: ConcreteStoreSites,
    pub generic_default_concrete_store_sites: ConcreteStoreSites,
    pub signature_concrete_store_sites: ConcreteStoreSignatureSites,
    pub binding_concrete_store_sites: ConcreteStoreSites,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PublicReexportEvidence {
    pub exported_path: Vec<String>,
    pub target_path: Vec<String>,
    pub fingerprint: String,
    #[serde(skip)]
    pub(in crate::structure) cfg: ProductionCfgContext,
}

#[derive(Clone, Copy)]
pub struct ProductionSyntaxOptions {
    pub collect_internal_imports: bool,
    pub rust_2015_absolute_paths: bool,
    pub require_reviewed_expansions: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::structure) struct ProductionAncestorPath {
    pub(in crate::structure) cfg: ProductionCfgContext,
    pub(in crate::structure) ancestors: Vec<String>,
}

#[derive(Default)]
pub(in crate::structure) struct ProductionSyntaxContext {
    pub(in crate::structure) cfg: ProductionCfgContext,
    pub(in crate::structure) declaration_ancestors: Vec<ProductionAncestorPath>,
}

#[cfg(test)]
pub fn production_syntax_facts(file: &syn::File, source_path: &str, crate_root: Option<&str>, options: ProductionSyntaxOptions) -> Result<ProductionSyntaxFacts> {
    production_syntax_facts_with_context(file, source_path, crate_root, options, ProductionSyntaxContext::default())
}

pub(in crate::structure) fn production_syntax_facts_with_context(
    file: &syn::File,
    source_path: &str,
    crate_root: Option<&str>,
    options: ProductionSyntaxOptions,
    initial_context: ProductionSyntaxContext,
) -> Result<ProductionSyntaxFacts> {
    let module = if source_path.starts_with("src/") || crate_root.is_some() {
        source_module(source_path, crate_root)?
    } else {
        Vec::new()
    };
    let builtin_stringify_aliases = collect_builtin_stringify_aliases(file, &module, &initial_context.cfg)?;
    let mut collector = ProductionSyntaxCollector {
        module,
        builtin_stringify_aliases,
        builtin_stringify_block_aliases: Vec::new(),
        macro_import_shadow_scopes: Vec::new(),
        imports: Vec::new(),
        use_resolutions: Vec::new(),
        public_reexports: Vec::new(),
        concrete_stores: ConcreteStoreInventory::default(),
        site_context: None,
        block_depth: 0,
        macro_shadow_scopes: vec![BTreeSet::new()],
        generic_default_depth: 0,
        impl_signature_headers: Vec::new(),
        impl_trait_exposures: Vec::new(),
        trait_exposures: Vec::new(),
        field_exposures: Vec::new(),
        impl_item_paths: Vec::new(),
        inherited_declaration_ancestors: initial_context.declaration_ancestors,
        declaration_ancestors: Vec::new(),
        cfg_context: initial_context.cfg,
        error: None,
        rust_2015_absolute_paths: options.rust_2015_absolute_paths,
        collect_internal_imports: options.collect_internal_imports,
        require_reviewed_expansions: options.require_reviewed_expansions,
    };
    collector.visit_file(file);
    if let Some(error) = collector.error {
        return Err(error);
    }
    collector.imports.sort();
    collector.imports.dedup();
    let mut public_reexports = resolve_public_reexport_aliases(collector.public_reexports, &collector.use_resolutions);
    public_reexports.sort();
    public_reexports.dedup();
    collector.concrete_stores.finish();
    Ok(ProductionSyntaxFacts {
        module: collector.module,
        internal_imports: collector.imports,
        public_reexports,
        concrete_stores: collector.concrete_stores.counts,
        public_concrete_store_structs: collector.concrete_stores.public_struct_declarations,
        concrete_store_sites: collector.concrete_stores.sites,
        generic_default_concrete_store_sites: collector.concrete_stores.generic_default_sites,
        signature_concrete_store_sites: collector.concrete_stores.signature_sites,
        binding_concrete_store_sites: collector.concrete_stores.binding_sites,
    })
}

struct ProductionSyntaxCollector {
    module: Vec<String>,
    builtin_stringify_aliases: BuiltinStringifyAliases,
    builtin_stringify_block_aliases: Vec<BTreeSet<BlockBuiltinStringifyAlias>>,
    macro_import_shadow_scopes: Vec<BTreeSet<MacroShadow>>,
    imports: Vec<String>,
    use_resolutions: Vec<UseResolution>,
    public_reexports: Vec<PendingPublicReexport>,
    concrete_stores: ConcreteStoreInventory,
    site_context: Option<String>,
    block_depth: usize,
    macro_shadow_scopes: Vec<BTreeSet<MacroShadow>>,
    generic_default_depth: usize,
    impl_signature_headers: Vec<TokenStream>,
    impl_trait_exposures: Vec<bool>,
    trait_exposures: Vec<bool>,
    field_exposures: Vec<FieldExposure>,
    impl_item_paths: Vec<Vec<String>>,
    inherited_declaration_ancestors: Vec<ProductionAncestorPath>,
    declaration_ancestors: Vec<String>,
    cfg_context: ProductionCfgContext,
    error: Option<anyhow::Error>,
    rust_2015_absolute_paths: bool,
    collect_internal_imports: bool,
    require_reviewed_expansions: bool,
}

impl ProductionSyntaxCollector {
    fn collect_use(&mut self, item: &ItemUse) -> Result<()> {
        if item.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(());
        }
        let import_count = self.imports.len();
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for mut path in paths {
            if path.renamed && path.segments.iter().any(|segment| is_concrete_store_name(segment)) {
                bail!("production concrete stores cannot be hidden behind renamed imports");
            }
            if item.leading_colon.is_some() {
                path.segments.insert(0, "crate".to_owned());
            }
            self.collect_path(&path)?;
        }
        if self.imports.len() != import_count && !matches!(item.vis, Visibility::Inherited) {
            bail!("production restricted imports cannot be re-exported");
        }
        Ok(())
    }

    fn record_public_reexport(&mut self, item: &ItemUse) -> Result<()> {
        if !visibility_is_exposed(&item.vis) || item.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(());
        }
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for mut path in paths {
            if item.leading_colon.is_some() {
                path.segments.insert(0, "crate".to_owned());
            }
            let Some(target_path) = resolve_path(&self.module, &path.segments, self.rust_2015_absolute_paths)? else {
                continue;
            };
            let mut exported_path = self.module.clone();
            exported_path.push(
                path.alias
                    .clone()
                    .or_else(|| target_path.last().cloned())
                    .context("production public re-export has no exported name")?,
            );
            let identity = format!(
                "public-reexport:{}\0alias:{}\0visibility:{}\0cfg:{}\0ancestors:{}",
                target_path.join("::"),
                path.alias.as_deref().unwrap_or_default(),
                syntax_fingerprint(&item.vis),
                self.cfg_context.identity(),
                self.declaration_ancestor_identity()
            );
            self.public_reexports.push(PendingPublicReexport {
                evidence: PublicReexportEvidence {
                    exported_path,
                    target_path,
                    fingerprint: syntax_fingerprint(&identity),
                    cfg: self.cfg_context.clone(),
                },
                cfg: self.cfg_context.clone(),
            });
        }
        Ok(())
    }

    fn record_use_resolutions(&mut self, item: &ItemUse) -> Result<()> {
        if item.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(());
        }
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for mut path in paths {
            if item.leading_colon.is_some() {
                path.segments.insert(0, "crate".to_owned());
            }
            let Some(target_path) = resolve_path(&self.module, &path.segments, self.rust_2015_absolute_paths)? else {
                continue;
            };
            let mut exported_path = self.module.clone();
            exported_path.push(path.alias.clone().or_else(|| target_path.last().cloned()).context("production use has no imported name")?);
            let identity = format!(
                "use-resolution:{}\0alias:{}\0visibility:{}\0cfg:{}\0ancestors:{}",
                target_path.join("::"),
                path.alias.as_deref().unwrap_or_default(),
                syntax_fingerprint(&item.vis),
                self.cfg_context.identity(),
                self.declaration_ancestor_identity()
            );
            self.use_resolutions.push(UseResolution {
                exported_path,
                target_path,
                fingerprint: syntax_fingerprint(&identity),
                cfg: self.cfg_context.clone(),
            });
        }
        Ok(())
    }

    fn collect_path(&mut self, path: &UsePath) -> Result<()> {
        self.collect_segments(&path.segments, path.renamed, self.rust_2015_absolute_paths)
    }

    fn collect_segments(&mut self, segments: &[String], renamed: bool, rust_2015_use_path: bool) -> Result<()> {
        if !self.collect_internal_imports {
            return Ok(());
        }
        let Some(resolved) = resolve_path(&self.module, segments, rust_2015_use_path)? else {
            return Ok(());
        };
        if resolved.is_empty() && renamed {
            bail!("production crate-root import aliases cannot be classified safely for dependency boundaries");
        }
        if resolved.as_slice() == ["*"] {
            bail!("production crate-root glob imports cannot be classified safely for dependency boundaries");
        }
        if matches!(resolved.first().map(String::as_str), Some("server" | "ui")) {
            self.imports.push(format!("crate::{}", resolved.join("::")));
        }
        Ok(())
    }

    fn enter_production_node(&mut self, attributes: Result<&[Attribute]>) -> Option<ProductionCfgContext> {
        if self.error.is_some() {
            return None;
        }
        let active = match attributes.and_then(|attributes| production_cfg_context(attributes, &self.cfg_context)) {
            Ok(Some(active)) => active,
            Ok(None) => return None,
            Err(error) => {
                self.error = Some(error);
                return None;
            }
        };
        Some(std::mem::replace(&mut self.cfg_context, active))
    }

    fn leave_production_node(&mut self, previous: ProductionCfgContext) {
        self.cfg_context = previous;
    }

    fn record_concrete_store(&mut self, ident: &proc_macro2::Ident) {
        let context = self.site_context.as_deref().unwrap_or("unscoped-production-syntax");
        if let Err(error) = self.concrete_stores.record_ident(ident, context) {
            self.error = Some(error);
        }
        if self.generic_default_depth > 0 {
            self.concrete_stores.record_generic_default_ident(ident, context);
        }
    }

    fn reject_concrete_store_alias(&mut self, before: ConcreteStoreCounts) {
        if self.error.is_none() && self.concrete_stores.counts != before {
            self.error = Some(anyhow::anyhow!("production concrete stores cannot be hidden behind type aliases"));
        }
    }

    fn record_concrete_stores_in_tokens(&mut self, tokens: &TokenStream) {
        let context = self.site_context.as_deref().unwrap_or("unscoped-production-syntax");
        if let Err(error) = self.concrete_stores.record_tokens(tokens, context) {
            self.error = Some(error);
        }
        if self.generic_default_depth > 0 {
            self.concrete_stores.record_generic_default_tokens(tokens, context);
        }
    }

    fn record_concrete_stores_in_signature(&mut self, kind: &str, syntax: &impl ToTokens) {
        let tokens = syntax.to_token_stream();
        self.record_concrete_stores_in_signature_with_identity(kind, &tokens, &tokens);
    }

    fn record_concrete_stores_in_visible_signature(&mut self, kind: &str, visibility: &Visibility, syntax: &impl ToTokens) {
        let tokens = syntax.to_token_stream();
        let mut identity = visibility.to_token_stream();
        identity.extend(tokens.clone());
        self.record_concrete_stores_in_signature_with_identity(kind, &tokens, &identity);
    }

    fn record_declaration_generics(&mut self, kind: &str, visibility: &Visibility, generics: &Generics) {
        let mut tokens = generics.to_token_stream();
        if let Some(where_clause) = &generics.where_clause {
            tokens.extend(where_clause.to_token_stream());
        }
        self.record_concrete_stores_in_visible_signature(kind, visibility, &tokens);
    }

    fn production_signature(&self, signature: &syn::Signature) -> Result<syn::Signature> {
        let mut production = signature.clone();
        let mut inputs = Vec::new();
        for input in &signature.inputs {
            if production_cfg_context(fn_arg_attributes(input), &self.cfg_context)?.is_some() {
                inputs.push(input.clone());
            }
        }
        production.inputs = inputs.into_iter().collect();

        let mut parameters = Vec::new();
        for parameter in &signature.generics.params {
            if production_cfg_context(generic_param_attributes(parameter), &self.cfg_context)?.is_some() {
                parameters.push(parameter.clone());
            }
        }
        production.generics.params = parameters.into_iter().collect();

        if let Some(variadic) = &signature.variadic
            && production_cfg_context(&variadic.attrs, &self.cfg_context)?.is_none()
        {
            production.variadic = None;
        }
        Ok(production)
    }

    fn record_concrete_stores_in_signature_with_identity(&mut self, kind: &str, tokens: &TokenStream, identity: &TokenStream) {
        let context = format!(
            "{kind}:{}\0cfg:{}\0ancestors:{}",
            syntax_fingerprint(identity),
            self.cfg_context.identity(),
            self.declaration_ancestor_identity()
        );
        let item_path = self.signature_item_path();
        self.concrete_stores.record_signature_tokens(tokens, &context, &item_path, &self.cfg_context);
    }

    fn record_concrete_stores_in_exposure_signature_with_identity(&mut self, kind: &str, tokens: &TokenStream, identity: &TokenStream) {
        let context = format!(
            "{kind}:{}\0cfg:{}\0ancestors:{}",
            syntax_fingerprint(identity),
            self.cfg_context.identity(),
            self.declaration_ancestor_identity()
        );
        let item_path = self.signature_item_path();
        self.concrete_stores.record_exposure_signature_tokens(tokens, &context, &item_path, &self.cfg_context);
    }

    fn record_impl_header_for_visible_member(&mut self, kind: &str, visibility: &Visibility, member: &impl ToTokens) {
        if !visibility_is_exposed(visibility) {
            return;
        }
        let Some(header) = self.impl_signature_headers.last().cloned() else {
            return;
        };
        let mut identity = header.clone();
        identity.extend(visibility.to_token_stream());
        identity.extend(member.to_token_stream());
        self.record_concrete_stores_in_exposure_signature_with_identity(kind, &header, &identity);
    }

    fn impl_member_is_exposed(&self, visibility: &Visibility) -> bool {
        visibility_is_exposed(visibility) || self.impl_trait_exposures.last().copied().unwrap_or(false)
    }

    fn trait_member_is_exposed(&self) -> bool {
        self.trait_exposures.last().copied().unwrap_or(false)
    }

    fn field_is_exposed(&self, visibility: &Visibility) -> bool {
        match self.field_exposures.last() {
            Some(FieldExposure::Enum(container_exposed)) => *container_exposed,
            Some(FieldExposure::Struct(container_exposed) | FieldExposure::Union(container_exposed)) => *container_exposed && visibility_is_exposed(visibility),
            None => visibility_is_exposed(visibility),
        }
    }

    fn enter_site_context(&mut self, kind: &str, syntax: &impl ToTokens) -> Option<String> {
        let context = context_fingerprint(self.site_context.as_deref(), kind, syntax);
        self.site_context.replace(context)
    }

    fn leave_site_context(&mut self, previous: Option<String>) {
        self.site_context = previous;
    }

    fn visit_generic_default(&mut self, visit: impl FnOnce(&mut Self)) {
        self.generic_default_depth += 1;
        visit(self);
        self.generic_default_depth -= 1;
    }

    fn signature_item_path(&self) -> Vec<String> {
        if self.block_depth > 0 {
            return Vec::new();
        }
        if let Some(path) = self.impl_item_paths.last().filter(|path| !path.is_empty()) {
            return path.clone();
        }
        let Some(name) = self.declaration_ancestors.iter().rev().find_map(|ancestor| {
            ["const:", "enum:", "fn:", "static:", "struct:", "trait:", "union:"]
                .into_iter()
                .find_map(|prefix| ancestor.strip_prefix(prefix))
                .and_then(|suffix| suffix.split_once(':').map(|(name, _)| name))
        }) else {
            return Vec::new();
        };
        let mut path = self.module.clone();
        path.push(name.to_owned());
        path
    }

    fn implemented_type_path(&self, item: &ItemImpl) -> Result<Vec<String>> {
        let syn::Type::Path(path) = item.self_ty.as_ref() else {
            return Ok(Vec::new());
        };
        if path.qself.is_some() || path.path.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(Vec::new());
        }
        let mut segments = path.path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
        if path.path.leading_colon.is_some() {
            segments.insert(0, "crate".to_owned());
        }
        Ok(resolve_path(&self.module, &segments, false)?.unwrap_or_default())
    }

    fn declaration_ancestor_identity(&self) -> String {
        self.inherited_declaration_ancestors
            .iter()
            .filter(|path| path.cfg.conjoin(&self.cfg_context).is_some())
            .map(|path| format!("out-of-line-module-path:cfg:{}\0ancestors:{}", path.cfg.identity(), path.ancestors.join("\0")))
            .chain(self.declaration_ancestors.iter().cloned())
            .collect::<Vec<_>>()
            .join("\0")
    }
}

macro_rules! visit_production_node {
    ($method:ident, $walk:ident, $node:ty, $binding:ident => $attributes:expr) => {
        fn $method(&mut self, $binding: &'ast $node) {
            let attributes: Result<&[Attribute]> = $attributes;
            let Some(previous) = self.enter_production_node(attributes) else {
                return;
            };
            visit::$walk(self, $binding);
            self.leave_production_node(previous);
        }
    };
}

impl<'ast> Visit<'ast> for ProductionSyntaxCollector {
    visit_production_node!(visit_file, visit_file, File, node => Ok(node.attrs.as_slice()));
    visit_production_node!(visit_expr, visit_expr, Expr, node => expr_attributes(node));
    visit_production_node!(visit_arm, visit_arm, Arm, node => Ok(node.attrs.as_slice()));
    visit_production_node!(visit_local, visit_local, Local, node => Ok(node.attrs.as_slice()));
    visit_production_node!(visit_stmt_macro, visit_stmt_macro, StmtMacro, node => Ok(node.attrs.as_slice()));
    visit_production_node!(
        visit_fn_arg,
        visit_fn_arg,
        FnArg,
        node => Ok(fn_arg_attributes(node))
    );
    visit_production_node!(visit_pat, visit_pat, Pat, node => pat_attributes(node));
    visit_production_node!(
        visit_bare_fn_arg,
        visit_bare_fn_arg,
        BareFnArg,
        node => Ok(node.attrs.as_slice())
    );
    visit_production_node!(
        visit_bare_variadic,
        visit_bare_variadic,
        BareVariadic,
        node => Ok(node.attrs.as_slice())
    );
    visit_production_node!(
        visit_variadic,
        visit_variadic,
        Variadic,
        node => Ok(node.attrs.as_slice())
    );
    visit_production_node!(
        visit_field_pat,
        visit_field_pat,
        FieldPat,
        node => Ok(node.attrs.as_slice())
    );
    visit_production_node!(
        visit_field_value,
        visit_field_value,
        FieldValue,
        node => Ok(node.attrs.as_slice())
    );

    fn visit_block(&mut self, node: &'ast Block) {
        let imports = match stringify_imports_in_block(node, &self.cfg_context) {
            Ok(imports) => imports,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.block_depth += 1;
        self.macro_shadow_scopes.push(BTreeSet::new());
        self.builtin_stringify_block_aliases.push(imports.aliases);
        self.macro_import_shadow_scopes.push(imports.shadows);
        visit::visit_block(self, node);
        self.macro_import_shadow_scopes.pop();
        self.builtin_stringify_block_aliases.pop();
        self.macro_shadow_scopes.pop();
        self.block_depth -= 1;
    }

    fn visit_item(&mut self, node: &'ast Item) {
        let Some(cfg) = self.enter_production_node(item_attributes(node)) else {
            return;
        };
        let previous = self.enter_site_context("item", node);
        let ancestor = declaration_ancestor(node);
        if let Some(ancestor) = &ancestor {
            self.declaration_ancestors.push(ancestor.clone());
        }
        visit::visit_item(self, node);
        if ancestor.is_some() {
            self.declaration_ancestors.pop();
        }
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_impl_item(&mut self, node: &'ast ImplItem) {
        let Some(cfg) = self.enter_production_node(impl_item_attributes(node)) else {
            return;
        };
        let previous = self.enter_site_context("impl-item", node);
        visit::visit_impl_item(self, node);
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_trait_item(&mut self, node: &'ast TraitItem) {
        let Some(cfg) = self.enter_production_node(trait_item_attributes(node)) else {
            return;
        };
        let previous = self.enter_site_context("trait-item", node);
        visit::visit_trait_item(self, node);
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_foreign_item(&mut self, node: &'ast ForeignItem) {
        let Some(cfg) = self.enter_production_node(foreign_item_attributes(node)) else {
            return;
        };
        let previous = self.enter_site_context("foreign-item", node);
        visit::visit_foreign_item(self, node);
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_stmt(&mut self, node: &'ast Stmt) {
        let previous = self.enter_site_context("statement", node);
        visit::visit_stmt(self, node);
        self.leave_site_context(previous);
    }

    fn visit_field(&mut self, node: &'ast Field) {
        let Some(cfg) = self.enter_production_node(Ok(node.attrs.as_slice())) else {
            return;
        };
        let previous = self.enter_site_context("field", node);
        if self.field_is_exposed(&node.vis) {
            self.record_concrete_stores_in_visible_signature("field-type", &node.vis, &node.ty);
        }
        visit::visit_field(self, node);
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_variant(&mut self, node: &'ast Variant) {
        let Some(cfg) = self.enter_production_node(Ok(node.attrs.as_slice())) else {
            return;
        };
        self.declaration_ancestors.push(format!("variant:{}", normalized_ident(&node.ident)));
        visit::visit_variant(self, node);
        self.declaration_ancestors.pop();
        self.leave_production_node(cfg);
    }

    fn visit_generic_param(&mut self, node: &'ast GenericParam) {
        let Some(cfg) = self.enter_production_node(Ok(generic_param_attributes(node))) else {
            return;
        };
        let previous = self.enter_site_context("generic-parameter", node);
        match node {
            GenericParam::Lifetime(parameter) => visit::visit_lifetime_param(self, parameter),
            GenericParam::Type(parameter) => {
                for attribute in &parameter.attrs {
                    self.visit_attribute(attribute);
                }
                self.visit_ident(&parameter.ident);
                for bound in &parameter.bounds {
                    self.visit_type_param_bound(bound);
                }
                if let Some(default) = &parameter.default {
                    self.visit_generic_default(|collector| collector.visit_type(default));
                }
            }
            GenericParam::Const(parameter) => {
                for attribute in &parameter.attrs {
                    self.visit_attribute(attribute);
                }
                self.visit_ident(&parameter.ident);
                self.visit_type(&parameter.ty);
                if let Some(default) = &parameter.default {
                    self.visit_generic_default(|collector| collector.visit_expr(default));
                }
            }
        }
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        let previous = self.enter_site_context("use", item);
        let result = self.collect_use(item).and_then(|()| {
            if self.block_depth == 0 {
                self.record_use_resolutions(item)?;
                self.record_public_reexport(item)?;
            }
            Ok(())
        });
        if self.error.is_none()
            && let Err(error) = result
        {
            self.error = Some(error);
            self.leave_site_context(previous);
            return;
        }
        visit::visit_item_use(self, item);
        self.leave_site_context(previous);
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        let exposed = visibility_is_exposed(&item.vis);
        if exposed {
            self.record_declaration_generics("struct-generics", &item.vis, &item.generics);
        }
        if matches!(item.vis, Visibility::Public(_)) {
            let mut item_path = self.module.clone();
            item_path.push(normalized_ident(&item.ident));
            let ancestors = self.declaration_ancestor_identity();
            if let Err(error) = self.concrete_stores.record_public_struct_declaration(item, &item_path, &self.cfg_context, &ancestors) {
                self.error = Some(error);
                return;
            }
        }
        self.field_exposures.push(FieldExposure::Struct(exposed));
        visit::visit_item_struct(self, item);
        self.field_exposures.pop();
    }

    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        let exposed = visibility_is_exposed(&item.vis);
        if exposed {
            self.record_declaration_generics("enum-generics", &item.vis, &item.generics);
        }
        self.field_exposures.push(FieldExposure::Enum(exposed));
        visit::visit_item_enum(self, item);
        self.field_exposures.pop();
    }

    fn visit_item_union(&mut self, item: &'ast ItemUnion) {
        let exposed = visibility_is_exposed(&item.vis);
        if exposed {
            self.record_declaration_generics("union-generics", &item.vis, &item.generics);
        }
        self.field_exposures.push(FieldExposure::Union(exposed));
        visit::visit_item_union(self, item);
        self.field_exposures.pop();
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        let exposed = visibility_is_exposed(&item.vis);
        if exposed {
            let mut header = item.clone();
            header.attrs.clear();
            header.items.clear();
            self.record_concrete_stores_in_signature("trait-header", &header);
        }
        self.trait_exposures.push(exposed);
        visit::visit_item_trait(self, item);
        self.trait_exposures.pop();
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        let item_path = match self.implemented_type_path(item) {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.impl_item_paths.push(item_path);
        let mut binding_item = item.clone();
        strip_impl_documentation(&mut binding_item);
        let mut header = binding_item.clone();
        header.items.clear();
        let binding = if item.trait_.is_some() {
            format!("trait-implementation:{}", syntax_fingerprint(&binding_item))
        } else {
            format!("impl-header:{}", syntax_fingerprint(&header))
        };
        let context = format!("{binding}\0cfg:{}\0ancestors:{}", self.cfg_context.identity(), self.declaration_ancestor_identity());
        self.concrete_stores.record_binding_tokens(&header.to_token_stream(), &context);
        let trait_exposure = item.trait_.is_some();
        if trait_exposure {
            let tokens = header.to_token_stream();
            self.record_concrete_stores_in_exposure_signature_with_identity("trait-impl-header", &tokens, &tokens);
        }
        self.impl_signature_headers.push(header.to_token_stream());
        self.impl_trait_exposures.push(trait_exposure);
        visit::visit_item_impl(self, item);
        self.impl_trait_exposures.pop();
        self.impl_signature_headers.pop();
        self.impl_item_paths.pop();
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if visibility_is_exposed(&item.vis) {
            match self.production_signature(&item.sig) {
                Ok(signature) => self.record_concrete_stores_in_visible_signature("function-signature", &item.vis, &signature),
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        let signature = match self.production_signature(&item.sig) {
            Ok(signature) => signature,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.record_impl_header_for_visible_member("inherent-impl-method", &item.vis, &signature);
        if self.impl_member_is_exposed(&item.vis) {
            self.record_concrete_stores_in_visible_signature("method-signature", &item.vis, &signature);
        }
        visit::visit_impl_item_fn(self, item);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast TraitItemFn) {
        if self.trait_member_is_exposed() {
            match self.production_signature(&item.sig) {
                Ok(signature) => self.record_concrete_stores_in_signature("trait-method-signature", &signature),
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        visit::visit_trait_item_fn(self, item);
    }

    fn visit_foreign_item_fn(&mut self, item: &'ast ForeignItemFn) {
        if visibility_is_exposed(&item.vis) {
            match self.production_signature(&item.sig) {
                Ok(signature) => self.record_concrete_stores_in_visible_signature("foreign-function-signature", &item.vis, &signature),
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        visit::visit_foreign_item_fn(self, item);
    }

    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        if visibility_is_exposed(&item.vis) {
            self.record_concrete_stores_in_visible_signature("const-type", &item.vis, &item.ty);
        }
        visit::visit_item_const(self, item);
    }

    fn visit_item_static(&mut self, item: &'ast ItemStatic) {
        if visibility_is_exposed(&item.vis) {
            self.record_concrete_stores_in_visible_signature("static-type", &item.vis, &item.ty);
        }
        visit::visit_item_static(self, item);
    }

    fn visit_impl_item_const(&mut self, item: &'ast ImplItemConst) {
        self.record_impl_header_for_visible_member("inherent-impl-const", &item.vis, &item.ty);
        if self.impl_member_is_exposed(&item.vis) {
            self.record_concrete_stores_in_visible_signature("associated-const-type", &item.vis, &item.ty);
        }
        visit::visit_impl_item_const(self, item);
    }

    fn visit_trait_item_const(&mut self, item: &'ast TraitItemConst) {
        if self.trait_member_is_exposed() {
            self.record_concrete_stores_in_signature("trait-const-type", &item.ty);
        }
        visit::visit_trait_item_const(self, item);
    }

    fn visit_foreign_item_static(&mut self, item: &'ast ForeignItemStatic) {
        if visibility_is_exposed(&item.vis) {
            self.record_concrete_stores_in_visible_signature("foreign-static-type", &item.vis, &item.ty);
        }
        visit::visit_foreign_item_static(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast ItemType) {
        let import_count = self.imports.len();
        let concrete_before = self.concrete_stores.counts;
        visit::visit_item_type(self, item);
        if self.error.is_none() && self.imports.len() != import_count && visibility_is_exposed(&item.vis) {
            self.error = Some(anyhow::anyhow!("production restricted imports cannot be exposed through public type aliases"));
        }
        self.reject_concrete_store_alias(concrete_before);
    }

    fn visit_impl_item_type(&mut self, item: &'ast ImplItemType) {
        let concrete_before = self.concrete_stores.counts;
        self.record_impl_header_for_visible_member("inherent-impl-type", &item.vis, &item.ty);
        visit::visit_impl_item_type(self, item);
        self.reject_concrete_store_alias(concrete_before);
    }

    fn visit_trait_item_type(&mut self, item: &'ast TraitItemType) {
        let concrete_before = self.concrete_stores.counts;
        visit::visit_trait_item_type(self, item);
        self.reject_concrete_store_alias(concrete_before);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast ItemExternCrate) {
        if self.error.is_none() && item.ident == "self" && item.rename.is_some() {
            self.error = Some(anyhow::anyhow!(
                "production crate-root extern aliases cannot be classified safely for dependency boundaries"
            ));
            return;
        }
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        self.record_concrete_store(ident);
    }

    fn visit_token_stream(&mut self, tokens: &'ast TokenStream) {
        self.record_concrete_stores_in_tokens(tokens);
    }

    fn visit_path(&mut self, path: &'ast SynPath) {
        if self.error.is_none() && (path.leading_colon.is_none() || self.rust_2015_absolute_paths) {
            let mut segments = path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
            if path.leading_colon.is_some() {
                segments.insert(0, "crate".to_owned());
            }
            let is_qualified = segments.len() > 1 || matches!(segments.first().map(String::as_str), Some("crate" | "self" | "super"));
            if is_qualified && let Err(error) = self.collect_segments(&segments, false, false) {
                self.error = Some(error);
                return;
            }
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if self.error.is_some() {
            return;
        }
        let stringifies = is_explicit_builtin_stringify(node) || self.is_imported_builtin_stringify(node);
        if !stringifies && tokens_may_hide_concrete_store(&node.tokens) {
            self.error = Some(anyhow::anyhow!(
                "production concrete stores cannot be hidden behind macro-generated aliases or renamed imports"
            ));
            return;
        }
        let previous = self.enter_site_context("macro-invocation", node);
        if self.collect_internal_imports && !stringifies {
            match restricted_token_identifier(&node.tokens, &self.module, self.rust_2015_absolute_paths, StringScan::RustFragment) {
                Ok(Some(restricted)) => {
                    self.error = Some(anyhow::anyhow!(
                        "production macro token stream names restricted crate module {restricted:?} and cannot be classified safely"
                    ));
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.error = Some(error);
                    self.leave_site_context(previous);
                    return;
                }
            }
        }
        if self.require_reviewed_expansions && !stringifies && !reviewed_macro_expansion(node) {
            self.error = Some(anyhow::anyhow!("production code invokes unreviewed macro expansion path {}", node.path.to_token_stream()));
            self.leave_site_context(previous);
            return;
        }
        self.visit_path(&node.path);
        if !stringifies {
            self.visit_token_stream(&node.tokens);
        }
        self.leave_site_context(previous);
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if self.error.is_some() {
            return;
        }
        if self.collect_internal_imports {
            match restricted_attribute_identifier(attribute, &self.module, self.rust_2015_absolute_paths, &self.cfg_context) {
                Ok(Some(restricted)) => {
                    self.error = Some(anyhow::anyhow!(
                        "production attribute token stream names restricted crate module {restricted:?} and cannot be classified safely"
                    ));
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        if self.require_reviewed_expansions && !reviewed_attribute_expansion(attribute) {
            self.error = Some(anyhow::anyhow!("production code uses unreviewed attribute expansion {}", attribute.meta.to_token_stream()));
            return;
        }
        let previous = self.enter_site_context("attribute", attribute);
        let site_context = self.site_context.as_deref().expect("attribute site context");
        if let Err(error) = self.concrete_stores.record_attribute(attribute, site_context, &self.cfg_context) {
            self.error = Some(error);
            self.leave_site_context(previous);
            return;
        }
        if self.generic_default_depth > 0
            && let Err(error) = self.concrete_stores.record_generic_default_attribute(attribute, site_context, &self.cfg_context)
        {
            self.error = Some(error);
            self.leave_site_context(previous);
            return;
        }
        self.leave_site_context(previous);
    }

    fn visit_item_macro(&mut self, item: &'ast ItemMacro) {
        let Some(name) = &item.ident else {
            visit::visit_item_macro(self, item);
            return;
        };
        for attribute in &item.attrs {
            self.visit_attribute(attribute);
        }
        if self.error.is_some() {
            return;
        }
        if contains_production_concrete_store(&item.mac.tokens, &self.cfg_context) {
            self.error = Some(anyhow::anyhow!("production macro definitions cannot inject concrete stores into call sites"));
            return;
        }
        if self.collect_internal_imports {
            match restricted_token_identifier(&item.mac.tokens, &self.module, self.rust_2015_absolute_paths, StringScan::RustFragment) {
                Ok(Some(restricted)) => {
                    self.error = Some(anyhow::anyhow!(
                        "production macro token stream names restricted crate module {restricted:?} and cannot be classified safely"
                    ));
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        if let Some(scope) = self.macro_shadow_scopes.last_mut() {
            scope.insert(MacroShadow {
                name: normalized_ident(name),
                cfg: self.cfg_context.clone(),
            });
        }
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        for attribute in &item.attrs {
            self.visit_attribute(attribute);
        }
        if self.error.is_some() {
            return;
        }
        self.visit_ident(&item.ident);
        let Some((_, items)) = &item.content else {
            return;
        };
        self.module.push(normalized_ident(&item.ident));
        self.macro_shadow_scopes.push(BTreeSet::new());
        for nested in items {
            self.visit_item(nested);
        }
        self.macro_shadow_scopes.pop();
        self.module.pop();
    }
}

impl ProductionSyntaxCollector {
    fn is_imported_builtin_stringify(&self, node: &syn::Macro) -> bool {
        if node.path.leading_colon.is_some() || node.path.segments.len() != 1 {
            return false;
        }
        let alias = normalized_ident(&node.path.segments[0].ident);
        let block_binding = self
            .builtin_stringify_block_aliases
            .iter()
            .zip(&self.macro_import_shadow_scopes)
            .rev()
            .find_map(|(builtin_scope, shadow_scope)| {
                let builtin = builtin_scope
                    .iter()
                    .any(|candidate| candidate.name == alias && candidate.cfg.conjoin(&self.cfg_context).is_some());
                let shadowed = shadow_scope.iter().any(|shadow| shadow.name == alias && shadow.cfg.conjoin(&self.cfg_context).is_some());
                (builtin || shadowed).then_some(builtin && !shadowed)
            });
        let imported = block_binding.unwrap_or_else(|| {
            self.builtin_stringify_aliases
                .iter()
                .any(|candidate| candidate.module == self.module && candidate.name == alias && candidate.cfg.conjoin(&self.cfg_context).is_some())
        });
        imported
            && !self
                .macro_shadow_scopes
                .iter()
                .rev()
                .any(|scope| scope.iter().any(|shadow| shadow.name == alias && shadow.cfg.conjoin(&self.cfg_context).is_some()))
    }
}

fn stringify_imports_in_block(block: &Block, inherited_cfg: &ProductionCfgContext) -> Result<BlockStringifyImports> {
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
            let builtin = item.leading_colon.is_some()
                && matches!(
                    path.segments.as_slice(),
                    [root, imported] if matches!(root.as_str(), "core" | "std") && imported == "stringify"
                );
            if builtin {
                aliases.insert(BlockBuiltinStringifyAlias { name, cfg: cfg.clone() });
            } else {
                shadows.insert(MacroShadow { name, cfg: cfg.clone() });
            }
        }
    }
    Ok(BlockStringifyImports { aliases, shadows })
}

fn collect_builtin_stringify_aliases(file: &File, module: &[String], inherited_cfg: &ProductionCfgContext) -> Result<BuiltinStringifyAliases> {
    let mut aliases = BTreeSet::new();
    collect_builtin_stringify_aliases_in_items(&file.items, module, inherited_cfg, &mut aliases)?;
    Ok(aliases)
}

fn collect_builtin_stringify_aliases_in_items(items: &[Item], module: &[String], inherited_cfg: &ProductionCfgContext, aliases: &mut BuiltinStringifyAliases) -> Result<()> {
    for item in items {
        let Some(cfg) = production_cfg_context(item_attributes(item)?, inherited_cfg)? else {
            continue;
        };
        match item {
            Item::Use(item) => collect_builtin_stringify_aliases_from_use(item, module, &cfg, aliases),
            Item::Mod(item) => {
                if let Some((_, nested)) = &item.content {
                    let mut nested_module = module.to_vec();
                    nested_module.push(normalized_ident(&item.ident));
                    collect_builtin_stringify_aliases_in_items(nested, &nested_module, &cfg, aliases)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn collect_builtin_stringify_aliases_from_use(item: &ItemUse, module: &[String], cfg: &ProductionCfgContext, aliases: &mut BuiltinStringifyAliases) {
    let mut names = BTreeSet::new();
    collect_builtin_stringify_alias_names_from_use(item, &mut names);
    aliases.extend(names.into_iter().map(|name| BuiltinStringifyAlias {
        module: module.to_vec(),
        name,
        cfg: cfg.clone(),
    }));
}

fn collect_builtin_stringify_alias_names_from_use(item: &ItemUse, aliases: &mut BTreeSet<String>) {
    if item.leading_colon.is_none() {
        return;
    }
    let mut paths = Vec::new();
    flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
    for path in paths
        .into_iter()
        .filter(|path| path.segments == ["core", "stringify"] || path.segments == ["std", "stringify"])
    {
        aliases.insert(path.alias.unwrap_or_else(|| "stringify".to_owned()));
    }
}

fn declaration_ancestor(item: &Item) -> Option<String> {
    match item {
        Item::Const(item) => Some(named_ancestor("const", &item.ident, &item.vis)),
        Item::Enum(item) => Some(named_ancestor("enum", &item.ident, &item.vis)),
        Item::Fn(item) => Some(named_ancestor("fn", &item.sig.ident, &item.vis)),
        Item::Impl(item) => {
            let mut header = item.clone();
            strip_impl_documentation(&mut header);
            header.items.clear();
            Some(format!("impl:{}", syntax_fingerprint(&header)))
        }
        Item::Mod(item) => Some(named_ancestor("mod", &item.ident, &item.vis)),
        Item::Static(item) => Some(named_ancestor("static", &item.ident, &item.vis)),
        Item::Struct(item) => Some(named_ancestor("struct", &item.ident, &item.vis)),
        Item::Trait(item) => Some(named_ancestor("trait", &item.ident, &item.vis)),
        Item::Union(item) => Some(named_ancestor("union", &item.ident, &item.vis)),
        _ => None,
    }
}

fn named_ancestor(kind: &str, ident: &proc_macro2::Ident, visibility: &Visibility) -> String {
    format!("{kind}:{}:{}", normalized_ident(ident), syntax_fingerprint(visibility))
}

fn visibility_is_exposed(visibility: &Visibility) -> bool {
    match visibility {
        Visibility::Inherited => false,
        Visibility::Restricted(restricted) => !restricted.path.is_ident("self"),
        Visibility::Public(_) => true,
    }
}

fn is_explicit_builtin_stringify(node: &syn::Macro) -> bool {
    node.path.leading_colon.is_some()
        && node.path.segments.len() == 2
        && matches!(normalized_ident(&node.path.segments[0].ident).as_str(), "core" | "std")
        && normalized_ident(&node.path.segments[1].ident) == "stringify"
}

fn strip_impl_documentation(item: &mut ItemImpl) {
    item.attrs.retain(|attribute| !attribute.path().is_ident("doc"));
    for member in &mut item.items {
        let attributes = match member {
            ImplItem::Const(item) => &mut item.attrs,
            ImplItem::Fn(item) => &mut item.attrs,
            ImplItem::Macro(item) => &mut item.attrs,
            ImplItem::Type(item) => &mut item.attrs,
            _ => continue,
        };
        attributes.retain(|attribute| !attribute.path().is_ident("doc"));
    }
}

fn tokens_may_hide_concrete_store(tokens: &TokenStream) -> bool {
    let mut identifiers = Vec::new();
    collect_token_identifiers(tokens, &mut identifiers);
    identifiers.iter().any(|identifier| is_concrete_store_name(identifier))
        && (identifiers.iter().any(|identifier| identifier == "type")
            || identifiers.iter().any(|identifier| identifier == "use") && identifiers.iter().any(|identifier| identifier == "as"))
}

fn collect_token_identifiers(tokens: &TokenStream, identifiers: &mut Vec<String>) {
    for token in resolving_tokens(tokens) {
        match token {
            TokenTree::Group(group) => collect_token_identifiers(&group.stream(), identifiers),
            TokenTree::Ident(ident) => identifiers.push(normalized_ident(&ident)),
            TokenTree::Literal(_) | TokenTree::Punct(_) => {}
        }
    }
}

#[cfg(test)]
mod tests;
