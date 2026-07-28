use std::path::Path as FsPath;

use anyhow::{Context, Result, bail};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens as _;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Arm, Attribute, BareFnArg, BareVariadic, Expr, Field, FieldPat, FieldValue, File, FnArg, ForeignItem, GenericParam, ImplItem, Item, ItemExternCrate, ItemMod, ItemUse, Local,
    Meta, Pat, Path as SynPath, StmtMacro, Token, TraitItem, UseTree, Variadic, Variant, Visibility,
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
mod tests {
    use super::*;

    fn imports(path: &str, source: &str) -> Result<Vec<String>> {
        let syntax = syn::parse_file(source)?;
        production_internal_imports(&syntax, path, Some("src/lib.rs"), false, true)
    }

    #[test]
    fn grouped_renamed_glob_and_relative_imports_are_normalized() {
        let source = "use crate::{server::{LocalHoldServer as Server, params::*}, ui};\n\
                      mod nested { use super::super::server::params; }\n";
        assert_eq!(
            imports("src/adapter.rs", source).expect("imports"),
            ["crate::server::LocalHoldServer", "crate::server::params", "crate::server::params::*", "crate::ui",]
        );
    }

    #[test]
    fn test_only_imports_are_excluded_at_item_and_parent_scope() {
        let source = "#[cfg(test)]\nuse crate::server::params;\n\
                      #[cfg(feature = \"testing\")]\nmod support { use crate::ui; }\n\
                      #[cfg(test)]\nfn test_path() -> crate::server::TestOnly { unreachable!() }\n\
                      use crate::server::LocalHoldServer;\n";
        assert_eq!(imports("src/http_transport.rs", source).expect("imports"), ["crate::server::LocalHoldServer"]);
    }

    #[test]
    fn qualified_paths_are_collected_without_double_counting_imports() {
        let source = "use crate::server::Imported;\n\
                      fn build() -> crate::server::Imported { crate::ui::qualified(); crate::ui::qualified() }\n";
        assert_eq!(
            imports("src/adapter.rs", source).expect("imports and qualified paths"),
            ["crate::server::Imported", "crate::ui::qualified"]
        );
    }

    #[test]
    fn bare_paths_resolve_only_at_the_crate_root() -> Result<()> {
        let source = "mod nested { mod server {} use server::Type; fn call() { server::call(); } }\n\
                      use crate::server::Actual;\n";
        assert_eq!(imports("src/adapter.rs", source).expect("lexical bare paths"), ["crate::server::Actual"]);

        let syntax = syn::parse_file("use server::AtRoot;\n")?;
        assert_eq!(
            production_internal_imports(&syntax, "src/core.rs", Some("src/core.rs"), false, true)?,
            ["crate::server::AtRoot"]
        );
        Ok(())
    }

    #[test]
    fn nested_custom_library_roots_define_relative_module_paths() -> Result<()> {
        let syntax = syn::parse_file("use super::server::Service;\n")?;
        assert_eq!(
            production_internal_imports(&syntax, "src/core/worker.rs", Some("src/core/lib.rs"), false, true)?,
            ["crate::server::Service"]
        );
        Ok(())
    }

    #[test]
    fn restricted_names_in_production_macro_tokens_fail_closed() {
        let source = "macro_rules! dependency { () => { use crate::server::Service; } }\n";
        assert!(imports("src/adapter.rs", source).unwrap_err().to_string().contains("production macro token stream"));

        let encoded = "numbered_placeholders!(\"crate::server::serialize\");\n";
        assert!(imports("src/adapter.rs", encoded).unwrap_err().to_string().contains("production macro token stream"));

        let test_only = "#[cfg(test)]\nmacro_rules! dependency { () => { use crate::ui::View; } }\n";
        assert!(imports("src/adapter.rs", test_only).expect("test-only macro").is_empty());
    }

    #[test]
    fn restricted_names_in_production_attribute_tokens_fail_closed() {
        let source = "#[adapter(crate::server::Service)]\nfn build() {}\n";
        assert!(imports("src/adapter.rs", source).unwrap_err().to_string().contains("production attribute token stream"));

        let test_only = "#[cfg(test)]\n#[adapter(crate::ui::View)]\nfn build() {}\n";
        assert!(imports("src/adapter.rs", test_only).expect("test-only attribute").is_empty());
    }

    #[test]
    fn string_encoded_attribute_paths_fail_closed() {
        let source = "#[serde(serialize_with = \"crate::server::serialize\")]\nstruct Record;\n";
        assert!(imports("src/adapter.rs", source).unwrap_err().to_string().contains("production attribute token stream"));

        let bound = "#[serde(bound(serialize = \"T: crate::server::Marker\"))]\nstruct Generic<T>(T);\n";
        assert!(imports("src/adapter.rs", bound).unwrap_err().to_string().contains("production attribute token stream"));

        let external_bound = "#[serde(bound(serialize = \"T: external::Marker\"))]\nstruct Generic<T>(T);\n";
        assert!(imports("src/adapter.rs", external_bound).expect("external bound").is_empty());

        let unclassifiable = "#[serde(bound(serialize = \"T: external::Marker /*\"))]\nstruct Generic<T>(T);\n";
        assert!(imports("src/adapter.rs", unclassifiable).unwrap_err().to_string().contains("not classifiable Rust syntax"));

        let external = "#[serde(serialize_with = \"::server::serialize\")]\nstruct Record;\n";
        assert!(imports("src/adapter.rs", external).expect("absolute external path").is_empty());

        let plain_text = "#[serde(rename = \"server::label\")]\nstruct Record;\n";
        assert!(imports("src/nested/adapter.rs", plain_text).expect("non-root relative label").is_empty());
    }

    #[test]
    fn cfg_attr_scans_only_nested_attributes_that_can_apply_in_production() {
        let test_only = "#[cfg_attr(test, serde(serialize_with = \"crate::server::serialize\"))]\n\
                         #[cfg_attr(feature = \"testing\", serde(serialize_with = \"crate::ui::serialize\"))]\n\
                         struct Record;\n";
        assert!(imports("src/adapter.rs", test_only).expect("test-only nested attributes").is_empty());

        let production = "#[cfg_attr(feature = \"other\", serde(serialize_with = \"crate::server::serialize\"))]\nstruct Record;\n";
        assert!(imports("src/adapter.rs", production).unwrap_err().to_string().contains("production attribute token stream"));
    }

    #[test]
    fn test_only_and_production_items_on_one_line_are_distinguished() {
        let source = "#[cfg(test)] fn helper() { crate::ui::test_only(); } use crate::server::Service;\n";
        assert_eq!(imports("src/adapter.rs", source).expect("same-line items"), ["crate::server::Service"]);
    }

    #[test]
    fn absolute_external_paths_are_not_classified_as_crate_relative() {
        let source = "use ::server::External;\n\
                      use ::ui::External as ExternalUi;\n\
                      fn build() -> ::server::Qualified { ::ui::qualified() }\n";
        assert!(imports("src/adapter.rs", source).expect("absolute external paths").is_empty());
    }

    #[test]
    fn rust_2015_absolute_paths_are_classified_as_crate_relative() -> Result<()> {
        let syntax = syn::parse_file(
            "use ::server::External;\n\
             fn build() -> ::server::Qualified { ::ui::qualified() }\n",
        )?;
        assert_eq!(
            production_internal_imports(&syntax, "src/adapter.rs", Some("src/lib.rs"), true, true)?,
            ["crate::server::External", "crate::server::Qualified", "crate::ui::qualified"]
        );
        Ok(())
    }

    #[test]
    fn rust_2015_nested_bare_use_paths_are_classified_from_the_crate_root() -> Result<()> {
        let syntax = syn::parse_file("mod nested { use server::Service; }\n")?;
        assert_eq!(
            production_internal_imports(&syntax, "src/adapter.rs", Some("src/lib.rs"), true, true)?,
            ["crate::server::Service"]
        );
        Ok(())
    }

    #[test]
    fn raw_identifiers_normalize_and_crate_root_aliases_fail_closed() {
        assert_eq!(
            imports("src/adapter.rs", "use crate::r#server::LocalHoldServer;\n").expect("raw import"),
            ["crate::server::LocalHoldServer"]
        );
        assert!(
            imports("src/adapter.rs", "use crate as root;\n")
                .unwrap_err()
                .to_string()
                .contains("crate-root import aliases")
        );
        assert!(
            imports("src/adapter.rs", "extern crate self as root;\n")
                .unwrap_err()
                .to_string()
                .contains("extern aliases")
        );
        assert!(imports("src/lib.rs", "use crate::*;\n").unwrap_err().to_string().contains("crate-root glob imports"));
    }

    #[test]
    fn restricted_imports_cannot_be_reexported() {
        assert!(
            imports("src/http_transport.rs", "pub use crate::server::LocalHoldServer;\n")
                .unwrap_err()
                .to_string()
                .contains("cannot be re-exported")
        );
        assert!(
            imports("src/http_transport.rs", "pub(crate) use crate::server::LocalHoldServer;\n")
                .unwrap_err()
                .to_string()
                .contains("cannot be re-exported")
        );
    }

    #[test]
    fn cfg_gated_parameters_are_excluded_by_node() {
        let source = "fn call(#[cfg(test)] _: crate::server::TestOnly) {}\n\
                      fn generic<#[cfg(feature = \"testing\")] T: crate::ui::TestOnly>() {}\n";
        assert!(imports("src/adapter.rs", source).expect("test-only parameters").is_empty());
    }

    #[test]
    fn unreviewed_production_expansions_fail_closed() {
        assert!(
            imports("src/adapter.rs", "fn call() { inject!(); }\n")
                .unwrap_err()
                .to_string()
                .contains("unreviewed macro expansion")
        );
        assert!(
            imports("src/adapter.rs", "#[inject]\nfn call() {}\n")
                .unwrap_err()
                .to_string()
                .contains("unreviewed attribute expansion")
        );
        let test_only = "#[cfg(test)]\n#[inject]\nfn call() { inject!(); }\n";
        assert!(imports("src/adapter.rs", test_only).expect("test-only opaque expansions").is_empty());
    }
}
