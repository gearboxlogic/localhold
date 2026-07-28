use anyhow::{Context, Result};
use proc_macro2::Span;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ForeignItem, ImplItem, Item, Meta, Token, TraitItem};

mod imports;

pub use imports::{ConcreteStoreCounts, ConcreteStoreSites, ProductionSyntaxFacts, ProductionSyntaxOptions, production_syntax_facts};

pub(super) fn normalized_ident(ident: &proc_macro2::Ident) -> String {
    let value = ident.to_string();
    value.strip_prefix("r#").unwrap_or(&value).to_owned()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Truth {
    AlwaysFalse,
    AlwaysTrue,
    Unknown,
}

pub struct TestLineCollector {
    test_lines: Vec<bool>,
    error: Option<anyhow::Error>,
}

impl TestLineCollector {
    pub fn new(physical_lines: usize) -> Self {
        Self {
            test_lines: vec![false; physical_lines],
            error: None,
        }
    }

    pub fn visit_file(&mut self, file: &syn::File) -> Result<()> {
        if !self.classify(Ok(&file.attrs), file) {
            Visit::visit_file(self, file);
        }
        self.error.take().map_or(Ok(()), Err)
    }

    pub fn test_line_count(&self) -> usize {
        self.test_lines.iter().filter(|line| **line).count()
    }

    fn classify<T: syn::spanned::Spanned>(&mut self, attributes: Result<&[Attribute]>, node: &T) -> bool {
        match attributes.and_then(|attributes| attributes_disable_production(attributes).map(|test_only| (attributes, test_only))) {
            Ok((attributes, true)) => {
                self.mark(attributes, node.span());
                true
            }
            Ok((_, false)) => false,
            Err(error) => {
                self.error = Some(error);
                true
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

impl<'ast> Visit<'ast> for TestLineCollector {
    fn visit_item(&mut self, node: &'ast Item) {
        if !self.classify(item_attributes(node), node) {
            visit::visit_item(self, node);
        }
    }

    fn visit_impl_item(&mut self, node: &'ast ImplItem) {
        if !self.classify(impl_item_attributes(node), node) {
            visit::visit_impl_item(self, node);
        }
    }

    fn visit_trait_item(&mut self, node: &'ast TraitItem) {
        if !self.classify(trait_item_attributes(node), node) {
            visit::visit_trait_item(self, node);
        }
    }

    fn visit_foreign_item(&mut self, node: &'ast ForeignItem) {
        if !self.classify(foreign_item_attributes(node), node) {
            visit::visit_foreign_item(self, node);
        }
    }

    fn visit_variant(&mut self, node: &'ast syn::Variant) {
        if !self.classify(Ok(&node.attrs), node) {
            visit::visit_variant(self, node);
        }
    }

    fn visit_field(&mut self, node: &'ast syn::Field) {
        if !self.classify(Ok(&node.attrs), node) {
            visit::visit_field(self, node);
        }
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        if !self.classify(Ok(&node.attrs), node) {
            visit::visit_arm(self, node);
        }
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        if !self.classify(Ok(&node.attrs), node) {
            visit::visit_local(self, node);
        }
    }

    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        if !self.classify(Ok(&node.attrs), node) {
            visit::visit_stmt_macro(self, node);
        }
    }

    fn visit_expr(&mut self, node: &'ast Expr) {
        if !self.classify(expr_attributes(node), node) {
            visit::visit_expr(self, node);
        }
    }

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

pub(super) fn attributes_disable_production(attributes: &[Attribute]) -> Result<bool> {
    for attribute in attributes {
        if attribute.path().is_ident("cfg") {
            let predicate = parse_single_meta(attribute).context("parse cfg predicate for line classification")?;
            if evaluate(&predicate) == Truth::AlwaysFalse {
                return Ok(true);
            }
        } else if attribute.path().is_ident("cfg_attr") && cfg_attr_disables_production(attribute)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cfg_attr_disables_production(attribute: &Attribute) -> Result<bool> {
    let Meta::List(list) = &attribute.meta else {
        return Ok(false);
    };
    cfg_attr_tokens_disable_production(&list.tokens)
}

fn cfg_attr_tokens_disable_production(tokens: &proc_macro2::TokenStream) -> Result<bool> {
    let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(tokens.clone())
        .context("parse cfg_attr arguments for line classification")?;
    let mut arguments = arguments.into_iter();
    let Some(condition) = arguments.next() else {
        return Ok(false);
    };
    if evaluate(&condition) != Truth::AlwaysTrue {
        return Ok(false);
    }
    for nested in arguments {
        if nested.path().is_ident("cfg") {
            let Meta::List(list) = nested else {
                anyhow::bail!("nested cfg predicate must use list syntax")
            };
            let predicate = Punctuated::<Meta, Token![,]>::parse_terminated
                .parse2(list.tokens)
                .context("parse nested cfg_attr cfg predicate")?;
            if predicate.len() != 1 {
                anyhow::bail!("nested cfg attribute must contain exactly one predicate");
            }
            if evaluate(predicate.first().context("nested cfg predicate disappeared")?) == Truth::AlwaysFalse {
                return Ok(true);
            }
        } else if nested.path().is_ident("cfg_attr")
            && let Meta::List(list) = nested
            && cfg_attr_tokens_disable_production(&list.tokens)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn parse_single_meta(attribute: &Attribute) -> Result<Meta> {
    let Meta::List(list) = &attribute.meta else {
        return Ok(attribute.meta.clone());
    };
    let predicates = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone())?;
    if predicates.len() != 1 {
        anyhow::bail!("cfg attribute must contain exactly one predicate");
    }
    predicates.into_iter().next().context("cfg predicate disappeared")
}

fn evaluate(meta: &Meta) -> Truth {
    match meta {
        Meta::Path(path) if path.is_ident("test") => Truth::AlwaysFalse,
        Meta::Path(_) | Meta::NameValue(_) => evaluate_leaf(meta),
        Meta::List(list) if list.path.is_ident("all") => evaluate_list(list, combine_all, Truth::AlwaysTrue),
        Meta::List(list) if list.path.is_ident("any") => evaluate_list(list, combine_any, Truth::AlwaysFalse),
        Meta::List(list) if list.path.is_ident("not") => {
            let Ok(arguments) = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone()) else {
                return Truth::Unknown;
            };
            if arguments.len() != 1 {
                return Truth::Unknown;
            }
            match evaluate(arguments.first().expect("length checked")) {
                Truth::AlwaysFalse => Truth::AlwaysTrue,
                Truth::AlwaysTrue => Truth::AlwaysFalse,
                Truth::Unknown => Truth::Unknown,
            }
        }
        Meta::List(_) => Truth::Unknown,
    }
}

pub(super) fn cfg_can_apply_in_production(meta: &Meta) -> bool {
    evaluate(meta) != Truth::AlwaysFalse
}

fn evaluate_leaf(meta: &Meta) -> Truth {
    let Meta::NameValue(value) = meta else {
        return Truth::Unknown;
    };
    if !value.path.is_ident("feature") {
        return Truth::Unknown;
    }
    let Expr::Lit(expression) = &value.value else {
        return Truth::Unknown;
    };
    let syn::Lit::Str(feature) = &expression.lit else {
        return Truth::Unknown;
    };
    if feature.value() == "testing" { Truth::AlwaysFalse } else { Truth::Unknown }
}

fn evaluate_list(list: &syn::MetaList, combine: fn(Truth, Truth) -> Truth, initial: Truth) -> Truth {
    let Ok(arguments) = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone()) else {
        return Truth::Unknown;
    };
    arguments.iter().map(evaluate).fold(initial, combine)
}

const fn combine_all(left: Truth, right: Truth) -> Truth {
    match (left, right) {
        (Truth::AlwaysFalse, _) | (_, Truth::AlwaysFalse) => Truth::AlwaysFalse,
        (Truth::AlwaysTrue, Truth::AlwaysTrue) => Truth::AlwaysTrue,
        _ => Truth::Unknown,
    }
}

const fn combine_any(left: Truth, right: Truth) -> Truth {
    match (left, right) {
        (Truth::AlwaysTrue, _) | (_, Truth::AlwaysTrue) => Truth::AlwaysTrue,
        (Truth::AlwaysFalse, Truth::AlwaysFalse) => Truth::AlwaysFalse,
        _ => Truth::Unknown,
    }
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
