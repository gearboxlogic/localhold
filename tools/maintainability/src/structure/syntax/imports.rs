use anyhow::{Result, bail};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use syn::visit::{self, Visit};
use syn::{
    Arm, Attribute, BareFnArg, BareVariadic, Expr, Field, FieldPat, FieldValue, File, FnArg, ForeignItem, GenericParam, ImplItem, ImplItemType, Item, ItemExternCrate, ItemMacro,
    ItemMod, ItemStruct, ItemType, ItemUse, Local, Pat, Path as SynPath, Stmt, StmtMacro, TraitItem, TraitItemType, Variadic, Variant, Visibility,
};

use crate::scan::{reviewed_attribute_expansion, reviewed_macro_expansion, syntax_fingerprint};

use super::{
    ProductionCfgContext, expr_attributes, fn_arg_attributes, foreign_item_attributes, generic_param_attributes, impl_item_attributes, item_attributes, normalized_ident,
    pat_attributes, production_cfg_context, trait_item_attributes,
};

mod concrete;
mod resolution;
pub use concrete::{ConcreteStoreCounts, ConcreteStoreSites};
use concrete::{ConcreteStoreInventory, context_fingerprint, is_concrete_store_name, tokens_contain_concrete_store};
use resolution::{StringScan, UsePath, flatten_use_tree, resolve_path, restricted_attribute_identifier, restricted_token_identifier, source_module};

#[derive(Default)]
pub struct ProductionSyntaxFacts {
    pub internal_imports: Vec<String>,
    pub concrete_stores: ConcreteStoreCounts,
    pub public_concrete_store_structs: ConcreteStoreSites,
    pub concrete_store_sites: ConcreteStoreSites,
    pub generic_default_concrete_store_sites: ConcreteStoreSites,
}

#[derive(Clone, Copy)]
pub struct ProductionSyntaxOptions {
    pub collect_internal_imports: bool,
    pub rust_2015_absolute_paths: bool,
    pub require_reviewed_expansions: bool,
}

pub fn production_syntax_facts(file: &syn::File, source_path: &str, crate_root: Option<&str>, options: ProductionSyntaxOptions) -> Result<ProductionSyntaxFacts> {
    let module = if options.collect_internal_imports {
        source_module(source_path, crate_root)?
    } else {
        Vec::new()
    };
    let mut collector = ProductionSyntaxCollector {
        module,
        imports: Vec::new(),
        concrete_stores: ConcreteStoreInventory::default(),
        site_context: None,
        generic_default_depth: 0,
        declaration_ancestors: Vec::new(),
        cfg_context: ProductionCfgContext::default(),
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
    collector.concrete_stores.finish();
    Ok(ProductionSyntaxFacts {
        internal_imports: collector.imports,
        concrete_stores: collector.concrete_stores.counts,
        public_concrete_store_structs: collector.concrete_stores.public_struct_declarations,
        concrete_store_sites: collector.concrete_stores.sites,
        generic_default_concrete_store_sites: collector.concrete_stores.generic_default_sites,
    })
}

struct ProductionSyntaxCollector {
    module: Vec<String>,
    imports: Vec<String>,
    concrete_stores: ConcreteStoreInventory,
    site_context: Option<String>,
    generic_default_depth: usize,
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
    visit_production_node!(visit_variant, visit_variant, Variant, node => Ok(node.attrs.as_slice()));
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
        visit::visit_field(self, node);
        self.leave_site_context(previous);
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
        if self.error.is_none()
            && let Err(error) = self.collect_use(item)
        {
            self.error = Some(error);
            self.leave_site_context(previous);
            return;
        }
        visit::visit_item_use(self, item);
        self.leave_site_context(previous);
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        if matches!(item.vis, Visibility::Public(_)) {
            self.concrete_stores
                .record_public_struct_declaration(item, &self.cfg_context.identity(), &self.declaration_ancestors);
        }
        visit::visit_item_struct(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast ItemType) {
        let import_count = self.imports.len();
        let concrete_before = self.concrete_stores.counts;
        visit::visit_item_type(self, item);
        if self.error.is_none() && self.imports.len() != import_count && !matches!(item.vis, Visibility::Inherited) {
            self.error = Some(anyhow::anyhow!("production restricted imports cannot be exposed through public type aliases"));
        }
        self.reject_concrete_store_alias(concrete_before);
    }

    fn visit_impl_item_type(&mut self, item: &'ast ImplItemType) {
        let concrete_before = self.concrete_stores.counts;
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
        if tokens_may_hide_concrete_store(&node.tokens) {
            self.error = Some(anyhow::anyhow!(
                "production concrete stores cannot be hidden behind macro-generated aliases or renamed imports"
            ));
            return;
        }
        let previous = self.enter_site_context("macro-invocation", node);
        if self.collect_internal_imports {
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
        if self.require_reviewed_expansions && !reviewed_macro_expansion(node) {
            self.error = Some(anyhow::anyhow!("production code invokes unreviewed macro expansion path {}", node.path.to_token_stream()));
            self.leave_site_context(previous);
            return;
        }
        self.visit_path(&node.path);
        self.visit_token_stream(&node.tokens);
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
        if item.ident.is_some() && tokens_contain_concrete_store(&item.mac.tokens) {
            self.error = Some(anyhow::anyhow!("production macro definitions cannot inject concrete stores into call sites"));
            return;
        }
        visit::visit_item_macro(self, item);
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
        for nested in items {
            self.visit_item(nested);
        }
        self.module.pop();
    }
}

fn declaration_ancestor(item: &Item) -> Option<String> {
    match item {
        Item::Const(item) => Some(named_ancestor("const", &item.ident, &item.vis)),
        Item::Fn(item) => Some(named_ancestor("fn", &item.sig.ident, &item.vis)),
        Item::Impl(item) => Some(format!("impl:{}", syntax_fingerprint(&item.self_ty))),
        Item::Mod(item) => Some(named_ancestor("mod", &item.ident, &item.vis)),
        Item::Static(item) => Some(named_ancestor("static", &item.ident, &item.vis)),
        Item::Trait(item) => Some(named_ancestor("trait", &item.ident, &item.vis)),
        _ => None,
    }
}

fn named_ancestor(kind: &str, ident: &proc_macro2::Ident, visibility: &Visibility) -> String {
    format!("{kind}:{}:{}", normalized_ident(ident), syntax_fingerprint(visibility))
}

fn tokens_may_hide_concrete_store(tokens: &TokenStream) -> bool {
    let mut identifiers = Vec::new();
    collect_token_identifiers(tokens, &mut identifiers);
    identifiers.iter().any(|identifier| is_concrete_store_name(identifier))
        && (identifiers.iter().any(|identifier| identifier == "type")
            || identifiers.iter().any(|identifier| identifier == "use") && identifiers.iter().any(|identifier| identifier == "as"))
}

fn collect_token_identifiers(tokens: &TokenStream, identifiers: &mut Vec<String>) {
    for token in tokens.clone() {
        match token {
            TokenTree::Group(group) => collect_token_identifiers(&group.stream(), identifiers),
            TokenTree::Ident(ident) => identifiers.push(normalized_ident(&ident)),
            TokenTree::Literal(_) | TokenTree::Punct(_) => {}
        }
    }
}

#[cfg(test)]
mod tests;
