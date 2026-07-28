use std::path::Path;

use anyhow::{Context, Result, bail};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens as _;
use syn::{Attribute, Meta, Path as SynPath, UseTree};

use super::super::{ProductionCfgContext, normalized_ident, production_cfg_attr_metas};
use super::tokens::resolving_tokens;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct UsePath {
    pub(super) segments: Vec<String>,
    pub(super) renamed: bool,
    pub(super) alias: Option<String>,
}

pub(super) fn flatten_use_tree(tree: &UseTree, prefix: &mut Vec<String>, paths: &mut Vec<UsePath>) {
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
            paths.push(UsePath {
                segments,
                renamed: false,
                alias: None,
            });
        }
        UseTree::Rename(rename) => {
            let name = normalized_ident(&rename.ident);
            let mut segments = prefix.clone();
            if name != "self" {
                segments.push(name);
            }
            paths.push(UsePath {
                segments,
                renamed: true,
                alias: Some(normalized_ident(&rename.rename)),
            });
        }
        UseTree::Glob(_) => {
            let mut segments = prefix.clone();
            segments.push("*".to_owned());
            paths.push(UsePath {
                segments,
                renamed: false,
                alias: None,
            });
        }
        UseTree::Group(group) => {
            for nested in &group.items {
                flatten_use_tree(nested, prefix, paths);
            }
        }
    }
}

pub(super) fn resolve_path(module: &[String], path: &[String], rust_2015_use_path: bool) -> Result<Option<Vec<String>>> {
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
    if rust_2015_use_path {
        return Ok(Some(path.to_vec()));
    }
    let mut resolved = module.to_vec();
    resolved.extend_from_slice(path);
    Ok(Some(resolved))
}

pub(super) fn source_module(source_path: &str, crate_root: Option<&str>) -> Result<Vec<String>> {
    if crate_root == Some(source_path) {
        return Ok(Vec::new());
    }
    let root_directory = match crate_root {
        Some(root) => Path::new(root).parent().context("Cargo library target has no parent directory")?,
        None => Path::new("src"),
    };
    let relative = Path::new(source_path)
        .strip_prefix(root_directory)
        .context("production internal import source is outside its Cargo library root")?;
    let mut parts = relative
        .parent()
        .into_iter()
        .flat_map(Path::components)
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

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum StringScan {
    Skip,
    RustFragment,
    GovernedAttribute,
}

pub(super) fn restricted_token_identifier(tokens: &TokenStream, module: &[String], rust_2015_absolute_paths: bool, string_scan: StringScan) -> Result<Option<String>> {
    let tokens = resolving_tokens(tokens);
    if let Some(restricted) = restricted_fragment_identifier(&tokens, module, rust_2015_absolute_paths)? {
        return Ok(Some(restricted));
    }
    for token in tokens {
        let restricted = match token {
            TokenTree::Group(group) => restricted_token_identifier(&group.stream(), module, rust_2015_absolute_paths, string_scan)?,
            TokenTree::Literal(literal) if string_scan != StringScan::Skip => {
                restricted_string_path(&literal, module, rust_2015_absolute_paths, string_scan == StringScan::GovernedAttribute)?
            }
            TokenTree::Ident(_) | TokenTree::Literal(_) | TokenTree::Punct(_) => None,
        };
        if restricted.is_some() {
            return Ok(restricted);
        }
    }
    Ok(None)
}

pub(super) fn restricted_attribute_identifier(
    attribute: &Attribute,
    module: &[String],
    rust_2015_absolute_paths: bool,
    cfg_context: &ProductionCfgContext,
) -> Result<Option<String>> {
    if !attribute.path().is_ident("cfg_attr") {
        return restricted_meta_contents(&attribute.meta, module, rust_2015_absolute_paths);
    }
    let Meta::List(list) = &attribute.meta else {
        return Ok(None);
    };
    restricted_cfg_attr_contents(&list.tokens, module, rust_2015_absolute_paths, cfg_context)
}

fn restricted_cfg_attr_contents(tokens: &TokenStream, module: &[String], rust_2015_absolute_paths: bool, cfg_context: &ProductionCfgContext) -> Result<Option<String>> {
    for nested in production_cfg_attr_metas(tokens, cfg_context)? {
        let restricted = restricted_meta_contents(&nested, module, rust_2015_absolute_paths)?;
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
