use proc_macro2::{Group, TokenStream, TokenTree};

use super::super::normalized_ident;

pub(super) fn resolving_tokens(tokens: &TokenStream) -> TokenStream {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    let mut resolving = TokenStream::new();
    let mut index = 0_usize;
    while index < tokens.len() {
        if stringifying_group(&tokens, index).is_some() {
            resolving.extend(tokens[index..index.saturating_add(2)].iter().cloned());
            let TokenTree::Group(group) = &tokens[index + 2] else {
                unreachable!("stringifying_group requires a group");
            };
            let mut empty = Group::new(group.delimiter(), TokenStream::new());
            empty.set_span(group.span());
            resolving.extend([TokenTree::Group(empty)]);
            index = index.saturating_add(3);
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

fn stringifying_group(tokens: &[TokenTree], index: usize) -> Option<&Group> {
    let TokenTree::Ident(ident) = tokens.get(index)? else {
        return None;
    };
    if normalized_ident(ident) != "stringify" {
        return None;
    }
    if !matches!(tokens.get(index + 1), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '!') {
        return None;
    }
    let Some(TokenTree::Group(group)) = tokens.get(index + 2) else {
        return None;
    };
    Some(group)
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::*;

    #[test]
    fn nested_stringify_arguments_are_removed_but_code_tokens_remain() {
        let tokens = quote!(SqliteStore, concat!(stringify!(PostgresStore), SqliteStore));
        assert_eq!(resolving_tokens(&tokens).to_string(), quote!(SqliteStore, concat!(stringify!(), SqliteStore)).to_string());
    }
}
