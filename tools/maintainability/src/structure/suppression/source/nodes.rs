use quote::ToTokens as _;
use syn::{ForeignItem, ImplItem, Item, TraitItem};

use crate::scan::syntax_fingerprint;
use crate::structure::syntax::normalized_ident;

pub(super) struct SuppressionScope {
    pub(super) item: String,
    pub(super) scope: String,
    pub(super) signature: Option<String>,
}

pub(super) fn item_scope(item: &Item) -> Option<SuppressionScope> {
    let (name, scope) = match item {
        Item::Const(item) => (normalized_ident(&item.ident), "item-const"),
        Item::Enum(item) => (normalized_ident(&item.ident), "item-enum"),
        Item::ExternCrate(item) => (normalized_ident(&item.ident), "item-extern-crate"),
        Item::Fn(item) => return Some(function_scope(&item.sig, "item-fn")),
        Item::ForeignMod(item) => {
            let mut header = item.clone();
            header.items.clear();
            (format!("extern:{}", syntax_fingerprint(&header)), "item-foreign-mod")
        }
        Item::Impl(item) => {
            let mut header = item.clone();
            header.items.clear();
            (format!("impl:{}", syntax_fingerprint(&header.to_token_stream())), "item-impl")
        }
        Item::Macro(item) => (item.ident.as_ref().map_or_else(|| "<macro>".to_owned(), normalized_ident), "item-macro"),
        Item::Mod(item) => (normalized_ident(&item.ident), "item-mod"),
        Item::Static(item) => (normalized_ident(&item.ident), "item-static"),
        Item::Struct(item) => (normalized_ident(&item.ident), "item-struct"),
        Item::Trait(item) => (normalized_ident(&item.ident), "item-trait"),
        Item::TraitAlias(item) => (normalized_ident(&item.ident), "item-trait-alias"),
        Item::Type(item) => (normalized_ident(&item.ident), "item-type"),
        Item::Union(item) => (normalized_ident(&item.ident), "item-union"),
        Item::Use(_) | Item::Verbatim(_) | _ => return None,
    };
    Some(SuppressionScope {
        item: name,
        scope: scope.to_owned(),
        signature: None,
    })
}

pub(super) fn impl_item_scope(item: &ImplItem) -> Option<SuppressionScope> {
    let (name, scope) = match item {
        ImplItem::Const(item) => (normalized_ident(&item.ident), "impl-const"),
        ImplItem::Fn(item) => return Some(function_scope(&item.sig, "impl-fn")),
        ImplItem::Type(item) => (normalized_ident(&item.ident), "impl-type"),
        ImplItem::Macro(_) | ImplItem::Verbatim(_) | _ => return None,
    };
    Some(SuppressionScope {
        item: name,
        scope: scope.to_owned(),
        signature: None,
    })
}

pub(super) fn trait_item_scope(item: &TraitItem) -> Option<SuppressionScope> {
    let (name, scope) = match item {
        TraitItem::Const(item) => (normalized_ident(&item.ident), "trait-const"),
        TraitItem::Fn(item) => return Some(function_scope(&item.sig, "trait-fn")),
        TraitItem::Type(item) => (normalized_ident(&item.ident), "trait-type"),
        TraitItem::Macro(_) | TraitItem::Verbatim(_) | _ => return None,
    };
    Some(SuppressionScope {
        item: name,
        scope: scope.to_owned(),
        signature: None,
    })
}

pub(super) fn foreign_item_scope(item: &ForeignItem) -> Option<SuppressionScope> {
    let (name, scope) = match item {
        ForeignItem::Fn(item) => return Some(function_scope(&item.sig, "foreign-fn")),
        ForeignItem::Static(item) => (normalized_ident(&item.ident), "foreign-static"),
        ForeignItem::Type(item) => (normalized_ident(&item.ident), "foreign-type"),
        ForeignItem::Macro(_) | ForeignItem::Verbatim(_) | _ => return None,
    };
    Some(SuppressionScope {
        item: name,
        scope: scope.to_owned(),
        signature: None,
    })
}

fn function_scope(signature: &syn::Signature, scope: &str) -> SuppressionScope {
    SuppressionScope {
        item: normalized_ident(&signature.ident),
        scope: scope.to_owned(),
        signature: Some(syntax_fingerprint(signature)),
    }
}
