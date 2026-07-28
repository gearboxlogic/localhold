use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use proc_macro2::{TokenStream, TokenTree};

use super::super::syntax::{item_is_test_only, normalized_ident};

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
