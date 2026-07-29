use anyhow::{Context, Result};
use proc_macro2::{Group, TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::{Attribute, Field, Fields, Generics, ItemStruct, Meta, Token};

use crate::scan::syntax_fingerprint;

use super::super::{ProductionCfgContext, generic_param_attributes, normalized_ident, production_cfg_attr_metas, production_cfg_context};
use super::tokens::resolving_tokens;

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

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ConcreteStoreSignatureSite {
    pub fingerprint: String,
    pub item_path: Vec<String>,
    #[serde(skip)]
    pub(in crate::structure) cfg: ProductionCfgContext,
    #[serde(skip)]
    pub(in crate::structure) impl_self_type: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ConcreteStoreSignatureSites {
    pub sqlite_store: Vec<ConcreteStoreSignatureSite>,
    pub postgres_store: Vec<ConcreteStoreSignatureSite>,
}

#[derive(Default)]
pub(super) struct ConcreteStoreInventory {
    pub(super) counts: ConcreteStoreCounts,
    pub(super) public_struct_declarations: ConcreteStoreSites,
    pub(super) sites: ConcreteStoreSites,
    pub(super) generic_default_sites: ConcreteStoreSites,
    pub(super) signature_sites: ConcreteStoreSignatureSites,
    pub(super) binding_sites: ConcreteStoreSites,
}

pub(super) struct SignatureSiteContext<'a> {
    pub(super) item_path: &'a [String],
    pub(super) cfg: &'a ProductionCfgContext,
    pub(super) impl_self_type: bool,
}

impl ConcreteStoreInventory {
    pub(super) fn finish(&mut self) {
        self.public_struct_declarations.sqlite_store.sort();
        self.public_struct_declarations.postgres_store.sort();
        self.sites.sqlite_store.sort();
        self.sites.postgres_store.sort();
        self.generic_default_sites.sqlite_store.sort();
        self.generic_default_sites.postgres_store.sort();
        self.signature_sites.sqlite_store.sort();
        self.signature_sites.postgres_store.sort();
        self.binding_sites.sqlite_store.sort();
        self.binding_sites.postgres_store.sort();
    }

    pub(super) fn record_ident(&mut self, ident: &proc_macro2::Ident, site_context: &str) -> Result<()> {
        self.record_name(&normalized_ident(ident), site_context)
    }

    pub(super) fn record_public_struct_declaration(&mut self, item: &ItemStruct, item_path: &[String], cfg: &ProductionCfgContext, ancestors: &str) -> Result<()> {
        let name = normalized_ident(&item.ident);
        let sites = match name.as_str() {
            "SqliteStore" => &mut self.public_struct_declarations.sqlite_store,
            "PostgresStore" => &mut self.public_struct_declarations.postgres_store,
            _ => return Ok(()),
        };
        let mut declaration = item.clone();
        declaration.attrs.retain(|attribute| !attribute.path().is_ident("doc"));
        declaration.generics = production_generics(&declaration.generics, cfg)?;
        match &mut declaration.fields {
            Fields::Named(fields) => fields.named = production_fields(&fields.named, cfg)?,
            Fields::Unnamed(fields) => fields.unnamed = production_fields(&fields.unnamed, cfg)?,
            Fields::Unit => {}
        }
        for field in &mut declaration.fields {
            field.attrs.retain(|attribute| !attribute.path().is_ident("doc"));
        }
        let declaration = syntax_fingerprint(&without_documentation(&declaration.to_token_stream()));
        let declaration = if cfg.identity().is_empty() && ancestors.is_empty() {
            declaration
        } else {
            syntax_fingerprint(&format!("declaration:{declaration}\0cfg:{}\0ancestors:{ancestors}", cfg.identity()))
        };
        sites.push(declaration.clone());
        self.record_exposure_signature_name(
            &name,
            &format!("canonical-declaration:{declaration}"),
            &SignatureSiteContext {
                item_path,
                cfg,
                impl_self_type: false,
            },
        );
        Ok(())
    }

    pub(super) fn record_tokens(&mut self, tokens: &TokenStream, site_context: &str) -> Result<()> {
        for token in resolving_tokens(tokens) {
            match token {
                TokenTree::Group(group) => self.record_tokens(&group.stream(), site_context)?,
                TokenTree::Ident(ident) => self.record_ident(&ident, site_context)?,
                TokenTree::Literal(_) | TokenTree::Punct(_) => {}
            }
        }
        Ok(())
    }

    pub(super) fn record_generic_default_ident(&mut self, ident: &proc_macro2::Ident, site_context: &str) {
        self.record_generic_default_name(&normalized_ident(ident), site_context);
    }

    pub(super) fn record_generic_default_tokens(&mut self, tokens: &TokenStream, site_context: &str) {
        for token in resolving_tokens(tokens) {
            match token {
                TokenTree::Group(group) => self.record_generic_default_tokens(&group.stream(), site_context),
                TokenTree::Ident(ident) => self.record_generic_default_name(&normalized_ident(&ident), site_context),
                TokenTree::Literal(_) | TokenTree::Punct(_) => {}
            }
        }
    }

    pub(super) fn record_signature_tokens(&mut self, tokens: &TokenStream, site_context: &str, signature: &SignatureSiteContext<'_>) {
        for token in resolving_tokens(tokens) {
            match token {
                TokenTree::Group(group) => self.record_signature_tokens(&group.stream(), site_context, signature),
                TokenTree::Ident(ident) => self.record_signature_name(&normalized_ident(&ident), site_context, signature),
                TokenTree::Literal(_) | TokenTree::Punct(_) => {}
            }
        }
    }

    pub(super) fn record_exposure_signature_tokens(&mut self, tokens: &TokenStream, site_context: &str, signature: &SignatureSiteContext<'_>) {
        for token in resolving_tokens(tokens) {
            match token {
                TokenTree::Group(group) => self.record_exposure_signature_tokens(&group.stream(), site_context, signature),
                TokenTree::Ident(ident) => {
                    self.record_exposure_signature_name(&normalized_ident(&ident), site_context, signature);
                }
                TokenTree::Literal(_) | TokenTree::Punct(_) => {}
            }
        }
    }

    pub(super) fn record_binding_tokens(&mut self, tokens: &TokenStream, site_context: &str) {
        for token in resolving_tokens(tokens) {
            match token {
                TokenTree::Group(group) => self.record_binding_tokens(&group.stream(), site_context),
                TokenTree::Ident(ident) => self.record_binding_name(&normalized_ident(&ident), site_context),
                TokenTree::Literal(_) | TokenTree::Punct(_) => {}
            }
        }
    }

    pub(super) fn record_attribute(&mut self, attribute: &Attribute, site_context: &str, cfg_context: &ProductionCfgContext) -> Result<()> {
        if !attribute.path().is_ident("cfg_attr") {
            return self.record_meta(&attribute.meta, site_context);
        }
        let Meta::List(list) = &attribute.meta else {
            return Ok(());
        };
        self.record_cfg_attr(&list.tokens, site_context, cfg_context)
    }

    pub(super) fn record_generic_default_attribute(&mut self, attribute: &Attribute, site_context: &str, cfg_context: &ProductionCfgContext) -> Result<()> {
        let mut discovered = Self::default();
        discovered.record_attribute(attribute, site_context, cfg_context)?;
        self.generic_default_sites.sqlite_store.extend(discovered.sites.sqlite_store);
        self.generic_default_sites.postgres_store.extend(discovered.sites.postgres_store);
        Ok(())
    }

    fn record_cfg_attr(&mut self, tokens: &TokenStream, site_context: &str, cfg_context: &ProductionCfgContext) -> Result<()> {
        for meta in production_cfg_attr_metas(tokens, cfg_context)? {
            self.record_meta(&meta, site_context)?;
        }
        Ok(())
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
        if !rust_fragment {
            return Ok(());
        }
        let Ok(syn::Lit::Str(literal)) = syn::parse_str::<syn::Lit>(&literal.to_string()) else {
            return Ok(());
        };
        let value = literal.value();
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

    fn record_signature_name(&mut self, name: &str, site_context: &str, signature: &SignatureSiteContext<'_>) {
        self.record_exposure_signature_name(name, site_context, signature);
    }

    fn record_exposure_signature_name(&mut self, name: &str, site_context: &str, signature: &SignatureSiteContext<'_>) {
        let sites = match name {
            "SqliteStore" => &mut self.signature_sites.sqlite_store,
            "PostgresStore" => &mut self.signature_sites.postgres_store,
            _ => return,
        };
        sites.push(ConcreteStoreSignatureSite {
            fingerprint: syntax_fingerprint(&site_context),
            item_path: signature.item_path.to_vec(),
            cfg: signature.cfg.clone(),
            impl_self_type: signature.impl_self_type,
        });
    }

    fn record_binding_name(&mut self, name: &str, site_context: &str) {
        let sites = match name {
            "SqliteStore" => &mut self.binding_sites.sqlite_store,
            "PostgresStore" => &mut self.binding_sites.postgres_store,
            _ => return,
        };
        sites.push(syntax_fingerprint(&site_context));
    }
}

pub(super) fn production_generics(generics: &Generics, cfg: &ProductionCfgContext) -> Result<Generics> {
    let mut production = generics.clone();
    let mut parameters = Vec::new();
    for parameter in &generics.params {
        if production_cfg_context(generic_param_attributes(parameter), cfg)?.is_some() {
            parameters.push(parameter.clone());
        }
    }
    production.params = parameters.into_iter().collect();
    if production.params.is_empty() {
        production.lt_token = None;
        production.gt_token = None;
    }
    Ok(production)
}

fn production_fields(fields: &Punctuated<Field, Token![,]>, cfg: &ProductionCfgContext) -> Result<Punctuated<Field, Token![,]>> {
    let mut production = Punctuated::new();
    for field in fields {
        if production_cfg_context(&field.attrs, cfg)?.is_some() {
            production.push(field.clone());
        }
    }
    if !production.is_empty() && !production.trailing_punct() {
        production.push_punct(syn::token::Comma::default());
    }
    Ok(production)
}

pub(super) fn context_fingerprint(parent: Option<&str>, kind: &str, syntax: &impl ToTokens) -> String {
    let local = format!("{kind}:{}", syntax_fingerprint(&without_documentation(&syntax.to_token_stream())));
    match parent {
        Some(parent) => syntax_fingerprint(&format!("{parent}\0{local}")),
        None => local,
    }
}

fn without_documentation(tokens: &TokenStream) -> TokenStream {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    let mut normalized = TokenStream::new();
    let mut index = 0;
    while index < tokens.len() {
        if let Some((attribute_end, attribute)) = parsed_attribute(&tokens, index) {
            if attribute.path().is_ident("doc") {
                index = attribute_end + 1;
                continue;
            }
            if attribute.path().is_ident("cfg_attr") {
                extend_conditional_attribute(&mut normalized, &tokens, index, attribute_end, attribute);
                index = attribute_end + 1;
                continue;
            }
        }
        match &tokens[index] {
            TokenTree::Group(group) => {
                let mut replacement = Group::new(group.delimiter(), without_documentation(&group.stream()));
                replacement.set_span(group.span());
                normalized.extend([TokenTree::Group(replacement)]);
            }
            token => normalized.extend([token.clone()]),
        }
        index += 1;
    }
    normalized
}

fn extend_conditional_attribute(output: &mut TokenStream, tokens: &[TokenTree], start: usize, end: usize, attribute: Meta) {
    let Some(attribute) = without_conditional_documentation(attribute) else {
        return;
    };
    output.extend(tokens[start..end].iter().cloned());
    let TokenTree::Group(original) = &tokens[end] else {
        unreachable!("parsed attribute must end in a bracket group");
    };
    let mut replacement = Group::new(original.delimiter(), attribute.to_token_stream());
    replacement.set_span(original.span());
    output.extend([TokenTree::Group(replacement)]);
}

fn parsed_attribute(tokens: &[TokenTree], index: usize) -> Option<(usize, Meta)> {
    if !matches!(tokens.get(index), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '#') {
        return None;
    }
    let group_index = if matches!(tokens.get(index + 1), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '!') {
        index + 2
    } else {
        index + 1
    };
    let Some(TokenTree::Group(group)) = tokens.get(group_index) else {
        return None;
    };
    if group.delimiter() != proc_macro2::Delimiter::Bracket {
        return None;
    }
    let attribute = syn::parse2::<Meta>(group.stream()).ok()?;
    Some((group_index, attribute))
}

fn without_conditional_documentation(attribute: Meta) -> Option<Meta> {
    if attribute.path().is_ident("doc") {
        return None;
    }
    if !attribute.path().is_ident("cfg_attr") {
        return Some(attribute);
    }
    let Meta::List(mut list) = attribute else {
        return Some(attribute);
    };
    let Ok(nested) = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone()) else {
        return Some(Meta::List(list));
    };
    let mut nested = nested.into_iter();
    let condition = nested.next()?;
    let attributes = nested.filter_map(without_conditional_documentation).collect::<Vec<_>>();
    if attributes.is_empty() {
        return None;
    }
    list.tokens = quote::quote!(#condition, #(#attributes),*);
    Some(Meta::List(list))
}

pub(super) fn tokens_contain_concrete_store(tokens: &TokenStream) -> bool {
    let tokens = resolving_tokens(tokens).into_iter().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if attribute_group(&tokens, index).is_some_and(attribute_group_contains_concrete_store) {
            return true;
        }
        match token {
            TokenTree::Group(group) if tokens_contain_concrete_store(&group.stream()) => return true,
            TokenTree::Ident(ident) if is_concrete_store_name(&normalized_ident(ident)) => return true,
            TokenTree::Group(_) | TokenTree::Ident(_) | TokenTree::Literal(_) | TokenTree::Punct(_) => {}
        }
    }
    false
}

fn attribute_group(tokens: &[TokenTree], index: usize) -> Option<&proc_macro2::Group> {
    if !matches!(tokens.get(index), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '#') {
        return None;
    }
    let group_index = if matches!(tokens.get(index + 1), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '!') {
        index + 2
    } else {
        index + 1
    };
    let Some(TokenTree::Group(group)) = tokens.get(group_index) else {
        return None;
    };
    (group.delimiter() == proc_macro2::Delimiter::Bracket).then_some(group)
}

fn attribute_group_contains_concrete_store(group: &proc_macro2::Group) -> bool {
    let Ok(meta) = syn::parse2::<Meta>(group.stream()) else {
        return false;
    };
    attribute_meta_contains_concrete_store(&meta)
}

fn attribute_meta_contains_concrete_store(meta: &Meta) -> bool {
    if meta.path().is_ident("cfg_attr")
        && let Meta::List(list) = meta
        && let Ok(nested) = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone())
    {
        return nested.iter().skip(1).any(attribute_meta_contains_concrete_store);
    }
    let governed = matches!(
        meta.path().segments.last().map(|segment| normalized_ident(&segment.ident)),
        Some(name) if matches!(name.as_str(), "serde" | "schemars")
    );
    if !governed {
        return false;
    }
    let mut inventory = ConcreteStoreInventory::default();
    inventory.record_meta(meta, "macro-attribute").is_ok() && (inventory.counts.sqlite_store != 0 || inventory.counts.postgres_store != 0)
}

pub(super) fn is_concrete_store_name(name: &str) -> bool {
    matches!(name, "SqliteStore" | "PostgresStore")
}

fn is_rust_fragment_key(name: &str) -> bool {
    matches!(
        name,
        "bound"
            | "crate"
            | "default"
            | "deserialize_with"
            | "example"
            | "from"
            | "getter"
            | "into"
            | "remote"
            | "schema_with"
            | "serialize_with"
            | "skip_serializing_if"
            | "transform"
            | "try_from"
            | "with"
    )
}
