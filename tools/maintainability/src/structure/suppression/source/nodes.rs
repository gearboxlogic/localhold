use syn::{ForeignItem, ImplItem, Item, TraitItem};

use crate::scan::syntax_fingerprint;
use crate::structure::syntax::normalized_ident;

pub(super) fn item_scope(item: &Item) -> Option<(String, String)> {
    let (name, scope) = match item {
        Item::Const(item) => (normalized_ident(&item.ident), "item-const"),
        Item::Enum(item) => (normalized_ident(&item.ident), "item-enum"),
        Item::ExternCrate(item) => (normalized_ident(&item.ident), "item-extern-crate"),
        Item::Fn(item) => (normalized_ident(&item.sig.ident), "item-fn"),
        Item::ForeignMod(_) => ("extern".to_owned(), "item-foreign-mod"),
        Item::Impl(item) => (format!("impl:{}", syntax_fingerprint(&item.self_ty)), "item-impl"),
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
    Some((name, scope.to_owned()))
}

pub(super) fn impl_item_scope(item: &ImplItem) -> Option<(String, String)> {
    let (name, scope) = match item {
        ImplItem::Const(item) => (normalized_ident(&item.ident), "impl-const"),
        ImplItem::Fn(item) => (normalized_ident(&item.sig.ident), "impl-fn"),
        ImplItem::Type(item) => (normalized_ident(&item.ident), "impl-type"),
        ImplItem::Macro(_) | ImplItem::Verbatim(_) | _ => return None,
    };
    Some((name, scope.to_owned()))
}

pub(super) fn trait_item_scope(item: &TraitItem) -> Option<(String, String)> {
    let (name, scope) = match item {
        TraitItem::Const(item) => (normalized_ident(&item.ident), "trait-const"),
        TraitItem::Fn(item) => (normalized_ident(&item.sig.ident), "trait-fn"),
        TraitItem::Type(item) => (normalized_ident(&item.ident), "trait-type"),
        TraitItem::Macro(_) | TraitItem::Verbatim(_) | _ => return None,
    };
    Some((name, scope.to_owned()))
}

pub(super) fn foreign_item_scope(item: &ForeignItem) -> Option<(String, String)> {
    let (name, scope) = match item {
        ForeignItem::Fn(item) => (normalized_ident(&item.sig.ident), "foreign-fn"),
        ForeignItem::Static(item) => (normalized_ident(&item.ident), "foreign-static"),
        ForeignItem::Type(item) => (normalized_ident(&item.ident), "foreign-type"),
        ForeignItem::Macro(_) | ForeignItem::Verbatim(_) | _ => return None,
    };
    Some((name, scope.to_owned()))
}
