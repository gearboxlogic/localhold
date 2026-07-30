use anyhow::{Context, Result};
use proc_macro2::{Delimiter, Group, TokenStream, TokenTree};
use quote::{ToTokens, quote};
use sha2::{Digest, Sha256};
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::{Meta, Token};

use crate::scan::syntax_fingerprint;

pub(super) fn suppression_free_fingerprint(tokens: &impl ToTokens) -> String {
    syntax_fingerprint(&without_suppression_attributes(tokens.to_token_stream()))
}

pub(in crate::structure::suppression) fn external_content_fingerprint<'a>(sources: impl IntoIterator<Item = (&'a str, &'a syn::File)>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"localhold-external-module-content-v2");
    for (logical_module, syntax) in sources {
        let fingerprint = suppression_free_fingerprint(syntax);
        for field in [logical_module, fingerprint.as_str()] {
            digest.update(field.len().to_string().as_bytes());
            digest.update(b":");
            digest.update(field.as_bytes());
        }
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn external_target_fingerprint(intrinsic: &str, external: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"localhold-external-module-target-v1");
    for field in [intrinsic, external] {
        digest.update(field.len().to_string().as_bytes());
        digest.update(b":");
        digest.update(field.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn suppression_site_fingerprint(meta: &Meta, activation_identity: Option<&str>) -> Result<String> {
    let normalized = normalized_suppression_meta(meta)?;
    let activation = activation_identity.unwrap_or_default();
    let mut digest = Sha256::new();
    digest.update(b"localhold-suppression-context-v1");
    digest.update(activation.len().to_string().as_bytes());
    digest.update(b":");
    digest.update(activation.as_bytes());
    digest.update(normalized.to_string().as_bytes());
    Ok(format!("{:x}", digest.finalize()))
}

fn normalized_suppression_meta(meta: &Meta) -> Result<TokenStream> {
    let Meta::List(list) = meta else {
        return Ok(meta.to_token_stream());
    };
    if !list.path.is_ident("allow") && !list.path.is_ident("expect") && !list.path.is_ident("warn") {
        return Ok(meta.to_token_stream());
    }
    let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .context("parse lint suppression context")?;
    let retained = arguments.into_iter().filter(|argument| !matches!(argument, Meta::Path(_)));
    let path = &list.path;
    Ok(quote!(#path(#(#retained),*)))
}

fn without_suppression_attributes(tokens: TokenStream) -> TokenStream {
    let tokens = tokens.into_iter().collect::<Vec<_>>();
    let mut filtered = TokenStream::new();
    let mut index = 0_usize;
    while index < tokens.len() {
        if let Some((attribute_index, group)) = attribute_group(&tokens, index) {
            if let Some(attribute) = normalized_attribute(group) {
                filtered.extend(tokens[index..attribute_index].iter().cloned());
                let mut filtered_group = Group::new(Delimiter::Bracket, without_suppression_attributes(attribute));
                filtered_group.set_span(group.span());
                filtered.extend([TokenTree::Group(filtered_group)]);
            }
            index = attribute_index.saturating_add(1);
            continue;
        }
        let token = match &tokens[index] {
            TokenTree::Group(group) => {
                let mut filtered_group = Group::new(group.delimiter(), without_suppression_attributes(group.stream()));
                filtered_group.set_span(group.span());
                TokenTree::Group(filtered_group)
            }
            token => token.clone(),
        };
        filtered.extend([token]);
        index = index.saturating_add(1);
    }
    filtered
}

fn normalized_attribute(group: &Group) -> Option<TokenStream> {
    let meta = syn::parse2::<Meta>(group.stream()).ok()?;
    normalized_meta_without_suppressions(&meta)
}

fn normalized_meta_without_suppressions(meta: &Meta) -> Option<TokenStream> {
    if meta.path().is_ident("allow") || meta.path().is_ident("expect") || meta.path().is_ident("warn") {
        return None;
    }
    if !meta.path().is_ident("cfg_attr") {
        return Some(meta.to_token_stream());
    }
    let Meta::List(list) = meta else {
        return Some(meta.to_token_stream());
    };
    let arguments = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone()).ok()?;
    let mut arguments = arguments.into_iter();
    let condition = arguments.next()?;
    let nested = arguments.filter_map(|meta| normalized_meta_without_suppressions(&meta)).collect::<Vec<_>>();
    if nested.is_empty() {
        return None;
    }
    let path = &list.path;
    Some(quote!(#path(#condition, #(#nested),*)))
}

fn attribute_group(tokens: &[TokenTree], index: usize) -> Option<(usize, &Group)> {
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
