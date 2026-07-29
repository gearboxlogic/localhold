use proc_macro2::{Group, TokenStream, TokenTree};

use super::super::normalized_ident;

pub(super) fn resolving_tokens(tokens: &TokenStream) -> TokenStream {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    let mut resolving = TokenStream::new();
    let mut index = 0_usize;
    while index < tokens.len() {
        if let Some((group, group_offset)) = stringifying_group(&tokens, index) {
            resolving.extend(tokens[index..index.saturating_add(group_offset)].iter().cloned());
            let mut empty = Group::new(group.delimiter(), TokenStream::new());
            empty.set_span(group.span());
            resolving.extend([TokenTree::Group(empty)]);
            index = index.saturating_add(group_offset).saturating_add(1);
            continue;
        }
        match &tokens[index] {
            TokenTree::Group(group) => {
                let mut nested = Group::new(group.delimiter(), resolving_tokens(&group.stream()));
                nested.set_span(group.span());
                resolving.extend([TokenTree::Group(nested)]);
            }
            token => resolving.extend([token.clone()]),
        }
        index = index.saturating_add(1);
    }
    resolving
}

fn stringifying_group(tokens: &[TokenTree], index: usize) -> Option<(&Group, usize)> {
    if !matches!(tokens.get(index), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == ':')
        || !matches!(tokens.get(index + 1), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == ':')
    {
        return None;
    }
    let Some(TokenTree::Ident(root)) = tokens.get(index + 2) else {
        return None;
    };
    if !matches!(normalized_ident(root).as_str(), "core" | "std")
        || !matches!(tokens.get(index + 3), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == ':')
        || !matches!(tokens.get(index + 4), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == ':')
    {
        return None;
    }
    let Some(TokenTree::Ident(macro_name)) = tokens.get(index + 5) else {
        return None;
    };
    if normalized_ident(macro_name) != "stringify" || !matches!(tokens.get(index + 6), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '!') {
        return None;
    }
    let Some(TokenTree::Group(group)) = tokens.get(index + 7) else {
        return None;
    };
    Some((group, 7))
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::*;

    #[test]
    fn nested_stringify_arguments_are_removed_but_code_tokens_remain() {
        let tokens = quote!(SqliteStore, concat!(::core::stringify!(PostgresStore), stringify!(PostgresStore), SqliteStore));
        assert_eq!(
            resolving_tokens(&tokens).to_string(),
            quote!(SqliteStore, concat!(::core::stringify!(), stringify!(PostgresStore), SqliteStore)).to_string()
        );
    }
}
