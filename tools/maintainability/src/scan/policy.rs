use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use syn::ext::IdentExt as _;
use syn::{Attribute, Macro};

const ASSEMBLY_MACROS: [&str; 4] = ["asm", "global_asm", "llvm_asm", "naked_asm"];
const STANDALONE_ASSEMBLY_MACROS: [&str; 3] = ["global_asm", "llvm_asm", "naked_asm"];

pub(super) fn macro_name(macro_invocation: &Macro) -> Option<String> {
    macro_invocation.path.segments.last().map(|segment| segment.ident.unraw().to_string())
}

pub(super) fn is_standalone_assembly_macro(name: Option<&str>) -> bool {
    name.is_some_and(|name| STANDALONE_ASSEMBLY_MACROS.contains(&name))
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
