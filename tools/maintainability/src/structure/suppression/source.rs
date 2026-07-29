use std::collections::BTreeMap;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use syn::ext::IdentExt as _;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Arm, Attribute, Expr, ForeignItem, GenericParam, ImplItem, Item, Local, Meta, StmtMacro, Token, TraitItem, Variant};

use self::nodes::{SuppressionScope, foreign_item_scope, impl_item_scope, item_scope, trait_item_scope};
use super::{SourceCategory, SourceSuppression};
use crate::scan::syntax_fingerprint;
use crate::structure::syntax::{
    ProductionCfgContext, cfg_attr_metas_with_production_reachability, expr_attributes, foreign_item_attributes, generic_param_attributes, impl_item_attributes, item_attributes,
    normalized_ident, pat_attributes, production_cfg_context, trait_item_attributes,
};

mod nodes;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
struct PendingSuppression {
    item: String,
    scope: String,
    signature: Option<String>,
    target: Option<String>,
    category: SourceCategory,
    level: String,
    lint: String,
    reason: String,
    macro_carried: bool,
    fingerprint: String,
}

pub(super) struct SourceScanner {
    category: SourceCategory,
    cfg_context: ProductionCfgContext,
    item: String,
    scope: String,
    signature: Option<String>,
    target: Option<String>,
    pending: Vec<PendingSuppression>,
    error: Option<anyhow::Error>,
}

impl SourceScanner {
    pub(super) fn scan(path: &str, component: &str, category: SourceCategory, syntax: &syn::File) -> Result<Vec<SourceSuppression>> {
        let mut scanner = Self {
            category,
            cfg_context: ProductionCfgContext::default(),
            item: "<module>".to_owned(),
            scope: "module".to_owned(),
            signature: None,
            target: None,
            pending: Vec::new(),
            error: None,
        };
        scanner.visit_file(syntax);
        if let Some(error) = scanner.error {
            return Err(error).with_context(|| format!("scan lint suppressions in {path}"));
        }
        let mut occurrences = BTreeMap::new();
        let mut sites = Vec::with_capacity(scanner.pending.len());
        for pending in scanner.pending {
            let key = (
                pending.item.clone(),
                pending.scope.clone(),
                pending.signature.clone(),
                pending.target.clone(),
                pending.category,
                pending.level.clone(),
                pending.lint.clone(),
                pending.reason.clone(),
                pending.macro_carried,
                pending.fingerprint.clone(),
            );
            let occurrence = occurrences.entry(key).or_insert(0_usize);
            let mut site = SourceSuppression {
                id: String::new(),
                path: path.to_owned(),
                component: component.to_owned(),
                item: pending.item,
                scope: pending.scope,
                signature: pending.signature,
                target: pending.target,
                category: pending.category,
                level: pending.level,
                lint: pending.lint,
                reason: pending.reason,
                macro_carried: pending.macro_carried,
                occurrence: *occurrence,
                fingerprint: pending.fingerprint,
            };
            site.id = site.stable_id();
            sites.push(site);
            *occurrence = occurrence.checked_add(1).context("lint suppression occurrence overflow")?;
        }
        Ok(sites)
    }

    fn visit_classified<'ast, T: ToTokens>(&mut self, attributes: Result<&[Attribute]>, item: Option<SuppressionScope>, node: &'ast T, visit: impl FnOnce(&mut Self, &'ast T)) {
        if self.error.is_some() {
            return;
        }
        let previous_category = self.category;
        let previous_cfg_context = self.cfg_context.clone();
        let previous_item = self.item.clone();
        let previous_scope = self.scope.clone();
        let previous_signature = self.signature.clone();
        let previous_target = self.target.clone();
        let attributes = match attributes {
            Ok(attributes) => attributes,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        if self.category == SourceCategory::Production {
            match production_cfg_context(attributes, &self.cfg_context) {
                Ok(Some(context)) => self.cfg_context = context,
                Ok(None) => self.category = SourceCategory::Test,
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        if let Some(item) = item {
            self.item = if previous_item == "<module>" {
                item.item
            } else {
                format!("{previous_item}::{}", item.item)
            };
            self.scope = item.scope;
            self.signature = item.signature;
            self.target = Some(syntax_fingerprint(node));
        }
        visit(self, node);
        self.category = previous_category;
        self.cfg_context = previous_cfg_context;
        self.item = previous_item;
        self.scope = previous_scope;
        self.signature = previous_signature;
        self.target = previous_target;
    }

    fn visit_anonymous_scope<'ast, T: ToTokens>(&mut self, attributes: Result<&[Attribute]>, scope: &str, node: &'ast T, visit: impl FnOnce(&mut Self, &'ast T)) {
        let previous_scope = std::mem::replace(&mut self.scope, scope.to_owned());
        let previous_target = self.target.replace(syntax_fingerprint(node));
        self.visit_classified(attributes, None, node, visit);
        self.scope = previous_scope;
        self.target = previous_target;
    }

    fn record_attribute(&mut self, attribute: &Attribute, macro_carried: bool) -> Result<()> {
        self.record_meta(&attribute.meta, self.category, macro_carried, &syntax_fingerprint(attribute))
    }

    fn record_meta(&mut self, meta: &Meta, category: SourceCategory, macro_carried: bool, fingerprint: &str) -> Result<()> {
        if meta.path().is_ident("cfg_attr") {
            let Meta::List(list) = meta else {
                return Ok(());
            };
            return self.record_cfg_attr(list, category, macro_carried, fingerprint);
        }
        let level = if meta.path().is_ident("expect") {
            "expect"
        } else if meta.path().is_ident("allow") {
            "allow"
        } else {
            return Ok(());
        };
        let Meta::List(suppression) = meta else {
            bail!("{level} lint suppression must use list syntax");
        };
        let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
            .parse2(suppression.tokens.clone())
            .with_context(|| format!("parse {level} lint suppression"))?;
        let reason = arguments
            .iter()
            .find_map(|argument| {
                let Meta::NameValue(value) = argument else {
                    return None;
                };
                if !value.path.is_ident("reason") {
                    return None;
                }
                let syn::Expr::Lit(expression) = &value.value else {
                    return None;
                };
                let syn::Lit::Str(reason) = &expression.lit else {
                    return None;
                };
                Some(reason.value())
            })
            .unwrap_or_default();
        for argument in arguments {
            let Meta::Path(path) = argument else {
                continue;
            };
            let lint = path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>().join("::");
            self.pending.push(PendingSuppression {
                item: self.item.clone(),
                scope: self.scope.clone(),
                signature: self.signature.clone(),
                target: self.target.clone(),
                category,
                level: level.to_owned(),
                lint,
                reason: reason.clone(),
                macro_carried,
                fingerprint: fingerprint.to_owned(),
            });
        }
        Ok(())
    }

    fn record_cfg_attr(&mut self, cfg_attr: &syn::MetaList, category: SourceCategory, macro_carried: bool, fingerprint: &str) -> Result<()> {
        let metas = cfg_attr_metas_with_production_reachability(&cfg_attr.tokens, &self.cfg_context)?;
        for classified in metas {
            let category = category_for_branch(category, classified.production_reachable);
            self.record_meta(&classified.meta, category, macro_carried, fingerprint)?;
        }
        Ok(())
    }

    fn record_macro_tokens(&mut self, tokens: &TokenStream) -> Result<()> {
        let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
        let mut index = 0_usize;
        while index < tokens.len() {
            if is_include_invocation(&tokens, index) {
                bail!("Rust source include! cannot be classified safely");
            }
            if let Some((attribute_index, group)) = macro_attribute_group(&tokens, index) {
                self.record_macro_attribute(group)?;
                index = attribute_index.saturating_add(1);
                continue;
            }
            if let Some(TokenTree::Group(group)) = tokens.get(index) {
                self.record_macro_tokens(&group.stream())?;
            }
            index = index.saturating_add(1);
        }
        Ok(())
    }

    fn record_macro_attribute(&mut self, group: &proc_macro2::Group) -> Result<()> {
        match syn::parse2::<Meta>(group.stream()) {
            Ok(meta) => {
                validate_audited_module_path_meta(&meta).context("validate macro-carried module path")?;
                let fingerprint = syntax_fingerprint(&meta);
                self.record_meta(&meta, self.category, true, &fingerprint).context("classify macro-carried lint attribute")
            }
            Err(error) => Err(error).context("opaque macro-carried attribute could hide a lint suppression"),
        }
    }
}

fn is_include_invocation(tokens: &[TokenTree], index: usize) -> bool {
    matches!(tokens.get(index), Some(TokenTree::Ident(ident)) if ident.unraw() == "include")
        && matches!(tokens.get(index.saturating_add(1)), Some(TokenTree::Punct(punct)) if punct.as_char() == '!')
}

fn macro_attribute_group(tokens: &[TokenTree], index: usize) -> Option<(usize, &proc_macro2::Group)> {
    if !matches!(tokens.get(index), Some(TokenTree::Punct(punct)) if punct.as_char() == '#') {
        return None;
    }
    let following_index = index.saturating_add(1);
    let attribute_index = if matches!(tokens.get(following_index), Some(TokenTree::Punct(punct)) if punct.as_char() == '!') {
        following_index.saturating_add(1)
    } else {
        following_index
    };
    match tokens.get(attribute_index) {
        Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Bracket => Some((attribute_index, group)),
        _ => None,
    }
}

fn category_for_branch(category: SourceCategory, production_reachable: bool) -> SourceCategory {
    if category == SourceCategory::Production && !production_reachable {
        SourceCategory::Test
    } else {
        category
    }
}

impl<'ast> Visit<'ast> for SourceScanner {
    fn visit_file(&mut self, node: &'ast syn::File) {
        self.visit_anonymous_scope(Ok(&node.attrs), "module", node, visit::visit_file);
    }

    fn visit_item(&mut self, node: &'ast Item) {
        let attributes = item_attributes(node);
        if let Some(scope) = item_scope(node) {
            self.visit_classified(attributes, Some(scope), node, visit::visit_item);
        } else {
            self.visit_anonymous_scope(attributes, "item", node, visit::visit_item);
        }
    }

    fn visit_impl_item(&mut self, node: &'ast ImplItem) {
        let attributes = impl_item_attributes(node);
        if let Some(scope) = impl_item_scope(node) {
            self.visit_classified(attributes, Some(scope), node, visit::visit_impl_item);
        } else {
            self.visit_anonymous_scope(attributes, "impl-item", node, visit::visit_impl_item);
        }
    }

    fn visit_trait_item(&mut self, node: &'ast TraitItem) {
        let attributes = trait_item_attributes(node);
        if let Some(scope) = trait_item_scope(node) {
            self.visit_classified(attributes, Some(scope), node, visit::visit_trait_item);
        } else {
            self.visit_anonymous_scope(attributes, "trait-item", node, visit::visit_trait_item);
        }
    }

    fn visit_foreign_item(&mut self, node: &'ast ForeignItem) {
        let attributes = foreign_item_attributes(node);
        if let Some(scope) = foreign_item_scope(node) {
            self.visit_classified(attributes, Some(scope), node, visit::visit_foreign_item);
        } else {
            self.visit_anonymous_scope(attributes, "foreign-item", node, visit::visit_foreign_item);
        }
    }

    fn visit_variant(&mut self, node: &'ast Variant) {
        self.visit_classified(
            Ok(&node.attrs),
            Some(SuppressionScope {
                item: normalized_ident(&node.ident),
                scope: "variant".to_owned(),
                signature: None,
            }),
            node,
            visit::visit_variant,
        );
    }

    fn visit_field(&mut self, node: &'ast syn::Field) {
        self.visit_anonymous_scope(Ok(&node.attrs), "field", node, visit::visit_field);
    }

    fn visit_generic_param(&mut self, node: &'ast GenericParam) {
        self.visit_anonymous_scope(Ok(generic_param_attributes(node)), "generic-parameter", node, visit::visit_generic_param);
    }

    fn visit_fn_arg(&mut self, node: &'ast syn::FnArg) {
        self.visit_anonymous_scope(Ok(crate::structure::syntax::fn_arg_attributes(node)), "function-argument", node, visit::visit_fn_arg);
    }

    fn visit_bare_fn_arg(&mut self, node: &'ast syn::BareFnArg) {
        self.visit_anonymous_scope(Ok(&node.attrs), "bare-function-argument", node, visit::visit_bare_fn_arg);
    }

    fn visit_variadic(&mut self, node: &'ast syn::Variadic) {
        self.visit_anonymous_scope(Ok(&node.attrs), "variadic-argument", node, visit::visit_variadic);
    }

    fn visit_pat(&mut self, node: &'ast syn::Pat) {
        self.visit_anonymous_scope(pat_attributes(node), "pattern", node, visit::visit_pat);
    }

    fn visit_arm(&mut self, node: &'ast Arm) {
        self.visit_anonymous_scope(Ok(&node.attrs), "match-arm", node, visit::visit_arm);
    }

    fn visit_local(&mut self, node: &'ast Local) {
        self.visit_anonymous_scope(Ok(&node.attrs), "local", node, visit::visit_local);
    }

    fn visit_stmt_macro(&mut self, node: &'ast StmtMacro) {
        self.visit_anonymous_scope(Ok(&node.attrs), "statement-macro", node, visit::visit_stmt_macro);
    }

    fn visit_expr(&mut self, node: &'ast Expr) {
        self.visit_anonymous_scope(expr_attributes(node), "expression", node, visit::visit_expr);
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if self.error.is_none() {
            if let Err(error) = validate_audited_module_path(attribute) {
                self.error = Some(error);
            } else if let Err(error) = self.record_attribute(attribute, false) {
                self.error = Some(error);
            }
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if self.error.is_none() {
            if node.path.segments.last().is_some_and(|segment| segment.ident.unraw() == "include") {
                self.error = Some(anyhow::anyhow!("Rust source include! cannot be classified safely"));
            } else if let Err(error) = self.record_macro_tokens(&node.tokens) {
                self.error = Some(error);
            }
        }
        visit::visit_macro(self, node);
    }
}

fn validate_audited_module_path(attribute: &Attribute) -> Result<()> {
    validate_audited_module_path_meta(&attribute.meta)
}

fn validate_audited_module_path_meta(meta: &Meta) -> Result<()> {
    if meta.path().is_ident("path") {
        let Meta::NameValue(value) = meta else {
            bail!("explicit Rust module path must use a string literal");
        };
        let Expr::Lit(expression) = &value.value else {
            bail!("explicit Rust module path must use a string literal");
        };
        let syn::Lit::Str(value) = &expression.lit else {
            bail!("explicit Rust module path must use a string literal");
        };
        let module_path = value.value();
        let path = Path::new(&module_path);
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
            || path.is_absolute()
            || path.components().any(|component| !matches!(component, Component::Normal(_)))
        {
            bail!("explicit Rust module path must name a normalized .rs source in the audited source tree");
        }
        return Ok(());
    }
    if !meta.path().is_ident("cfg_attr") {
        return Ok(());
    }
    let Meta::List(list) = meta else {
        return Ok(());
    };
    let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .context("parse cfg_attr arguments for suppression source classification")?;
    for nested in arguments.iter().skip(1) {
        validate_audited_module_path_meta(nested)?;
    }
    Ok(())
}
