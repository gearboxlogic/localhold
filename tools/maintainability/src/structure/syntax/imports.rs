use anyhow::{Context, Result, bail};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens as _;
use serde::Serialize;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Arm, Attribute, BareFnArg, BareVariadic, Expr, Field, FieldPat, FieldValue, File, FnArg, ForeignItem, GenericParam, ImplItem, Item, ItemExternCrate, ItemMod, ItemType,
    ItemUse, Local, Meta, Pat, Path as SynPath, StmtMacro, Token, TraitItem, Variadic, Variant, Visibility,
};

use crate::scan::{reviewed_attribute_expansion, reviewed_macro_expansion};

use super::{
    attributes_disable_production, cfg_can_apply_in_production, expr_attributes, fn_arg_attributes, foreign_item_attributes, generic_param_attributes, impl_item_attributes,
    item_is_test_only, normalized_ident, pat_attributes, trait_item_attributes,
};

mod resolution;
use resolution::{StringScan, UsePath, flatten_use_tree, resolve_path, restricted_attribute_identifier, restricted_token_identifier, source_module};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ConcreteStoreCounts {
    pub sqlite_store: usize,
    pub postgres_store: usize,
}

#[derive(Default)]
pub struct ProductionSyntaxFacts {
    pub internal_imports: Vec<String>,
    pub concrete_stores: ConcreteStoreCounts,
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
        concrete_stores: ConcreteStoreCounts::default(),
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
    Ok(ProductionSyntaxFacts {
        internal_imports: collector.imports,
        concrete_stores: collector.concrete_stores,
    })
}

struct ProductionSyntaxCollector {
    module: Vec<String>,
    imports: Vec<String>,
    concrete_stores: ConcreteStoreCounts,
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

    fn skip_test_only(&mut self, test_only: Result<bool>) -> bool {
        if self.error.is_some() {
            return true;
        }
        match test_only {
            Ok(test_only) => test_only,
            Err(error) => {
                self.error = Some(error);
                true
            }
        }
    }

    fn record_concrete_store(&mut self, ident: &proc_macro2::Ident) {
        let count = match normalized_ident(ident).as_str() {
            "SqliteStore" => &mut self.concrete_stores.sqlite_store,
            "PostgresStore" => &mut self.concrete_stores.postgres_store,
            _ => return,
        };
        let Some(next) = count.checked_add(1) else {
            self.error = Some(anyhow::anyhow!("production concrete-store name count overflow"));
            return;
        };
        *count = next;
    }

    fn record_concrete_stores_in_tokens(&mut self, tokens: &TokenStream) {
        for token in tokens.clone() {
            match token {
                TokenTree::Group(group) => self.record_concrete_stores_in_tokens(&group.stream()),
                TokenTree::Ident(ident) => self.record_concrete_store(&ident),
                TokenTree::Punct(_) | TokenTree::Literal(_) => {}
            }
        }
    }

    fn record_concrete_stores_in_attribute(&mut self, attribute: &Attribute) -> Result<()> {
        if !attribute.path().is_ident("cfg_attr") {
            self.record_concrete_stores_in_meta(&attribute.meta);
            return Ok(());
        }
        let Meta::List(list) = &attribute.meta else {
            return Ok(());
        };
        self.record_concrete_stores_in_cfg_attr(&list.tokens)
    }

    fn record_concrete_stores_in_cfg_attr(&mut self, tokens: &TokenStream) -> Result<()> {
        let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
            .parse2(tokens.clone())
            .context("parse cfg_attr arguments for production concrete-store classification")?;
        let mut arguments = arguments.into_iter();
        let Some(condition) = arguments.next() else {
            return Ok(());
        };
        if !cfg_can_apply_in_production(&condition) {
            return Ok(());
        }
        for nested in arguments {
            self.record_concrete_stores_in_nested_meta(&nested)?;
        }
        Ok(())
    }

    fn record_concrete_stores_in_nested_meta(&mut self, nested: &Meta) -> Result<()> {
        if !nested.path().is_ident("cfg_attr") {
            self.record_concrete_stores_in_meta(nested);
            return Ok(());
        }
        let Meta::List(list) = nested else {
            return Ok(());
        };
        self.record_concrete_stores_in_cfg_attr(&list.tokens)
    }

    fn record_concrete_stores_in_meta(&mut self, meta: &Meta) {
        let tokens = match meta {
            Meta::Path(_) => return,
            Meta::List(list) => list.tokens.clone(),
            Meta::NameValue(value) => value.value.to_token_stream(),
        };
        self.record_concrete_stores_in_attribute_tokens(&tokens);
    }

    fn record_concrete_stores_in_attribute_tokens(&mut self, tokens: &TokenStream) {
        for token in tokens.clone() {
            match token {
                TokenTree::Group(group) => self.record_concrete_stores_in_attribute_tokens(&group.stream()),
                TokenTree::Literal(literal) => self.record_concrete_stores_in_path_literal(&literal),
                TokenTree::Ident(_) | TokenTree::Punct(_) => {}
            }
        }
    }

    fn record_concrete_stores_in_path_literal(&mut self, literal: &proc_macro2::Literal) {
        let Ok(syn::Lit::Str(literal)) = syn::parse_str::<syn::Lit>(&literal.to_string()) else {
            return;
        };
        let value = literal.value();
        if !value.contains("::") {
            return;
        }
        let Ok(path) = syn::parse_str::<SynPath>(&value) else {
            return;
        };
        for segment in path.segments {
            self.record_concrete_store(&segment.ident);
        }
    }
}

macro_rules! visit_production_node {
    ($method:ident, $walk:ident, $node:ty, $binding:ident => $test_only:expr) => {
        fn $method(&mut self, $binding: &'ast $node) {
            let test_only: Result<bool> = $test_only;
            if !self.skip_test_only(test_only) {
                visit::$walk(self, $binding);
            }
        }
    };
}

impl<'ast> Visit<'ast> for ProductionSyntaxCollector {
    visit_production_node!(visit_file, visit_file, File, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_item, visit_item, Item, node => item_is_test_only(node));
    visit_production_node!(
        visit_impl_item,
        visit_impl_item,
        ImplItem,
        node =>
        impl_item_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(
        visit_trait_item,
        visit_trait_item,
        TraitItem,
        node =>
        trait_item_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(
        visit_foreign_item,
        visit_foreign_item,
        ForeignItem,
        node =>
        foreign_item_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(visit_variant, visit_variant, Variant, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_field, visit_field, Field, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_arm, visit_arm, Arm, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_local, visit_local, Local, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_stmt_macro, visit_stmt_macro, StmtMacro, node => attributes_disable_production(&node.attrs));
    visit_production_node!(
        visit_expr,
        visit_expr,
        Expr,
        node => expr_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(
        visit_fn_arg,
        visit_fn_arg,
        FnArg,
        node => attributes_disable_production(fn_arg_attributes(node))
    );
    visit_production_node!(
        visit_generic_param,
        visit_generic_param,
        GenericParam,
        node => attributes_disable_production(generic_param_attributes(node))
    );
    visit_production_node!(
        visit_pat,
        visit_pat,
        Pat,
        node => pat_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(
        visit_bare_fn_arg,
        visit_bare_fn_arg,
        BareFnArg,
        node => attributes_disable_production(&node.attrs)
    );
    visit_production_node!(
        visit_bare_variadic,
        visit_bare_variadic,
        BareVariadic,
        node => attributes_disable_production(&node.attrs)
    );
    visit_production_node!(
        visit_variadic,
        visit_variadic,
        Variadic,
        node => attributes_disable_production(&node.attrs)
    );
    visit_production_node!(
        visit_field_pat,
        visit_field_pat,
        FieldPat,
        node => attributes_disable_production(&node.attrs)
    );
    visit_production_node!(
        visit_field_value,
        visit_field_value,
        FieldValue,
        node => attributes_disable_production(&node.attrs)
    );

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        if self.error.is_none()
            && let Err(error) = self.collect_use(item)
        {
            self.error = Some(error);
            return;
        }
        visit::visit_item_use(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast ItemType) {
        let import_count = self.imports.len();
        visit::visit_item_type(self, item);
        if self.error.is_none() && self.imports.len() != import_count && !matches!(item.vis, Visibility::Inherited) {
            self.error = Some(anyhow::anyhow!("production restricted imports cannot be exposed through public type aliases"));
        }
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
                    return;
                }
            }
        }
        if self.require_reviewed_expansions && !reviewed_macro_expansion(node) {
            self.error = Some(anyhow::anyhow!("production code invokes unreviewed macro expansion path {}", node.path.to_token_stream()));
            return;
        }
        self.visit_path(&node.path);
        self.visit_token_stream(&node.tokens);
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if self.error.is_some() {
            return;
        }
        if self.collect_internal_imports {
            match restricted_attribute_identifier(attribute, &self.module, self.rust_2015_absolute_paths) {
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
        if let Err(error) = self.record_concrete_stores_in_attribute(attribute) {
            self.error = Some(error);
            return;
        }
        visit::visit_attribute(self, attribute);
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

#[cfg(test)]
mod tests;
