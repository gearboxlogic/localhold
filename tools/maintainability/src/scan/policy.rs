use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use syn::Meta;
use syn::ext::IdentExt as _;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::{Attribute, ItemUse, Macro, Path, Token, UseTree};

use super::{RESERVED_LOCAL_MACROS, REVIEWED_EXPANSION_PACKAGES};

const ASSEMBLY_MACROS: [&str; 4] = ["asm", "global_asm", "llvm_asm", "naked_asm"];
const STANDALONE_ASSEMBLY_MACROS: [&str; 2] = ["core::arch::global_asm", "core::arch::naked_asm"];
const BUILTIN_MACROS: [&str; 19] = [
    "assert",
    "assert_eq",
    "assert_ne",
    "cfg",
    "concat",
    "debug_assert",
    "debug_assert_eq",
    "env",
    "format",
    "format_args",
    "include_str",
    "macro_rules",
    "matches",
    "panic",
    "println",
    "stringify",
    "vec",
    "write",
    "writeln",
];
const BUILTIN_DERIVES: [&str; 9] = ["Clone", "Copy", "Debug", "Default", "Eq", "Hash", "Ord", "PartialEq", "PartialOrd"];
const PROTECTED_ATTRIBUTES: [&str; 34] = [
    "allow",
    "cfg",
    "cfg_attr",
    "cold",
    "default",
    "deny",
    "deprecated",
    "derive",
    "doc",
    "error",
    "expect",
    "forbid",
    "from",
    "ignore",
    "inline",
    "link",
    "link_name",
    "link_ordinal",
    "macro_export",
    "must_use",
    "non_exhaustive",
    "proptest_config",
    "repr",
    "schemars",
    "serde",
    "should_panic",
    "source",
    "test",
    "thread_local",
    "track_caller",
    "unsafe",
    "used",
    "warn",
    "windows_subsystem",
];
pub(super) fn macro_name(macro_invocation: &Macro) -> Option<String> {
    macro_invocation.path.segments.last().map(|segment| segment.ident.unraw().to_string())
}

pub(super) fn is_trusted_local_macro_name(name: &str) -> bool {
    RESERVED_LOCAL_MACROS.contains(&name)
}

pub(super) fn is_trusted_macro(macro_invocation: &Macro) -> bool {
    matches!(
        normalized_path(&macro_invocation.path).as_str(),
        "assert"
            | "assert_eq"
            | "assert_ne"
            | "cfg"
            | "concat"
            | "concat_placeholders"
            | "concat_with_sep"
            | "criterion_group"
            | "criterion_main"
            | "debug_assert"
            | "debug_assert_eq"
            | "define_memory_columns"
            | "env"
            | "format"
            | "format_args"
            | "futures::poll"
            | "include_str"
            | "info"
            | "insta::assert_json_snapshot"
            | "json"
            | "json_schema"
            | "macro_rules"
            | "matches"
            | "numbered_placeholders"
            | "ort::inputs"
            | "panic"
            | "params"
            | "println"
            | "prop_oneof"
            | "proptest"
            | "rusqlite::params"
            | "schemars::json_schema"
            | "schemars::schema_for"
            | "serde_json::json"
            | "stringify"
            | "tokio::join"
            | "tokio::pin"
            | "tokio::select"
            | "tokio::try_join"
            | "tracing::debug"
            | "tracing::error"
            | "tracing::info"
            | "tracing::warn"
            | "transport_test"
            | "vec"
            | "warn"
            | "write"
            | "writeln"
    )
}

pub(super) fn is_trusted_attribute(attribute: &Attribute) -> bool {
    is_trusted_meta(&attribute.meta)
}

pub(super) fn is_reserved_expansion_root(name: &str) -> bool {
    REVIEWED_EXPANSION_PACKAGES.contains(&name)
}

pub(super) fn untrusted_import(item: &ItemUse) -> Option<String> {
    let mut imports = Vec::new();
    collect_imports(&item.tree, &mut Vec::new(), &mut imports);
    imports.into_iter().find_map(|import| match import {
        Import::Named { source, binding } => untrusted_named_import(&source, &binding),
        Import::Glob(source) => {
            let path = source.join("::");
            let local = source.first().is_some_and(|root| matches!(root.as_str(), "crate" | "self" | "super"));
            (path != "proptest::prelude" && path != "rand::prelude" && !local).then(|| format!("glob import {path} can introduce an unreviewed expansion name"))
        }
    })
}

fn untrusted_named_import(source: &[String], binding: &str) -> Option<String> {
    let root = source.first().map(String::as_str).unwrap_or_default();
    if is_reserved_expansion_root(binding) && (source.len() != 1 || root != binding) {
        return Some(format!("import {} as {binding} can shadow reviewed expansion package {binding}", source.join("::")));
    }
    let local = matches!(root, "crate" | "self" | "super" | "localhold");
    if RESERVED_LOCAL_MACROS.contains(&binding) {
        let direct_local = source.len() == 1 && root == binding;
        return (!local && !direct_local).then(|| format!("import {} as {binding} impersonates a reviewed local macro", source.join("::")));
    }
    let expected = match binding {
        "criterion_group" | "criterion_main" => Some("criterion"),
        "poll" => Some("futures"),
        "assert_json_snapshot" => Some("insta"),
        "inputs" => Some("ort"),
        "prop_oneof" | "proptest" => Some("proptest"),
        "tool" | "tool_router" => Some("rmcp"),
        "params" => Some("rusqlite"),
        "JsonSchema" | "json_schema" => Some("schemars"),
        "Deserialize" | "Serialize" => Some("serde"),
        "json" => Some("serde_json"),
        "debug" | "error" | "info" | "warn" => Some("tracing"),
        name if BUILTIN_MACROS.contains(&name) => Some(""),
        name if BUILTIN_DERIVES.contains(&name) => Some(""),
        name if PROTECTED_ATTRIBUTES.contains(&name) => Some(""),
        _ => None,
    };
    expected
        .and_then(|expected| (root != expected && !local).then(|| format!("import {} as {binding} does not come from reviewed expansion package {expected}", source.join("::"))))
}

enum Import {
    Named { source: Vec<String>, binding: String },
    Glob(Vec<String>),
}

fn collect_imports(tree: &UseTree, prefix: &mut Vec<String>, imports: &mut Vec<Import>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.unraw().to_string());
            collect_imports(&path.tree, prefix, imports);
            prefix.pop();
        }
        UseTree::Name(name) => {
            let binding = name.ident.unraw().to_string();
            let mut source = prefix.clone();
            source.push(binding.clone());
            imports.push(Import::Named { source, binding });
        }
        UseTree::Rename(rename) => {
            let mut source = prefix.clone();
            source.push(rename.ident.unraw().to_string());
            imports.push(Import::Named {
                source,
                binding: rename.rename.unraw().to_string(),
            });
        }
        UseTree::Glob(_) => imports.push(Import::Glob(prefix.clone())),
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_imports(tree, prefix, imports);
            }
        }
    }
}

fn is_trusted_meta(meta: &Meta) -> bool {
    let path = meta.path();
    let name = normalized_path(path);
    match name.as_str() {
        "cfg_attr" => trusted_cfg_attr(meta),
        "derive" => trusted_derive(meta),
        _ => matches!(
            name.as_str(),
            "allow"
                | "cfg"
                | "cold"
                | "default"
                | "deny"
                | "deprecated"
                | "doc"
                | "error"
                | "expect"
                | "forbid"
                | "from"
                | "ignore"
                | "inline"
                | "link"
                | "link_name"
                | "link_ordinal"
                | "macro_export"
                | "must_use"
                | "non_exhaustive"
                | "proptest_config"
                | "repr"
                | "schemars"
                | "serde"
                | "should_panic"
                | "source"
                | "test"
                | "thread_local"
                | "tokio::main"
                | "tokio::test"
                | "tool"
                | "tool_router"
                | "track_caller"
                | "unsafe"
                | "used"
                | "warn"
        ),
    }
}

fn trusted_cfg_attr(meta: &Meta) -> bool {
    let Meta::List(list) = meta else {
        return false;
    };
    let Ok(items) = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone()) else {
        return false;
    };
    items.len() >= 2 && items.iter().skip(1).all(|nested| normalized_path(nested.path()) != "doc" && is_trusted_meta(nested))
}

fn trusted_derive(meta: &Meta) -> bool {
    let Meta::List(list) = meta else {
        return false;
    };
    Punctuated::<Path, Token![,]>::parse_terminated.parse2(list.tokens.clone()).is_ok_and(|paths| {
        paths.iter().all(|path| {
            matches!(
                normalized_path(path).as_str(),
                "Clone"
                    | "Copy"
                    | "Debug"
                    | "Default"
                    | "Deserialize"
                    | "Eq"
                    | "Hash"
                    | "JsonSchema"
                    | "Ord"
                    | "PartialEq"
                    | "PartialOrd"
                    | "Serialize"
                    | "schemars::JsonSchema"
                    | "serde::Deserialize"
                    | "serde::Serialize"
                    | "thiserror::Error"
            )
        })
    })
}

fn normalized_path(path: &Path) -> String {
    path.segments.iter().map(|segment| segment.ident.unraw().to_string()).collect::<Vec<_>>().join("::")
}

pub(super) fn is_standalone_assembly_macro(macro_invocation: &Macro) -> bool {
    STANDALONE_ASSEMBLY_MACROS.contains(&normalized_path(&macro_invocation.path).as_str())
}

pub(super) fn contains_assembly_macro(tokens: &TokenStream) -> bool {
    ASSEMBLY_MACROS.into_iter().any(|name| contains_structural_ident(tokens.clone(), name))
}

pub(super) fn is_safety_lint_exception(attribute: &Attribute) -> bool {
    let path = attribute.path().segments.last().map(|segment| segment.ident.unraw().to_string());
    let tokens = attribute.meta.to_token_stream();
    match path.as_deref() {
        Some("allow" | "expect" | "warn") => contains_safety_lint_name(&tokens),
        Some("cfg_attr") => contains_safety_lint_name(&tokens) && ["allow", "expect", "warn"].into_iter().any(|level| contains_structural_ident(tokens.clone(), level)),
        _ => false,
    }
}

pub(super) fn is_unsafe_attribute(attribute: &Attribute) -> bool {
    let path = attribute.path().segments.last().map(|segment| segment.ident.unraw().to_string());
    path.as_deref() == Some("unsafe") || (path.as_deref() == Some("cfg_attr") && contains_structural_ident(attribute.meta.to_token_stream(), "unsafe"))
}

pub(super) fn is_path_override(attribute: &Attribute) -> bool {
    let path = attribute.path().segments.last().map(|segment| segment.ident.unraw().to_string());
    path.as_deref() == Some("path") || (path.as_deref() == Some("cfg_attr") && contains_structural_ident(attribute.meta.to_token_stream(), "path"))
}

pub(super) fn contains_unaudited_macro_syntax(tokens: &TokenStream) -> bool {
    contains_plain_ident(tokens.clone(), "unsafe")
        || contains_structural_ident(tokens.clone(), "static")
        || contains_macro_safety_lint_exception(tokens.clone())
        || contains_assembly_macro(tokens)
}

pub(super) fn contains_token_paste_syntax(tokens: &TokenStream) -> bool {
    tokens.clone().into_iter().any(|token| match token {
        TokenTree::Group(group) => {
            let children: Vec<_> = group.stream().into_iter().collect();
            group.delimiter() == Delimiter::Bracket
                && matches!(children.first(), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '<')
                && matches!(children.last(), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '>')
                || contains_token_paste_syntax(&group.stream())
        }
        TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

pub(super) fn contains_structural_ident(tokens: TokenStream, expected: &str) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => contains_structural_ident(group.stream(), expected),
        TokenTree::Ident(identifier) => identifier.unraw() == expected,
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

pub(super) fn contains_opaque_attribute(tokens: TokenStream) -> bool {
    let tokens: Vec<_> = tokens.into_iter().collect();
    tokens.iter().enumerate().any(|(index, token)| {
        if !matches!(token, TokenTree::Punct(punctuation) if punctuation.as_char() == '#') {
            return false;
        }
        let mut attribute_index = index + 1;
        if matches!(
            tokens.get(attribute_index),
            Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '!'
        ) {
            attribute_index += 1;
        }
        !matches!(
            tokens.get(attribute_index),
            Some(TokenTree::Group(group))
                if group.delimiter() == Delimiter::Bracket && !contains_punctuation(group.stream(), '$')
        )
    }) || tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => contains_opaque_attribute(group.stream()),
        TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

pub(super) fn contains_path_attribute(tokens: TokenStream) -> bool {
    let tokens: Vec<_> = tokens.into_iter().collect();
    tokens.windows(2).any(|pair| {
        matches!(&pair[0], TokenTree::Punct(punctuation) if punctuation.as_char() == '#')
            && matches!(&pair[1], TokenTree::Group(group) if {
                let attribute = group.stream();
                first_ident_is(attribute.clone(), "path")
                    || (first_ident_is(attribute.clone(), "cfg_attr") && contains_structural_ident(attribute, "path"))
            })
    }) || tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => contains_path_attribute(group.stream()),
        TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn contains_macro_safety_lint_exception(tokens: TokenStream) -> bool {
    let tokens: Vec<_> = tokens.into_iter().collect();
    tokens.iter().enumerate().any(|(index, token)| {
        if !matches!(token, TokenTree::Punct(punctuation) if punctuation.as_char() == '#') {
            return false;
        }
        let attribute_index = index
            + usize::from(matches!(
                tokens.get(index + 1),
                Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '!'
            ))
            + 1;
        matches!(
            tokens.get(attribute_index),
            Some(TokenTree::Group(group))
                if group.delimiter() == Delimiter::Bracket && is_safety_lint_meta(&group.stream())
        )
    }) || tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => contains_macro_safety_lint_exception(group.stream()),
        TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn is_safety_lint_meta(tokens: &TokenStream) -> bool {
    if ["allow", "expect", "warn"].into_iter().any(|level| first_ident_is(tokens.clone(), level)) {
        return contains_safety_lint_name(tokens);
    }
    first_ident_is(tokens.clone(), "cfg_attr")
        && contains_safety_lint_name(tokens)
        && ["allow", "expect", "warn"].into_iter().any(|level| contains_structural_ident(tokens.clone(), level))
}

fn contains_safety_lint_name(tokens: &TokenStream) -> bool {
    let names = ["unsafe_code", "unsafe_op_in_unsafe_fn", "undocumented_unsafe_blocks"];
    let groups = ["all", "future_incompatible", "restriction", "rust_2024_compatibility", "warnings"];
    names.into_iter().chain(groups).any(|name| contains_structural_ident(tokens.clone(), name))
}

fn contains_plain_ident(tokens: TokenStream, expected: &str) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => contains_plain_ident(group.stream(), expected),
        TokenTree::Ident(identifier) => identifier == expected,
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn contains_punctuation(tokens: TokenStream, expected: char) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => contains_punctuation(group.stream(), expected),
        TokenTree::Punct(punctuation) => punctuation.as_char() == expected,
        TokenTree::Ident(_) | TokenTree::Literal(_) => false,
    })
}

fn first_ident_is(tokens: TokenStream, expected: &str) -> bool {
    matches!(tokens.into_iter().next(), Some(TokenTree::Ident(identifier)) if identifier.unraw() == expected)
}
