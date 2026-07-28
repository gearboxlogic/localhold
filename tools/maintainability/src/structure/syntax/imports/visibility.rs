use anyhow::{Context, Result};
use proc_macro2::{TokenStream, TokenTree};
use serde::Serialize;
use syn::{Attribute, Meta, Visibility};

use super::super::{ProductionCfgContext, normalized_ident, production_cfg_attr_metas};
use super::tokens::resolving_tokens;

mod macro_audit;
pub(super) use macro_audit::VisibilityMacroAudit;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct VisibilityCounts {
    pub pub_crate: usize,
    pub pub_super: usize,
}

impl VisibilityCounts {
    pub(super) fn record_visibility(&mut self, visibility: &Visibility) -> Result<()> {
        let Visibility::Restricted(restricted) = visibility else {
            return Ok(());
        };
        let Some(first) = restricted.path.segments.first() else {
            return Ok(());
        };
        self.record_kind(normalized_ident(&first.ident).as_str())
    }

    pub(super) fn record_tokens(&mut self, tokens: &TokenStream) -> Result<()> {
        self.record_resolving_tokens(&resolving_tokens(tokens))
    }

    pub(super) fn from_tokens(tokens: &TokenStream) -> Result<Self> {
        let mut counts = Self::default();
        counts.record_tokens(tokens)?;
        Ok(counts)
    }

    pub(super) const fn is_empty(self) -> bool {
        self.pub_crate == 0 && self.pub_super == 0
    }

    fn record_resolving_tokens(&mut self, tokens: &TokenStream) -> Result<()> {
        let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
        for token in &tokens {
            if let TokenTree::Group(group) = token {
                self.record_resolving_tokens(&group.stream())?;
            }
        }
        for index in 0..tokens.len() {
            if token_ident(tokens.get(index)).as_deref() != Some("pub") {
                continue;
            }
            let Some(TokenTree::Group(restriction)) = tokens.get(index + 1) else {
                continue;
            };
            let restriction = restriction.stream().into_iter().collect::<Vec<_>>();
            let first = token_ident(restriction.first());
            let narrowed = token_ident(restriction.get(1));
            let kind = match first.as_deref() {
                Some("crate") => Some("crate"),
                Some("super") => Some("super"),
                Some("in") => narrowed.as_deref(),
                _ => None,
            };
            if let Some(kind) = kind {
                self.record_kind(kind)?;
            }
        }
        Ok(())
    }

    pub(super) fn record_attribute(&mut self, attribute: &Attribute, cfg_context: &ProductionCfgContext) -> Result<()> {
        if attribute.path().is_ident("cfg_attr") {
            let Meta::List(list) = &attribute.meta else {
                return Ok(());
            };
            return self.record_cfg_attr(&list.tokens, cfg_context);
        }
        self.record_meta_contents(&attribute.meta)
    }

    fn record_cfg_attr(&mut self, tokens: &TokenStream, cfg_context: &ProductionCfgContext) -> Result<()> {
        for meta in production_cfg_attr_metas(tokens, cfg_context)? {
            self.record_meta_contents(&meta)?;
        }
        Ok(())
    }

    fn record_meta_contents(&mut self, meta: &Meta) -> Result<()> {
        match meta {
            Meta::Path(_) => Ok(()),
            Meta::List(list) => self.record_tokens(&list.tokens),
            Meta::NameValue(value) => self.record_tokens(&quote::ToTokens::to_token_stream(&value.value)),
        }
    }

    fn record_kind(&mut self, kind: &str) -> Result<()> {
        let count = match kind {
            "crate" => &mut self.pub_crate,
            "super" => &mut self.pub_super,
            _ => return Ok(()),
        };
        *count = count.checked_add(1).context("production restricted-visibility count overflow")?;
        Ok(())
    }
}

fn token_ident(token: Option<&TokenTree>) -> Option<String> {
    let Some(TokenTree::Ident(ident)) = token else {
        return None;
    };
    Some(normalized_ident(ident))
}
