use std::path::Path as FsPath;

use anyhow::{Context, Result, bail};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens as _;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Arm, Attribute, BareFnArg, BareVariadic, Expr, Field, FieldPat, FieldValue, File, FnArg, ForeignItem, GenericParam, ImplItem, Item, ItemExternCrate, ItemMod, ItemType,
    ItemUse, Local, Meta, Pat, Path as SynPath, StmtMacro, Token, TraitItem, UseTree, Variadic, Variant, Visibility,
};

use crate::scan::{reviewed_attribute_expansion, reviewed_macro_expansion};

use super::{
    attributes_disable_production, cfg_can_apply_in_production, expr_attributes, fn_arg_attributes, foreign_item_attributes, generic_param_attributes, impl_item_attributes,
    item_is_test_only, normalized_ident, pat_attributes, trait_item_attributes,
};

pub fn production_internal_imports(
    file: &syn::File,
    source_path: &str,
    crate_root: Option<&str>,
    rust_2015_absolute_paths: bool,
    require_reviewed_expansions: bool,
) -> Result<Vec<String>> {
    let module = source_module(source_path, crate_root)?;
    let mut collector = ImportCollector {
        module,
        imports: Vec::new(),
        error: None,
        rust_2015_absolute_paths,
        require_reviewed_expansions,
    };
    collector.visit_file(file);
    if let Some(error) = collector.error {
        return Err(error);
    }
    collector.imports.sort();
    collector.imports.dedup();
    Ok(collector.imports)
}

struct ImportCollector {
    module: Vec<String>,
    imports: Vec<String>,
    error: Option<anyhow::Error>,
    rust_2015_absolute_paths: bool,
    require_reviewed_expansions: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum StringScan {
    Skip,
    RustFragment,
    GovernedAttribute,
}

impl ImportCollector {
    fn collect_use(&mut self, item: &ItemUse) -> Result<()> {
        if item.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(());
        }
        let import_count = self.imports.len();
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for mut path in paths {
            if item.leading_colon.is_some() {
                path.segments.insert(0, "crate".to_owned());
            }
            self.collect_path(&path)?;
        }
        if self.imports.len() != import_count && !matches!(item.vis, Visibility::Inherited) {
            bail!("production restricted imports cannot be re-exported");
        }
        Ok(())
    }

    fn collect_path(&mut self, path: &UsePath) -> Result<()> {
        self.collect_segments(&path.segments, path.renamed, self.rust_2015_absolute_paths)
    }

    fn collect_segments(&mut self, segments: &[String], renamed: bool, rust_2015_use_path: bool) -> Result<()> {
        let Some(resolved) = resolve_path(&self.module, segments, rust_2015_use_path)? else {
            return Ok(());
        };
        if resolved.is_empty() && renamed {
            bail!("production crate-root import aliases cannot be classified safely for dependency boundaries");
        }
        if resolved.as_slice() == ["*"] {
            bail!("production crate-root glob imports cannot be classified safely for dependency boundaries");
        }
        if matches!(resolved.first().map(String::as_str), Some("server" | "ui")) {
            self.imports.push(format!("crate::{}", resolved.join("::")));
        }
        Ok(())
    }

    fn skip_test_only(&mut self, test_only: Result<bool>) -> bool {
        if self.error.is_some() {
            return true;
        }
        match test_only {
            Ok(test_only) => test_only,
            Err(error) => {
                self.error = Some(error);
                true
            }
        }
    }
}

macro_rules! visit_production_node {
    ($method:ident, $walk:ident, $node:ty, $binding:ident => $test_only:expr) => {
        fn $method(&mut self, $binding: &'ast $node) {
            let test_only: Result<bool> = $test_only;
            if !self.skip_test_only(test_only) {
                visit::$walk(self, $binding);
            }
        }
    };
}

impl<'ast> Visit<'ast> for ImportCollector {
    visit_production_node!(visit_file, visit_file, File, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_item, visit_item, Item, node => item_is_test_only(node));
    visit_production_node!(
        visit_impl_item,
        visit_impl_item,
        ImplItem,
        node =>
        impl_item_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(
        visit_trait_item,
        visit_trait_item,
        TraitItem,
        node =>
        trait_item_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(
        visit_foreign_item,
        visit_foreign_item,
        ForeignItem,
        node =>
        foreign_item_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(visit_variant, visit_variant, Variant, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_field, visit_field, Field, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_arm, visit_arm, Arm, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_local, visit_local, Local, node => attributes_disable_production(&node.attrs));
    visit_production_node!(visit_stmt_macro, visit_stmt_macro, StmtMacro, node => attributes_disable_production(&node.attrs));
    visit_production_node!(
        visit_expr,
        visit_expr,
        Expr,
        node => expr_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(
        visit_fn_arg,
        visit_fn_arg,
        FnArg,
        node => attributes_disable_production(fn_arg_attributes(node))
    );
    visit_production_node!(
        visit_generic_param,
        visit_generic_param,
        GenericParam,
        node => attributes_disable_production(generic_param_attributes(node))
    );
    visit_production_node!(
        visit_pat,
        visit_pat,
        Pat,
        node => pat_attributes(node).and_then(attributes_disable_production)
    );
    visit_production_node!(
        visit_bare_fn_arg,
        visit_bare_fn_arg,
        BareFnArg,
        node => attributes_disable_production(&node.attrs)
    );
    visit_production_node!(
        visit_bare_variadic,
        visit_bare_variadic,
        BareVariadic,
        node => attributes_disable_production(&node.attrs)
    );
    visit_production_node!(
        visit_variadic,
        visit_variadic,
        Variadic,
        node => attributes_disable_production(&node.attrs)
    );
    visit_production_node!(
        visit_field_pat,
        visit_field_pat,
        FieldPat,
        node => attributes_disable_production(&node.attrs)
    );
    visit_production_node!(
        visit_field_value,
        visit_field_value,
        FieldValue,
        node => attributes_disable_production(&node.attrs)
    );

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        for attribute in &item.attrs {
            self.visit_attribute(attribute);
        }
        if self.error.is_none()
            && let Err(error) = self.collect_use(item)
        {
            self.error = Some(error);
        }
    }

    fn visit_item_type(&mut self, item: &'ast ItemType) {
        let import_count = self.imports.len();
        visit::visit_item_type(self, item);
        if self.error.is_none() && self.imports.len() != import_count && !matches!(item.vis, Visibility::Inherited) {
            self.error = Some(anyhow::anyhow!("production restricted imports cannot be exposed through public type aliases"));
        }
    }

    fn visit_item_extern_crate(&mut self, item: &'ast ItemExternCrate) {
        for attribute in &item.attrs {
            self.visit_attribute(attribute);
        }
        if self.error.is_none() && item.ident == "self" && item.rename.is_some() {
            self.error = Some(anyhow::anyhow!(
                "production crate-root extern aliases cannot be classified safely for dependency boundaries"
            ));
        }
    }

    fn visit_path(&mut self, path: &'ast SynPath) {
        if self.error.is_none() && (path.leading_colon.is_none() || self.rust_2015_absolute_paths) {
            let mut segments = path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
            if path.leading_colon.is_some() {
                segments.insert(0, "crate".to_owned());
            }
            let is_qualified = segments.len() > 1 || matches!(segments.first().map(String::as_str), Some("crate" | "self" | "super"));
            if is_qualified && let Err(error) = self.collect_segments(&segments, false, false) {
                self.error = Some(error);
                return;
            }
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if self.error.is_some() {
            return;
        }
        match restricted_token_identifier(&node.tokens, &self.module, self.rust_2015_absolute_paths, StringScan::RustFragment) {
            Ok(Some(restricted)) => {
                self.error = Some(anyhow::anyhow!(
                    "production macro token stream names restricted crate module {restricted:?} and cannot be classified safely"
                ));
                return;
            }
            Ok(None) => {}
            Err(error) => {
                self.error = Some(error);
                return;
            }
        }
        if self.require_reviewed_expansions && !reviewed_macro_expansion(node) {
            self.error = Some(anyhow::anyhow!("production code invokes unreviewed macro expansion path {}", node.path.to_token_stream()));
            return;
        }
        self.visit_path(&node.path);
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if self.error.is_some() {
            return;
        }
        match restricted_attribute_identifier(attribute, &self.module, self.rust_2015_absolute_paths) {
            Ok(Some(restricted)) => {
                self.error = Some(anyhow::anyhow!(
                    "production attribute token stream names restricted crate module {restricted:?} and cannot be classified safely"
                ));
                return;
            }
            Ok(None) => {}
            Err(error) => {
                self.error = Some(error);
                return;
            }
        }
        if self.require_reviewed_expansions && !reviewed_attribute_expansion(attribute) {
            self.error = Some(anyhow::anyhow!("production code uses unreviewed attribute expansion {}", attribute.meta.to_token_stream()));
            return;
        }
        visit::visit_attribute(self, attribute);
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        for attribute in &item.attrs {
            self.visit_attribute(attribute);
        }
        if self.error.is_some() {
            return;
        }
        let Some((_, items)) = &item.content else {
            return;
        };
        self.module.push(normalized_ident(&item.ident));
        for nested in items {
            self.visit_item(nested);
        }
        self.module.pop();
    }
}

struct UsePath {
    segments: Vec<String>,
    renamed: bool,
}

fn flatten_use_tree(tree: &UseTree, prefix: &mut Vec<String>, paths: &mut Vec<UsePath>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(normalized_ident(&path.ident));
            flatten_use_tree(&path.tree, prefix, paths);
            prefix.pop();
        }
        UseTree::Name(name) => {
            let name = normalized_ident(&name.ident);
            let mut segments = prefix.clone();
            if name != "self" {
                segments.push(name);
            }
            paths.push(UsePath { segments, renamed: false });
        }
        UseTree::Rename(rename) => {
            let name = normalized_ident(&rename.ident);
            let mut segments = prefix.clone();
            if name != "self" {
                segments.push(name);
            }
            paths.push(UsePath { segments, renamed: true });
        }
        UseTree::Glob(_) => {
            let mut segments = prefix.clone();
            segments.push("*".to_owned());
            paths.push(UsePath { segments, renamed: false });
        }
        UseTree::Group(group) => {
            for nested in &group.items {
                flatten_use_tree(nested, prefix, paths);
            }
        }
    }
}

fn resolve_path(module: &[String], path: &[String], rust_2015_use_path: bool) -> Result<Option<Vec<String>>> {
    let Some(first) = path.first().map(String::as_str) else {
        bail!("production use path has no segments");
    };
    if first == "crate" {
        return Ok(Some(path[1..].to_vec()));
    }
    if first == "self" {
        let mut resolved = module.to_vec();
        resolved.extend_from_slice(&path[1..]);
        return Ok(Some(resolved));
    }
    if first == "super" {
        let mut resolved = module.to_vec();
        let mut consumed = 0;
        while path.get(consumed).is_some_and(|segment| segment == "super") {
            resolved.pop().context("production use path escapes its crate root")?;
            consumed += 1;
        }
        resolved.extend_from_slice(&path[consumed..]);
        return Ok(Some(resolved));
    }
    Ok((rust_2015_use_path || module.is_empty()).then(|| path.to_vec()))
}

fn source_module(source_path: &str, crate_root: Option<&str>) -> Result<Vec<String>> {
    if crate_root == Some(source_path) {
        return Ok(Vec::new());
    }
    let root_directory = match crate_root {
        Some(root) => FsPath::new(root).parent().context("Cargo library target has no parent directory")?,
        None => FsPath::new("src"),
    };
    let relative = FsPath::new(source_path)
        .strip_prefix(root_directory)
        .context("production internal import source is outside its Cargo library root")?;
    let mut parts = relative
        .parent()
        .into_iter()
        .flat_map(FsPath::components)
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let file = relative
        .file_name()
        .and_then(|file| file.to_str())
        .context("production Rust source has no UTF-8 filename")?;
    let stem = file.strip_suffix(".rs").context("production source path is not a Rust file")?;
    if stem != "mod" {
        parts.push(stem.to_owned());
    }
    Ok(parts)
}

fn restricted_token_identifier(tokens: &TokenStream, module: &[String], rust_2015_absolute_paths: bool, string_scan: StringScan) -> Result<Option<String>> {
    for token in tokens.clone() {
        let restricted = match token {
            TokenTree::Group(group) => restricted_token_identifier(&group.stream(), module, rust_2015_absolute_paths, string_scan)?,
            TokenTree::Ident(ident) => {
                let normalized = normalized_ident(&ident);
                matches!(normalized.as_str(), "server" | "ui").then_some(normalized)
            }
            TokenTree::Literal(literal) if string_scan != StringScan::Skip => {
                restricted_string_path(&literal, module, rust_2015_absolute_paths, string_scan == StringScan::GovernedAttribute)?
            }
            TokenTree::Literal(_) | TokenTree::Punct(_) => None,
        };
        if restricted.is_some() {
            return Ok(restricted);
        }
    }
    Ok(None)
}

fn restricted_attribute_identifier(attribute: &Attribute, module: &[String], rust_2015_absolute_paths: bool) -> Result<Option<String>> {
    if !attribute.path().is_ident("cfg_attr") {
        return restricted_meta_contents(&attribute.meta, module, rust_2015_absolute_paths);
    }
    let Meta::List(list) = &attribute.meta else {
        return Ok(None);
    };
    restricted_cfg_attr_contents(&list.tokens, module, rust_2015_absolute_paths)
}

fn restricted_cfg_attr_contents(tokens: &TokenStream, module: &[String], rust_2015_absolute_paths: bool) -> Result<Option<String>> {
    let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(tokens.clone())
        .context("parse cfg_attr arguments for production import classification")?;
    let mut arguments = arguments.into_iter();
    let Some(condition) = arguments.next() else {
        return Ok(None);
    };
    if !cfg_can_apply_in_production(&condition) {
        return Ok(None);
    }
    for nested in arguments {
        let restricted = if nested.path().is_ident("cfg_attr") {
            let Meta::List(list) = nested else {
                continue;
            };
            restricted_cfg_attr_contents(&list.tokens, module, rust_2015_absolute_paths)?
        } else {
            restricted_meta_contents(&nested, module, rust_2015_absolute_paths)?
        };
        if restricted.is_some() {
            return Ok(restricted);
        }
    }
    Ok(None)
}

fn restricted_meta_contents(meta: &Meta, module: &[String], rust_2015_absolute_paths: bool) -> Result<Option<String>> {
    let string_scan = if matches!(
        meta.path().segments.last().map(|segment| normalized_ident(&segment.ident)),
        Some(name) if matches!(name.as_str(), "serde" | "schemars")
    ) {
        StringScan::GovernedAttribute
    } else {
        StringScan::Skip
    };
    let tokens = match meta {
        Meta::Path(_) => return Ok(None),
        Meta::List(list) => &list.tokens,
        Meta::NameValue(value) => {
            return restricted_token_identifier(&value.value.to_token_stream(), module, rust_2015_absolute_paths, string_scan);
        }
    };
    restricted_token_identifier(tokens, module, rust_2015_absolute_paths, string_scan)
}

fn restricted_string_path(literal: &proc_macro2::Literal, module: &[String], rust_2015_absolute_paths: bool, fail_unclassifiable: bool) -> Result<Option<String>> {
    let Ok(syn::Lit::Str(literal)) = syn::parse_str::<syn::Lit>(&literal.to_string()) else {
        return Ok(None);
    };
    let value = literal.value();
    if !value.contains("::") {
        return Ok(None);
    }
    if let Ok(path) = syn::parse_str::<SynPath>(&value) {
        return restricted_path_identifier(&path, module, rust_2015_absolute_paths);
    }
    let tokens = match value.parse::<TokenStream>() {
        Ok(tokens) => tokens,
        Err(_) if fail_unclassifiable => {
            bail!("path-bearing string in reviewed expansion is not classifiable Rust syntax");
        }
        Err(_) => return Ok(None),
    };
    restricted_fragment_identifier(&tokens, module, rust_2015_absolute_paths)
}

fn restricted_path_identifier(path: &SynPath, module: &[String], rust_2015_absolute_paths: bool) -> Result<Option<String>> {
    if path.leading_colon.is_some() && !rust_2015_absolute_paths {
        return Ok(None);
    }
    let mut segments = path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
    if path.leading_colon.is_some() {
        segments.insert(0, "crate".to_owned());
    }
    let Some(resolved) = resolve_path(module, &segments, false)? else {
        return Ok(None);
    };
    Ok(matches!(resolved.first().map(String::as_str), Some("server" | "ui")).then(|| resolved[0].clone()))
}

fn restricted_fragment_identifier(tokens: &TokenStream, module: &[String], rust_2015_absolute_paths: bool) -> Result<Option<String>> {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    for token in &tokens {
        if let TokenTree::Group(group) = token
            && let Some(restricted) = restricted_fragment_identifier(&group.stream(), module, rust_2015_absolute_paths)?
        {
            return Ok(Some(restricted));
        }
    }
    for index in 0..tokens.len() {
        let (leading_colon, start) = path_start(&tokens, index);
        let Some(start) = start else {
            continue;
        };
        if leading_colon && !rust_2015_absolute_paths {
            continue;
        }
        if !leading_colon && preceded_by_path_separator(&tokens, start) {
            continue;
        }
        let mut segments = path_segments(&tokens, start);
        if segments.len() < 2 {
            continue;
        }
        if leading_colon {
            segments.insert(0, "crate".to_owned());
        }
        let Some(resolved) = resolve_path(module, &segments, false)? else {
            continue;
        };
        if matches!(resolved.first().map(String::as_str), Some("server" | "ui")) {
            return Ok(Some(resolved[0].clone()));
        }
    }
    Ok(None)
}

fn path_start(tokens: &[TokenTree], index: usize) -> (bool, Option<usize>) {
    if matches!(tokens.get(index), Some(TokenTree::Ident(_))) {
        return (false, Some(index));
    }
    let leading_colon = punctuation(tokens.get(index), ':') && punctuation(tokens.get(index + 1), ':') && matches!(tokens.get(index + 2), Some(TokenTree::Ident(_)));
    (leading_colon, leading_colon.then_some(index + 2))
}

fn path_segments(tokens: &[TokenTree], start: usize) -> Vec<String> {
    let mut segments = Vec::new();
    let mut index = start;
    while let Some(TokenTree::Ident(ident)) = tokens.get(index) {
        segments.push(normalized_ident(ident));
        if !punctuation(tokens.get(index + 1), ':') || !punctuation(tokens.get(index + 2), ':') {
            break;
        }
        index += 3;
    }
    segments
}

fn preceded_by_path_separator(tokens: &[TokenTree], index: usize) -> bool {
    index >= 2 && punctuation(tokens.get(index - 2), ':') && punctuation(tokens.get(index - 1), ':')
}

fn punctuation(token: Option<&TokenTree>, expected: char) -> bool {
    matches!(token, Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == expected)
}

#[cfg(test)]
mod tests;
