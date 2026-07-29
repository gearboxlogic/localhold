use proc_macro2::{Group, Ident, TokenStream, TokenTree};
use quote::quote;

use super::concrete::tokens_contain_concrete_store;
use super::{ProductionCfgContext, ProductionSourceRevision, ProductionSyntaxContext, ProductionSyntaxOptions, production_syntax_facts_with_context};

pub(super) fn contains_production_concrete_store(tokens: &TokenStream, cfg: &ProductionCfgContext) -> bool {
    let transcribers = macro_transcribers(tokens);
    if transcribers.is_empty() {
        return tokens_contain_concrete_store(tokens);
    }
    transcribers
        .into_iter()
        .filter(tokens_contain_concrete_store)
        .any(|tokens| transcriber_contains_production_concrete_store(&tokens, cfg))
}

fn macro_transcribers(tokens: &TokenStream) -> Vec<TokenStream> {
    tokens
        .clone()
        .into_iter()
        .collect::<Vec<_>>()
        .windows(3)
        .filter_map(|window| match window {
            [TokenTree::Punct(equals), TokenTree::Punct(greater), TokenTree::Group(group)] if equals.as_char() == '=' && greater.as_char() == '>' => Some(group.stream()),
            _ => None,
        })
        .collect()
}

fn transcriber_contains_production_concrete_store(tokens: &TokenStream, cfg: &ProductionCfgContext) -> bool {
    let tokens = normalize_metavariables(tokens);
    let syntax = syn::parse2::<syn::File>(tokens.clone()).or_else(|_| {
        syn::parse2::<syn::File>(quote! {
            fn __localhold_macro_transcriber() {
                #tokens
            }
        })
    });
    let Ok(syntax) = syntax else {
        return true;
    };
    let facts = production_syntax_facts_with_context(
        &syntax,
        "macro-transcriber.rs",
        None,
        ProductionSyntaxOptions {
            collect_internal_imports: false,
            rust_2015_absolute_paths: false,
            require_reviewed_expansions: false,
        },
        ProductionSyntaxContext {
            cfg: cfg.clone(),
            declaration_ancestors: Vec::new(),
            module_exposure_cfg: Some(cfg.clone()),
            source_revision: ProductionSourceRevision::Current,
        },
    );
    match facts {
        Ok(facts) => facts.concrete_stores.sqlite_store != 0 || facts.concrete_stores.postgres_store != 0,
        Err(_) => true,
    }
}

fn normalize_metavariables(tokens: &TokenStream) -> TokenStream {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    let mut normalized = TokenStream::new();
    let mut index = 0;
    while index < tokens.len() {
        match (&tokens[index], tokens.get(index + 1)) {
            (TokenTree::Punct(dollar), Some(TokenTree::Ident(ident))) if dollar.as_char() == '$' => {
                let replacement = if ident == "crate" { "crate" } else { "__localhold_macro_value" };
                normalized.extend([TokenTree::Ident(Ident::new(replacement, ident.span()))]);
                index += 2;
            }
            (TokenTree::Group(group), _) => {
                let mut replacement = Group::new(group.delimiter(), normalize_metavariables(&group.stream()));
                replacement.set_span(group.span());
                normalized.extend([TokenTree::Group(replacement)]);
                index += 1;
            }
            (token, _) => {
                normalized.extend([token.clone()]);
                index += 1;
            }
        }
    }
    normalized
}
