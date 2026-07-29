use anyhow::{Context, Result};
use proc_macro2::Span;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ForeignItem, ImplItem, Item, Meta, Token, TraitItem};

mod cfg;
mod imports;

pub(super) use cfg::{ProductionCfgContext, attributes_disable_production, production_cfg_attr_metas, production_cfg_context};
pub use imports::{
    ConcreteStoreCounts, ConcreteStoreSignatureSite, ConcreteStoreSignatureSites, ConcreteStoreSites, ProductionSyntaxFacts, ProductionSyntaxOptions, PublicReexportEvidence,
    TypeDeclarationEvidence,
};
pub(super) use imports::{ProductionAncestorPath, ProductionSyntaxContext, production_syntax_facts_with_context, source_module};

pub(super) fn normalized_ident(ident: &proc_macro2::Ident) -> String {
    let value = ident.to_string();
    value.strip_prefix("r#").unwrap_or(&value).to_owned()
}

pub(super) fn visibility_is_exposed(visibility: &syn::Visibility) -> bool {
    match visibility {
        syn::Visibility::Inherited => false,
        syn::Visibility::Restricted(restricted) => !restricted.path.is_ident("self"),
        syn::Visibility::Public(_) => true,
    }
}

pub struct TestLineCollector {
    test_lines: Vec<bool>,
    cfg_context: ProductionCfgContext,
    error: Option<anyhow::Error>,
}

impl TestLineCollector {
    #[cfg(test)]
    pub fn new(physical_lines: usize) -> Self {
        Self::with_cfg_context(physical_lines, ProductionCfgContext::default())
    }

    pub(super) fn with_cfg_context(physical_lines: usize, cfg_context: ProductionCfgContext) -> Self {
        Self {
            test_lines: vec![false; physical_lines],
            cfg_context,
            error: None,
        }
    }

    pub fn visit_file(&mut self, file: &syn::File) -> Result<()> {
        if let Some(previous) = self.enter_node(Ok(&file.attrs), file) {
            Visit::visit_file(self, file);
            self.cfg_context = previous;
        }
        self.error.take().map_or(Ok(()), Err)
    }

    pub fn test_line_count(&self) -> usize {
        self.test_lines.iter().filter(|line| **line).count()
    }

    fn enter_node<T: syn::spanned::Spanned>(&mut self, attributes: Result<&[Attribute]>, node: &T) -> Option<ProductionCfgContext> {
        match attributes.and_then(|attributes| production_cfg_context(attributes, &self.cfg_context).map(|context| (attributes, context))) {
            Ok((attributes, None)) => {
                self.mark(attributes, node.span());
                None
            }
            Ok((_, Some(context))) => Some(std::mem::replace(&mut self.cfg_context, context)),
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }

    fn mark(&mut self, attributes: &[Attribute], node: Span) {
        let node_start = node.start();
        let start = attributes.iter().map(|attribute| attribute.span().start()).min().unwrap_or(node_start);
        let end = node.end();
        if start.line == 0 || end.line == 0 || self.test_lines.is_empty() {
            return;
        }
        let end_line = if end.column == 0 && end.line > start.line { end.line - 1 } else { end.line };
        let start_index = start.line.saturating_sub(1).min(self.test_lines.len());
        let end_index = end_line.min(self.test_lines.len());
        for line in &mut self.test_lines[start_index..end_index] {
            *line = true;
        }
    }
}

macro_rules! visit_classified_node {
    ($method:ident, $walk:ident, $node:ty, $binding:ident => $attributes:expr) => {
        fn $method(&mut self, $binding: &'ast $node) {
            let attributes: Result<&[Attribute]> = $attributes;
            let Some(previous) = self.enter_node(attributes, $binding) else {
                return;
            };
            visit::$walk(self, $binding);
            self.cfg_context = previous;
        }
    };
}

impl<'ast> Visit<'ast> for TestLineCollector {
    visit_classified_node!(visit_item, visit_item, Item, node => item_attributes(node));
    visit_classified_node!(visit_impl_item, visit_impl_item, ImplItem, node => impl_item_attributes(node));
    visit_classified_node!(visit_trait_item, visit_trait_item, TraitItem, node => trait_item_attributes(node));
    visit_classified_node!(visit_foreign_item, visit_foreign_item, ForeignItem, node => foreign_item_attributes(node));
    visit_classified_node!(visit_variant, visit_variant, syn::Variant, node => Ok(&node.attrs));
    visit_classified_node!(visit_field, visit_field, syn::Field, node => Ok(&node.attrs));
    visit_classified_node!(visit_arm, visit_arm, syn::Arm, node => Ok(&node.attrs));
    visit_classified_node!(visit_local, visit_local, syn::Local, node => Ok(&node.attrs));
    visit_classified_node!(visit_stmt_macro, visit_stmt_macro, syn::StmtMacro, node => Ok(&node.attrs));
    visit_classified_node!(visit_expr, visit_expr, Expr, node => expr_attributes(node));

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if node.path.is_ident("include") {
            self.error = Some(anyhow::anyhow!("Rust source include! cannot be classified safely"));
            return;
        }
        visit::visit_macro(self, node);
    }
}

pub fn item_is_test_only(item: &Item) -> Result<bool> {
    attributes_disable_production(item_attributes(item)?)
}

pub fn reject_module_path_overrides(attributes: &[Attribute]) -> Result<()> {
    for attribute in attributes {
        if attribute.path().is_ident("path") {
            anyhow::bail!("explicit Rust module paths cannot be classified safely");
        }
        if attribute.path().is_ident("cfg_attr") && cfg_attr_contains_path(attribute)? {
            anyhow::bail!("conditional Rust module paths cannot be classified safely");
        }
    }
    Ok(())
}

fn cfg_attr_contains_path(attribute: &Attribute) -> Result<bool> {
    let Meta::List(list) = &attribute.meta else {
        return Ok(false);
    };
    let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .context("parse cfg_attr arguments for module path classification")?;
    arguments.iter().skip(1).try_fold(false, |found, nested| Ok(found || meta_contains_path(nested)?))
}

fn meta_contains_path(meta: &Meta) -> Result<bool> {
    if meta.path().is_ident("path") {
        return Ok(true);
    }
    if !meta.path().is_ident("cfg_attr") {
        return Ok(false);
    }
    let Meta::List(list) = meta else {
        return Ok(false);
    };
    let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .context("parse nested cfg_attr arguments for module path classification")?;
    arguments.iter().skip(1).try_fold(false, |found, nested| Ok(found || meta_contains_path(nested)?))
}

pub(super) fn item_attributes(item: &Item) -> Result<&[Attribute]> {
    Ok(match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) => anyhow::bail!("opaque item syntax cannot be classified"),
        _ => anyhow::bail!("unsupported item syntax cannot be classified"),
    })
}

pub(super) fn impl_item_attributes(item: &ImplItem) -> Result<&[Attribute]> {
    Ok(match item {
        ImplItem::Const(item) => &item.attrs,
        ImplItem::Fn(item) => &item.attrs,
        ImplItem::Macro(item) => &item.attrs,
        ImplItem::Type(item) => &item.attrs,
        ImplItem::Verbatim(_) => anyhow::bail!("opaque impl-item syntax cannot be classified"),
        _ => anyhow::bail!("unsupported impl-item syntax cannot be classified"),
    })
}

pub(super) fn trait_item_attributes(item: &TraitItem) -> Result<&[Attribute]> {
    Ok(match item {
        TraitItem::Const(item) => &item.attrs,
        TraitItem::Fn(item) => &item.attrs,
        TraitItem::Macro(item) => &item.attrs,
        TraitItem::Type(item) => &item.attrs,
        TraitItem::Verbatim(_) => anyhow::bail!("opaque trait-item syntax cannot be classified"),
        _ => anyhow::bail!("unsupported trait-item syntax cannot be classified"),
    })
}

pub(super) fn foreign_item_attributes(item: &ForeignItem) -> Result<&[Attribute]> {
    Ok(match item {
        ForeignItem::Fn(item) => &item.attrs,
        ForeignItem::Macro(item) => &item.attrs,
        ForeignItem::Static(item) => &item.attrs,
        ForeignItem::Type(item) => &item.attrs,
        ForeignItem::Verbatim(_) => anyhow::bail!("opaque foreign-item syntax cannot be classified"),
        _ => anyhow::bail!("unsupported foreign-item syntax cannot be classified"),
    })
}

pub(super) fn fn_arg_attributes(argument: &syn::FnArg) -> &[Attribute] {
    match argument {
        syn::FnArg::Receiver(argument) => &argument.attrs,
        syn::FnArg::Typed(argument) => &argument.attrs,
    }
}

pub(super) fn generic_param_attributes(parameter: &syn::GenericParam) -> &[Attribute] {
    match parameter {
        syn::GenericParam::Lifetime(parameter) => &parameter.attrs,
        syn::GenericParam::Type(parameter) => &parameter.attrs,
        syn::GenericParam::Const(parameter) => &parameter.attrs,
    }
}

pub(super) fn pat_attributes(pattern: &syn::Pat) -> Result<&[Attribute]> {
    Ok(match pattern {
        syn::Pat::Const(pattern) => &pattern.attrs,
        syn::Pat::Ident(pattern) => &pattern.attrs,
        syn::Pat::Lit(pattern) => &pattern.attrs,
        syn::Pat::Macro(pattern) => &pattern.attrs,
        syn::Pat::Or(pattern) => &pattern.attrs,
        syn::Pat::Paren(pattern) => &pattern.attrs,
        syn::Pat::Path(pattern) => &pattern.attrs,
        syn::Pat::Range(pattern) => &pattern.attrs,
        syn::Pat::Reference(pattern) => &pattern.attrs,
        syn::Pat::Rest(pattern) => &pattern.attrs,
        syn::Pat::Slice(pattern) => &pattern.attrs,
        syn::Pat::Struct(pattern) => &pattern.attrs,
        syn::Pat::Tuple(pattern) => &pattern.attrs,
        syn::Pat::TupleStruct(pattern) => &pattern.attrs,
        syn::Pat::Type(pattern) => &pattern.attrs,
        syn::Pat::Verbatim(_) => anyhow::bail!("opaque pattern syntax cannot be classified"),
        syn::Pat::Wild(pattern) => &pattern.attrs,
        _ => anyhow::bail!("unsupported pattern syntax cannot be classified"),
    })
}

pub(super) fn expr_attributes(expression: &Expr) -> Result<&[Attribute]> {
    Ok(match expression {
        Expr::Array(value) => &value.attrs,
        Expr::Assign(value) => &value.attrs,
        Expr::Async(value) => &value.attrs,
        Expr::Await(value) => &value.attrs,
        Expr::Binary(value) => &value.attrs,
        Expr::Block(value) => &value.attrs,
        Expr::Break(value) => &value.attrs,
        Expr::Call(value) => &value.attrs,
        Expr::Cast(value) => &value.attrs,
        Expr::Closure(value) => &value.attrs,
        Expr::Const(value) => &value.attrs,
        Expr::Continue(value) => &value.attrs,
        Expr::Field(value) => &value.attrs,
        Expr::ForLoop(value) => &value.attrs,
        Expr::Group(value) => &value.attrs,
        Expr::If(value) => &value.attrs,
        Expr::Index(value) => &value.attrs,
        Expr::Infer(value) => &value.attrs,
        Expr::Let(value) => &value.attrs,
        Expr::Lit(value) => &value.attrs,
        Expr::Loop(value) => &value.attrs,
        Expr::Macro(value) => &value.attrs,
        Expr::Match(value) => &value.attrs,
        Expr::MethodCall(value) => &value.attrs,
        Expr::Paren(value) => &value.attrs,
        Expr::Path(value) => &value.attrs,
        Expr::Range(value) => &value.attrs,
        Expr::RawAddr(value) => &value.attrs,
        Expr::Reference(value) => &value.attrs,
        Expr::Repeat(value) => &value.attrs,
        Expr::Return(value) => &value.attrs,
        Expr::Struct(value) => &value.attrs,
        Expr::Try(value) => &value.attrs,
        Expr::TryBlock(value) => &value.attrs,
        Expr::Tuple(value) => &value.attrs,
        Expr::Unary(value) => &value.attrs,
        Expr::Unsafe(value) => &value.attrs,
        Expr::While(value) => &value.attrs,
        Expr::Yield(value) => &value.attrs,
        Expr::Verbatim(_) => anyhow::bail!("opaque expression syntax cannot be classified"),
        _ => anyhow::bail!("unsupported expression syntax cannot be classified"),
    })
}
