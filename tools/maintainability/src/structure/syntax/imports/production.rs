use anyhow::Result;
use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::fold::{self, Fold};
use syn::punctuated::Punctuated;
use syn::{Attribute, Expr, Field, FieldValue, FnArg, ForeignItem, GenericParam, ImplItem, Item, ItemImpl, Stmt, TraitItem, Variant};

use super::super::{
    ProductionCfgContext, expr_attributes, fn_arg_attributes, foreign_item_attributes, generic_param_attributes, impl_item_attributes, item_attributes, production_cfg_context,
    trait_item_attributes,
};

pub(super) fn production_item_tokens(item: &Item, cfg: &ProductionCfgContext) -> Result<TokenStream> {
    normalize(item.clone(), cfg, fold::fold_item)
}

#[cfg(test)]
pub(super) fn production_impl_tokens(item: &ItemImpl, cfg: &ProductionCfgContext) -> Result<TokenStream> {
    normalize(item.clone(), cfg, ProductionNormalizer::fold_item_impl)
}

pub(super) fn production_impl_item_tokens(item: &ImplItem, cfg: &ProductionCfgContext) -> Result<TokenStream> {
    normalize(item.clone(), cfg, fold::fold_impl_item)
}

pub(super) fn production_trait_item_tokens(item: &TraitItem, cfg: &ProductionCfgContext) -> Result<TokenStream> {
    normalize(item.clone(), cfg, fold::fold_trait_item)
}

pub(super) fn production_foreign_item_tokens(item: &ForeignItem, cfg: &ProductionCfgContext) -> Result<TokenStream> {
    normalize(item.clone(), cfg, fold::fold_foreign_item)
}

pub(super) fn production_stmt_tokens(stmt: &Stmt, cfg: &ProductionCfgContext) -> Result<TokenStream> {
    normalize(stmt.clone(), cfg, fold::fold_stmt)
}

fn normalize<T: ToTokens>(node: T, cfg: &ProductionCfgContext, fold: fn(&mut ProductionNormalizer, T) -> T) -> Result<TokenStream> {
    let mut normalizer = ProductionNormalizer { cfg: cfg.clone(), error: None };
    let folded = fold(&mut normalizer, node);
    normalizer.error.map_or_else(|| Ok(folded.to_token_stream()), Err)
}

struct ProductionNormalizer {
    cfg: ProductionCfgContext,
    error: Option<anyhow::Error>,
}

type AttributeExtractor<T> = fn(&T) -> Vec<Attribute>;
type NodeFolder<T> = fn(&mut ProductionNormalizer, T) -> T;

impl ProductionNormalizer {
    fn fold_active<T>(&mut self, node: T, attributes: Result<Vec<Attribute>>, fold: fn(&mut Self, T) -> T) -> Option<T> {
        if self.error.is_some() {
            return None;
        }
        let active = match attributes.and_then(|attributes| production_cfg_context(&attributes, &self.cfg)) {
            Ok(Some(active)) => active,
            Ok(None) => return None,
            Err(error) => {
                self.error = Some(error);
                return None;
            }
        };
        let previous = std::mem::replace(&mut self.cfg, active);
        let node = fold(self, node);
        self.cfg = previous;
        Some(node)
    }

    fn fold_items(&mut self, items: Vec<Item>) -> Vec<Item> {
        items
            .into_iter()
            .filter_map(|item| {
                let attributes = item_attributes(&item).map(<[Attribute]>::to_vec);
                self.fold_active(item, attributes, fold::fold_item)
            })
            .collect()
    }

    fn fold_stmts(&mut self, stmts: Vec<Stmt>) -> Vec<Stmt> {
        stmts
            .into_iter()
            .filter_map(|stmt| {
                let attributes = stmt_attributes(&stmt);
                self.fold_active(stmt, attributes, fold::fold_stmt)
            })
            .collect()
    }

    fn fold_impl_items(&mut self, items: Vec<ImplItem>) -> Vec<ImplItem> {
        items
            .into_iter()
            .filter_map(|item| {
                let attributes = impl_item_attributes(&item).map(<[Attribute]>::to_vec);
                self.fold_active(item, attributes, fold::fold_impl_item)
            })
            .collect()
    }

    fn fold_trait_items(&mut self, items: Vec<TraitItem>) -> Vec<TraitItem> {
        items
            .into_iter()
            .filter_map(|item| {
                let attributes = trait_item_attributes(&item).map(<[Attribute]>::to_vec);
                self.fold_active(item, attributes, fold::fold_trait_item)
            })
            .collect()
    }

    fn fold_foreign_items(&mut self, items: Vec<ForeignItem>) -> Vec<ForeignItem> {
        items
            .into_iter()
            .filter_map(|item| {
                let attributes = foreign_item_attributes(&item).map(<[Attribute]>::to_vec);
                self.fold_active(item, attributes, fold::fold_foreign_item)
            })
            .collect()
    }

    fn fold_punctuated<T, P>(&mut self, nodes: Punctuated<T, P>, attributes: AttributeExtractor<T>, fold: NodeFolder<T>) -> Punctuated<T, P>
    where
        P: Default,
    {
        nodes
            .into_iter()
            .filter_map(|node| {
                let attributes = Ok(attributes(&node));
                self.fold_active(node, attributes, fold)
            })
            .collect()
    }

    fn fold_exprs(&mut self, expressions: Punctuated<Expr, syn::Token![,]>) -> Punctuated<Expr, syn::Token![,]> {
        expressions
            .into_iter()
            .filter_map(|expression| {
                let attributes = expression_attributes(&expression);
                self.fold_active(expression, attributes, fold::fold_expr)
            })
            .collect()
    }
}

impl Fold for ProductionNormalizer {
    fn fold_block(&mut self, mut block: syn::Block) -> syn::Block {
        block.stmts = self.fold_stmts(block.stmts);
        block
    }

    fn fold_item_mod(&mut self, mut item: syn::ItemMod) -> syn::ItemMod {
        item.content = item.content.map(|(brace, items)| (brace, self.fold_items(items)));
        item
    }

    fn fold_item_impl(&mut self, mut item: ItemImpl) -> ItemImpl {
        item.generics = self.fold_generics(item.generics);
        item.trait_ = item.trait_.map(|(bang, path, for_token)| (bang, self.fold_path(path), for_token));
        item.self_ty = Box::new(self.fold_type(*item.self_ty));
        item.items = self.fold_impl_items(item.items);
        item
    }

    fn fold_item_trait(&mut self, mut item: syn::ItemTrait) -> syn::ItemTrait {
        item.generics = self.fold_generics(item.generics);
        item.supertraits = item.supertraits.into_iter().map(|bound| self.fold_type_param_bound(bound)).collect();
        item.items = self.fold_trait_items(item.items);
        item
    }

    fn fold_item_foreign_mod(&mut self, mut item: syn::ItemForeignMod) -> syn::ItemForeignMod {
        item.items = self.fold_foreign_items(item.items);
        item
    }

    fn fold_item_enum(&mut self, mut item: syn::ItemEnum) -> syn::ItemEnum {
        item.generics = self.fold_generics(item.generics);
        item.variants = self.fold_punctuated(item.variants, variant_attributes, fold::fold_variant);
        item
    }

    fn fold_fields_named(&mut self, mut fields: syn::FieldsNamed) -> syn::FieldsNamed {
        fields.named = self.fold_punctuated(fields.named, field_attributes, fold::fold_field);
        fields
    }

    fn fold_fields_unnamed(&mut self, mut fields: syn::FieldsUnnamed) -> syn::FieldsUnnamed {
        fields.unnamed = self.fold_punctuated(fields.unnamed, field_attributes, fold::fold_field);
        fields
    }

    fn fold_generics(&mut self, mut generics: syn::Generics) -> syn::Generics {
        generics.params = self.fold_punctuated(generics.params, generic_attributes, fold::fold_generic_param);
        generics.where_clause = generics.where_clause.map(|clause| self.fold_where_clause(clause));
        if generics.params.is_empty() {
            generics.lt_token = None;
            generics.gt_token = None;
        }
        generics
    }

    fn fold_signature(&mut self, mut signature: syn::Signature) -> syn::Signature {
        signature.generics = self.fold_generics(signature.generics);
        signature.inputs = self.fold_punctuated(signature.inputs, argument_attributes, fold::fold_fn_arg);
        signature.variadic = signature.variadic.and_then(|variadic| {
            let attributes = Ok(variadic.attrs.clone());
            self.fold_active(variadic, attributes, fold::fold_variadic)
        });
        signature.output = self.fold_return_type(signature.output);
        signature
    }

    fn fold_expr_array(&mut self, mut expression: syn::ExprArray) -> syn::ExprArray {
        expression.elems = self.fold_exprs(expression.elems);
        expression
    }

    fn fold_expr_call(&mut self, mut expression: syn::ExprCall) -> syn::ExprCall {
        expression.func = Box::new(self.fold_expr(*expression.func));
        expression.args = self.fold_exprs(expression.args);
        expression
    }

    fn fold_expr_method_call(&mut self, mut expression: syn::ExprMethodCall) -> syn::ExprMethodCall {
        expression.receiver = Box::new(self.fold_expr(*expression.receiver));
        expression.turbofish = expression.turbofish.map(|arguments| self.fold_angle_bracketed_generic_arguments(arguments));
        expression.args = self.fold_exprs(expression.args);
        expression
    }

    fn fold_expr_tuple(&mut self, mut expression: syn::ExprTuple) -> syn::ExprTuple {
        expression.elems = self.fold_exprs(expression.elems);
        expression
    }

    fn fold_expr_struct(&mut self, mut expression: syn::ExprStruct) -> syn::ExprStruct {
        expression.path = self.fold_path(expression.path);
        expression.fields = self.fold_punctuated(expression.fields, field_value_attributes, fold::fold_field_value);
        expression.rest = expression.rest.map(|rest| Box::new(self.fold_expr(*rest)));
        expression
    }

    fn fold_expr_match(&mut self, mut expression: syn::ExprMatch) -> syn::ExprMatch {
        expression.expr = Box::new(self.fold_expr(*expression.expr));
        expression.arms = expression
            .arms
            .into_iter()
            .filter_map(|arm| {
                let attributes = Ok(arm.attrs.clone());
                self.fold_active(arm, attributes, fold::fold_arm)
            })
            .collect();
        expression
    }
}

fn stmt_attributes(stmt: &Stmt) -> Result<Vec<Attribute>> {
    match stmt {
        Stmt::Local(local) => Ok(local.attrs.clone()),
        Stmt::Item(item) => item_attributes(item).map(<[Attribute]>::to_vec),
        Stmt::Expr(expression, _) => expression_attributes(expression),
        Stmt::Macro(statement) => Ok(statement.attrs.clone()),
    }
}

fn expression_attributes(expression: &Expr) -> Result<Vec<Attribute>> {
    expr_attributes(expression).map(<[Attribute]>::to_vec)
}

fn field_attributes(field: &Field) -> Vec<Attribute> {
    field.attrs.clone()
}

fn variant_attributes(variant: &Variant) -> Vec<Attribute> {
    variant.attrs.clone()
}

fn generic_attributes(parameter: &GenericParam) -> Vec<Attribute> {
    generic_param_attributes(parameter).to_vec()
}

fn argument_attributes(argument: &FnArg) -> Vec<Attribute> {
    fn_arg_attributes(argument).to_vec()
}

fn field_value_attributes(field: &FieldValue) -> Vec<Attribute> {
    field.attrs.clone()
}
