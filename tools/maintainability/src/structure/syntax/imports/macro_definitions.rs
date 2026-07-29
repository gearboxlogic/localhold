use anyhow::{Context, Result, bail};
use proc_macro2::{Group, Ident, TokenStream, TokenTree};
use quote::quote;

use crate::scan::syntax_fingerprint;

use super::concrete::tokens_contain_concrete_store;
use super::{ProductionCfgContext, ProductionSourceRevision, ProductionSyntaxContext, ProductionSyntaxOptions, production_syntax_facts_with_context};

pub(super) struct ReviewedMacroTranscriber {
    pub(super) syntax: syn::File,
    pub(super) matcher_fingerprint: String,
}

pub(super) fn contains_production_concrete_store(tokens: &TokenStream, cfg: &ProductionCfgContext) -> bool {
    let transcribers = macro_transcribers(tokens);
    if transcribers.is_empty() {
        return tokens_contain_concrete_store(tokens);
    }
    transcribers
        .into_iter()
        .filter(|transcriber| tokens_contain_concrete_store(&transcriber.tokens))
        .any(|transcriber| transcriber_contains_production_concrete_store(&transcriber.tokens, cfg))
}

pub(super) fn reviewed_macro_transcribers(tokens: &TokenStream) -> Result<Vec<ReviewedMacroTranscriber>> {
    let transcribers = macro_transcribers(tokens);
    if transcribers.is_empty() {
        bail!("the definition has no analyzable macro_rules transcribers");
    }
    transcribers
        .into_iter()
        .map(|transcriber| {
            let syntax = parse_transcriber(&transcriber.tokens).context("parse macro_rules transcriber as Rust syntax")?;
            Ok(ReviewedMacroTranscriber {
                syntax,
                matcher_fingerprint: syntax_fingerprint(&transcriber.matcher),
            })
        })
        .collect()
}

struct MacroTranscriber {
    matcher: TokenStream,
    tokens: TokenStream,
}

fn macro_transcribers(tokens: &TokenStream) -> Vec<MacroTranscriber> {
    tokens
        .clone()
        .into_iter()
        .collect::<Vec<_>>()
        .windows(4)
        .filter_map(|window| match window {
            [
                TokenTree::Group(matcher),
                TokenTree::Punct(equals),
                TokenTree::Punct(greater),
                TokenTree::Group(transcriber),
            ] if equals.as_char() == '=' && greater.as_char() == '>' => Some(MacroTranscriber {
                matcher: matcher.stream(),
                tokens: transcriber.stream(),
            }),
            _ => None,
        })
        .collect()
}

fn transcriber_contains_production_concrete_store(tokens: &TokenStream, cfg: &ProductionCfgContext) -> bool {
    let Ok(syntax) = parse_transcriber(tokens) else {
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

fn parse_transcriber(tokens: &TokenStream) -> Result<syn::File> {
    let tokens = normalize_metavariables(tokens);
    syn::parse2::<syn::File>(tokens.clone())
        .or_else(|_| {
            syn::parse2::<syn::File>(quote! {
                fn __localhold_macro_transcriber() {
                    #tokens
                }
            })
        })
        .map_err(Into::into)
}

fn normalize_metavariables(tokens: &TokenStream) -> TokenStream {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    let mut normalized = TokenStream::new();
    let mut index = 0;
    while index < tokens.len() {
        match (&tokens[index], tokens.get(index + 1)) {
            (TokenTree::Punct(dollar), Some(TokenTree::Group(group))) if dollar.as_char() == '$' => {
                let repetition_end = match (tokens.get(index + 2), tokens.get(index + 3)) {
                    (Some(TokenTree::Punct(operator)), _) if matches!(operator.as_char(), '*' | '+' | '?') => Some(index + 3),
                    (_, Some(TokenTree::Punct(operator))) if matches!(operator.as_char(), '*' | '+' | '?') => Some(index + 4),
                    _ => None,
                };
                if let Some(repetition_end) = repetition_end {
                    normalized.extend(normalize_metavariables(&group.stream()));
                    index = repetition_end;
                    continue;
                }
                normalized.extend([tokens[index].clone()]);
                index += 1;
            }
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
