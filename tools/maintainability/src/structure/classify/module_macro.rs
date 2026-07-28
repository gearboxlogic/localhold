use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use proc_macro2::{TokenStream, TokenTree};
use syn::visit::{self, Visit};
use syn::{Arm, Expr, GenericParam, ImplItem, Item, Local, TraitItem, Variant};

use super::super::syntax::{
    attributes_disable_production, expr_attributes, generic_param_attributes, impl_item_attributes, item_is_test_only, normalized_ident, trait_item_attributes,
};
use crate::scan::RESERVED_LOCAL_MACROS;

pub(super) fn audit_reviewed_macro_definitions(file: &syn::File) -> Result<()> {
    let mut audit = ReviewedMacroAudit { error: None };
    audit.visit_file(file);
    audit.error.map_or(Ok(()), Err)
}

pub(super) fn safe_macro_definitions(items: &[syn::Item], parent_test_only: bool, inherited: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    let mut definitions = BTreeMap::new();
    for item in items {
        let syn::Item::Macro(item_macro) = item else {
            continue;
        };
        let Some(name) = &item_macro.ident else {
            continue;
        };
        if parent_test_only || item_is_test_only(item)? {
            continue;
        }
        let name = normalized_ident(name);
        definitions.entry(name).and_modify(|definition| *definition = None).or_insert(Some(&item_macro.mac.tokens));
    }
    let mut safe = inherited.clone();
    loop {
        let before = safe.len();
        for (name, tokens) in definitions.iter().filter_map(|(name, tokens)| tokens.map(|tokens| (name, tokens))) {
            if !token_stream_names_module(tokens) && !token_stream_has_opaque_parameters(tokens) && !token_stream_uses_unknown_macro(tokens, &safe) {
                safe.insert(name.clone());
            }
        }
        if safe.len() == before {
            return Ok(safe);
        }
    }
}

pub(super) fn record_item_macro(
    item: &syn::Item,
    parent_test_only: bool,
    source_path: &str,
    safe_macros: &BTreeSet<String>,
    opaque_sources: &mut BTreeSet<String>,
) -> Result<bool> {
    let syn::Item::Macro(item_macro) = item else {
        return Ok(false);
    };
    if parent_test_only || item_is_test_only(item)? || item_macro.ident.is_some() {
        return Ok(true);
    }
    if !is_known_safe_macro_invocation(item_macro, safe_macros) {
        opaque_sources.insert(source_path.to_owned());
    }
    Ok(true)
}

fn is_known_safe_macro_invocation(item: &syn::ItemMacro, safe_macros: &BTreeSet<String>) -> bool {
    let mut segments = item.mac.path.segments.iter();
    let Some(segment) = segments.next() else {
        return false;
    };
    segments.next().is_none() && safe_macros.contains(&normalized_ident(&segment.ident))
}

fn token_stream_names_module(tokens: &TokenStream) -> bool {
    tokens.clone().into_iter().any(|token| match token {
        TokenTree::Group(group) => token_stream_names_module(&group.stream()),
        TokenTree::Ident(ident) => ident == "mod",
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn token_stream_names_restricted_module(tokens: &TokenStream) -> Option<String> {
    tokens.clone().into_iter().find_map(|token| match token {
        TokenTree::Group(group) => token_stream_names_restricted_module(&group.stream()),
        TokenTree::Ident(ident) => {
            let name = normalized_ident(&ident);
            matches!(name.as_str(), "server" | "ui").then_some(name)
        }
        TokenTree::Literal(literal) => string_literal_names_restricted_module(&literal),
        TokenTree::Punct(_) => None,
    })
}

fn string_literal_names_restricted_module(literal: &proc_macro2::Literal) -> Option<String> {
    let Ok(syn::Lit::Str(literal)) = syn::parse_str::<syn::Lit>(&literal.to_string()) else {
        return None;
    };
    let value = literal.value();
    if !value.contains("::") {
        return None;
    }
    value
        .parse::<TokenStream>()
        .map_or_else(|_| Some("unclassifiable path literal".to_owned()), |tokens| token_stream_names_restricted_module(&tokens))
}

struct ReviewedMacroAudit {
    error: Option<anyhow::Error>,
}

impl ReviewedMacroAudit {
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

    fn audit_item_macro(&mut self, item: &syn::ItemMacro) {
        let Some(name) = item.ident.as_ref().map(normalized_ident) else {
            return;
        };
        if !RESERVED_LOCAL_MACROS.contains(&name.as_str()) {
            return;
        }
        if let Some(restricted) = token_stream_names_restricted_module(&item.mac.tokens) {
            self.error = Some(anyhow::anyhow!("reviewed local macro {name:?} generates restricted crate module {restricted:?}"));
        } else if token_stream_has_opaque_parameters(&item.mac.tokens) {
            self.error = Some(anyhow::anyhow!(
                "reviewed local macro {name:?} uses non-literal metavariables that can conceal a restricted crate path"
            ));
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

impl<'ast> Visit<'ast> for ReviewedMacroAudit {
    visit_production_node!(visit_file, visit_file, syn::File, node => attributes_disable_production(&node.attrs));
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
    visit_production_node!(visit_variant, visit_variant, Variant, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_arm, visit_arm, Arm, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_local, visit_local, Local, node => attributes_disable_production(&node.attrs));
    visit_production_node!(
        visit_expr,
        visit_expr,
        Expr,
        node => expr_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(
        visit_generic_param,
        visit_generic_param,
        GenericParam,
        node => attributes_disable_production(generic_param_attributes(node))
    );

    fn visit_item(&mut self, item: &'ast Item) {
        if self.skip_test_only(item_is_test_only(item)) {
            return;
        }
        if let Item::Macro(item_macro) = item {
            self.audit_item_macro(item_macro);
        }
        if self.error.is_none() {
            visit::visit_item(self, item);
        }
    }
}

fn token_stream_has_opaque_parameters(tokens: &TokenStream) -> bool {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    for window in tokens.windows(4) {
        let [TokenTree::Punct(dollar), TokenTree::Ident(_), TokenTree::Punct(colon), TokenTree::Ident(fragment)] = window else {
            continue;
        };
        if dollar.as_char() == '$' && colon.as_char() == ':' && normalized_ident(fragment) != "literal" {
            return true;
        }
    }
    tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => token_stream_has_opaque_parameters(&group.stream()),
        TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn token_stream_uses_unknown_macro(tokens: &TokenStream, safe_macros: &BTreeSet<String>) -> bool {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if let TokenTree::Group(group) = token
            && token_stream_uses_unknown_macro(&group.stream(), safe_macros)
        {
            return true;
        }
        let TokenTree::Punct(punctuation) = token else {
            continue;
        };
        if punctuation.as_char() != '!' || !matches!(tokens.get(index + 1), Some(TokenTree::Group(_))) {
            continue;
        }
        let Some(TokenTree::Ident(name)) = index.checked_sub(1).and_then(|previous| tokens.get(previous)) else {
            return true;
        };
        let name = normalized_ident(name);
        if !matches!(name.as_str(), "concat" | "stringify") && !safe_macros.contains(&name) {
            return true;
        }
    }
    false
}
