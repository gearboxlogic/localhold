use anyhow::{Context, Result};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::{Attribute, Meta, Token};

use crate::scan::syntax_fingerprint;

use super::super::{cfg_can_apply_in_production, normalized_ident};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ConcreteStoreCounts {
    pub sqlite_store: usize,
    pub postgres_store: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ConcreteStoreSites {
    pub sqlite_store: Vec<String>,
    pub postgres_store: Vec<String>,
}

#[derive(Default)]
pub(super) struct ConcreteStoreInventory {
    pub(super) counts: ConcreteStoreCounts,
    pub(super) sites: ConcreteStoreSites,
    pub(super) generic_default_sites: ConcreteStoreSites,
}

impl ConcreteStoreInventory {
    pub(super) fn finish(&mut self) {
        self.sites.sqlite_store.sort();
        self.sites.postgres_store.sort();
        self.generic_default_sites.sqlite_store.sort();
        self.generic_default_sites.postgres_store.sort();
    }

    pub(super) fn record_ident(&mut self, ident: &proc_macro2::Ident, site_context: &str) -> Result<()> {
        self.record_name(&normalized_ident(ident), site_context)
    }

    pub(super) fn record_tokens(&mut self, tokens: &TokenStream, site_context: &str) -> Result<()> {
        for token in tokens.clone() {
            match token {
                TokenTree::Group(group) => self.record_tokens(&group.stream(), site_context)?,
                TokenTree::Ident(ident) => self.record_ident(&ident, site_context)?,
                TokenTree::Literal(_) | TokenTree::Punct(_) => {}
            }
        }
        Ok(())
    }

    pub(super) fn record_generic_default(&mut self, tokens: &impl ToTokens, site_context: &str) -> Result<()> {
        self.record_generic_default_tokens(&tokens.to_token_stream(), site_context)
    }

    fn record_generic_default_tokens(&mut self, tokens: &TokenStream, site_context: &str) -> Result<()> {
        for token in tokens.clone() {
            match token {
                TokenTree::Group(group) => self.record_generic_default_tokens(&group.stream(), site_context)?,
                TokenTree::Ident(ident) => self.record_generic_default_name(&normalized_ident(&ident), site_context),
                TokenTree::Literal(_) | TokenTree::Punct(_) => {}
            }
        }
        Ok(())
    }

    pub(super) fn record_attribute(&mut self, attribute: &Attribute, site_context: &str) -> Result<()> {
        if !attribute.path().is_ident("cfg_attr") {
            return self.record_meta(&attribute.meta, site_context);
        }
        let Meta::List(list) = &attribute.meta else {
            return Ok(());
        };
        self.record_cfg_attr(&list.tokens, site_context)
    }

    fn record_cfg_attr(&mut self, tokens: &TokenStream, site_context: &str) -> Result<()> {
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
            self.record_cfg_nested(&nested, site_context)?;
        }
        Ok(())
    }

    fn record_cfg_nested(&mut self, nested: &Meta, site_context: &str) -> Result<()> {
        if !nested.path().is_ident("cfg_attr") {
            return self.record_meta(nested, site_context);
        }
        let Meta::List(list) = nested else {
            return Ok(());
        };
        self.record_cfg_attr(&list.tokens, site_context)
    }

    fn record_meta(&mut self, meta: &Meta, site_context: &str) -> Result<()> {
        let governed = matches!(
            meta.path().segments.last().map(|segment| normalized_ident(&segment.ident)),
            Some(name) if matches!(name.as_str(), "serde" | "schemars")
        );
        match meta {
            Meta::Path(path) => {
                for segment in &path.segments {
                    self.record_ident(&segment.ident, site_context)?;
                }
                Ok(())
            }
            Meta::NameValue(value) => self.record_attribute_tokens(&value.value.to_token_stream(), governed, site_context),
            Meta::List(list) if governed => {
                let nested = Punctuated::<Meta, Token![,]>::parse_terminated
                    .parse2(list.tokens.clone())
                    .context("parse governed attribute arguments for production concrete-store classification")?;
                for meta in nested {
                    self.record_governed_meta(&meta, false, site_context)?;
                }
                Ok(())
            }
            Meta::List(list) => self.record_attribute_tokens(&list.tokens, false, site_context),
        }
    }

    fn record_governed_meta(&mut self, meta: &Meta, inherited_rust_fragment: bool, site_context: &str) -> Result<()> {
        let rust_fragment = inherited_rust_fragment
            || meta
                .path()
                .segments
                .last()
                .map(|segment| normalized_ident(&segment.ident))
                .is_some_and(|name| is_rust_fragment_key(&name));
        match meta {
            Meta::Path(path) => {
                for segment in &path.segments {
                    self.record_ident(&segment.ident, site_context)?;
                }
                Ok(())
            }
            Meta::NameValue(value) => self.record_attribute_tokens(&value.value.to_token_stream(), rust_fragment, site_context),
            Meta::List(list) => self.record_governed_list(&list.tokens, rust_fragment, site_context),
        }
    }

    fn record_governed_list(&mut self, tokens: &TokenStream, rust_fragment: bool, site_context: &str) -> Result<()> {
        let Ok(nested) = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(tokens.clone()) else {
            return self.record_attribute_tokens(tokens, rust_fragment, site_context);
        };
        for meta in nested {
            self.record_governed_meta(&meta, rust_fragment, site_context)?;
        }
        Ok(())
    }

    fn record_attribute_tokens(&mut self, tokens: &TokenStream, rust_fragment: bool, site_context: &str) -> Result<()> {
        for token in tokens.clone() {
            match token {
                TokenTree::Group(group) => self.record_attribute_tokens(&group.stream(), rust_fragment, site_context)?,
                TokenTree::Literal(literal) => self.record_attribute_literal(&literal, rust_fragment, site_context)?,
                TokenTree::Ident(ident) => self.record_ident(&ident, site_context)?,
                TokenTree::Punct(_) => {}
            }
        }
        Ok(())
    }

    fn record_attribute_literal(&mut self, literal: &proc_macro2::Literal, rust_fragment: bool, site_context: &str) -> Result<()> {
        let Ok(syn::Lit::Str(literal)) = syn::parse_str::<syn::Lit>(&literal.to_string()) else {
            return Ok(());
        };
        let value = literal.value();
        if !rust_fragment && !value.contains("::") {
            return Ok(());
        }
        let Ok(tokens) = value.parse::<TokenStream>() else {
            return Ok(());
        };
        self.record_tokens(&tokens, site_context)
    }

    fn record_name(&mut self, name: &str, site_context: &str) -> Result<()> {
        let (count, sites) = match name {
            "SqliteStore" => (&mut self.counts.sqlite_store, &mut self.sites.sqlite_store),
            "PostgresStore" => (&mut self.counts.postgres_store, &mut self.sites.postgres_store),
            _ => return Ok(()),
        };
        *count = count.checked_add(1).context("production concrete-store name count overflow")?;
        sites.push(syntax_fingerprint(&site_context));
        Ok(())
    }

    fn record_generic_default_name(&mut self, name: &str, site_context: &str) {
        let sites = match name {
            "SqliteStore" => &mut self.generic_default_sites.sqlite_store,
            "PostgresStore" => &mut self.generic_default_sites.postgres_store,
            _ => return,
        };
        sites.push(syntax_fingerprint(&site_context));
    }
}

pub(super) fn context_fingerprint(parent: Option<&str>, kind: &str, syntax: &impl ToTokens) -> String {
    let local = format!("{kind}:{}", syntax_fingerprint(syntax));
    match parent {
        Some(parent) => syntax_fingerprint(&format!("{parent}\0{local}")),
        None => local,
    }
}

pub(super) fn tokens_contain_concrete_store(tokens: &TokenStream) -> bool {
    for token in tokens.clone() {
        match token {
            TokenTree::Group(group) if tokens_contain_concrete_store(&group.stream()) => return true,
            TokenTree::Ident(ident) if is_concrete_store_name(&normalized_ident(&ident)) => return true,
            TokenTree::Group(_) | TokenTree::Ident(_) | TokenTree::Literal(_) | TokenTree::Punct(_) => {}
        }
    }
    false
}

pub(super) fn is_concrete_store_name(name: &str) -> bool {
    matches!(name, "SqliteStore" | "PostgresStore")
}

fn is_rust_fragment_key(name: &str) -> bool {
    matches!(
        name,
        "bound" | "crate" | "deserialize_with" | "from" | "into" | "remote" | "schema_with" | "serialize_with" | "transform" | "try_from" | "with"
    )
}
