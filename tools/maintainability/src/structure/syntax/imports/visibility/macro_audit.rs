use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use proc_macro2::{Delimiter, Group, TokenStream, TokenTree};
use syn::Path;

use super::super::tokens::resolving_tokens;
use super::VisibilityCounts;
use crate::structure::syntax::normalized_ident;

#[derive(Default)]
pub struct VisibilityMacroAudit {
    definitions: BTreeMap<MacroId, MacroDefinition>,
    direct_invocations: Vec<MacroInvocation>,
}

struct MacroDefinition {
    has_restricted_visibility: bool,
    references: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MacroId {
    module: Vec<String>,
    name: String,
}

struct MacroInvocation {
    module: Vec<String>,
    leading_colon: bool,
    segments: Vec<String>,
}

impl VisibilityMacroAudit {
    pub fn record_definition(&mut self, module: &[String], name: &proc_macro2::Ident, tokens: &TokenStream) -> Result<()> {
        let name = normalized_ident(name);
        let tokens = resolving_tokens(tokens);
        let transcribers = macro_transcribers(&tokens);
        let all_counts = VisibilityCounts::from_tokens(&tokens)?;
        let body_counts = transcribers.iter().try_fold(VisibilityCounts::default(), |mut total, body| {
            total.add(VisibilityCounts::from_tokens(&body.stream())?)?;
            Ok::<_, anyhow::Error>(total)
        })?;
        if all_counts != body_counts {
            bail!("production macro {name:?} places restricted visibility outside a macro transcriber");
        }
        if !all_counts.is_empty() {
            if transcribers.len() != 1 {
                bail!("production macro {name:?} with restricted visibility must have exactly one expansion arm");
            }
            if visibility_in_repetition(&transcribers[0].stream(), false)? {
                bail!("production macro {name:?} cannot repeat restricted visibility");
            }
        }
        if transcribers.iter().any(|body| transcriber_may_construct_restricted_visibility(&body.stream())) {
            bail!("production macro {name:?} cannot construct restricted visibility from metavariables");
        }
        let definition = MacroDefinition {
            has_restricted_visibility: !all_counts.is_empty(),
            references: macro_references(&tokens),
        };
        let id = MacroId {
            module: module.to_vec(),
            name: name.clone(),
        };
        if self.definitions.insert(id, definition).is_some() {
            bail!("production macro name {name:?} is ambiguous within module {module:?} for restricted-visibility accounting");
        }
        Ok(())
    }

    pub fn record_invocation(&mut self, module: &[String], path: &Path, tokens: &TokenStream) -> Result<()> {
        let tokens = resolving_tokens(tokens);
        if invocation_may_supply_visibility(&tokens)? {
            bail!("production macro invocation arguments cannot supply or construct restricted visibility");
        }
        if path.segments.is_empty() {
            return Ok(());
        }
        self.direct_invocations.push(MacroInvocation {
            module: module.to_vec(),
            leading_colon: path.leading_colon.is_some(),
            segments: path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect(),
        });
        Ok(())
    }

    pub fn finish(&self) -> Result<()> {
        for (id, definition) in &self.definitions {
            if !definition.has_restricted_visibility {
                continue;
            }
            let referenced_by = self
                .definitions
                .iter()
                .find_map(|(container, candidate)| candidate.references.contains(&id.name).then_some(container));
            if let Some(container) = referenced_by {
                bail!(
                    "production macro {:?} in module {:?} with restricted visibility cannot be invoked indirectly by macro {:?} in module {:?}",
                    id.name,
                    id.module,
                    container.name,
                    container.module
                );
            }
            let invocations = self
                .direct_invocations
                .iter()
                .filter(|invocation| self.resolve_invocation(invocation).is_some_and(|resolved| resolved == id))
                .count();
            if invocations != 1 {
                bail!(
                    "production macro {:?} in module {:?} with restricted visibility must have exactly one direct production invocation; observed {invocations}",
                    id.name,
                    id.module
                );
            }
        }
        Ok(())
    }

    fn resolve_invocation(&self, invocation: &MacroInvocation) -> Option<&MacroId> {
        let (first, rest) = invocation.segments.split_first()?;
        if invocation.leading_colon {
            return None;
        }
        let explicit = match first.as_str() {
            "crate" => Some(rest.to_vec()),
            "self" => {
                let mut path = invocation.module.clone();
                path.extend_from_slice(rest);
                Some(path)
            }
            "super" => resolve_super_path(&invocation.module, &invocation.segments),
            _ if !rest.is_empty() => {
                let mut path = invocation.module.clone();
                path.extend_from_slice(&invocation.segments);
                Some(path)
            }
            _ => None,
        };
        if let Some(mut path) = explicit {
            let name = path.pop()?;
            return self.definitions.get_key_value(&MacroId { module: path, name }).map(|(id, _)| id);
        }
        self.definitions
            .keys()
            .filter(|id| id.name == *first && invocation.module.starts_with(&id.module))
            .max_by_key(|id| id.module.len())
    }
}

fn resolve_super_path(module: &[String], segments: &[String]) -> Option<Vec<String>> {
    let mut path = module.to_vec();
    let mut index = 0;
    while segments.get(index).is_some_and(|segment| segment == "super") {
        path.pop()?;
        index += 1;
    }
    path.extend_from_slice(&segments[index..]);
    Some(path)
}

impl VisibilityCounts {
    fn add(&mut self, other: Self) -> Result<()> {
        self.pub_crate = self.pub_crate.checked_add(other.pub_crate).context("production pub(crate) macro count overflow")?;
        self.pub_super = self.pub_super.checked_add(other.pub_super).context("production pub(super) macro count overflow")?;
        Ok(())
    }
}

fn macro_transcribers(tokens: &TokenStream) -> Vec<Group> {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    tokens
        .windows(4)
        .filter_map(|window| match window {
            [TokenTree::Group(_), TokenTree::Punct(equal), TokenTree::Punct(arrow), TokenTree::Group(body)] if equal.as_char() == '=' && arrow.as_char() == '>' => {
                Some(body.clone())
            }
            _ => None,
        })
        .collect()
}

fn visibility_in_repetition(tokens: &TokenStream, inside_repetition: bool) -> Result<bool> {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    if inside_repetition && !VisibilityCounts::from_tokens(&tokens.iter().cloned().collect::<TokenStream>())?.is_empty() {
        return Ok(true);
    }
    for (index, token) in tokens.iter().enumerate() {
        let TokenTree::Group(group) = token else {
            continue;
        };
        let repeated = is_repetition_group(&tokens, index);
        if visibility_in_repetition(&group.stream(), inside_repetition || repeated)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn transcriber_may_construct_restricted_visibility(tokens: &TokenStream) -> bool {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if token_ident(token).as_deref() == Some("pub") {
            match tokens.get(index.saturating_add(1)) {
                Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '$' => return true,
                Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis && token_stream_contains_dollar(&group.stream()) => {
                    return true;
                }
                _ => {}
            }
        }
        if is_dollar(token)
            && matches!(tokens.get(index.saturating_add(1)), Some(TokenTree::Ident(_)))
            && let Some(TokenTree::Group(group)) = tokens.get(index.saturating_add(2))
            && group.delimiter() == Delimiter::Parenthesis
            && restriction_tokens_may_supply_scope(&group.stream())
        {
            return true;
        }
        if let TokenTree::Group(group) = token
            && transcriber_may_construct_restricted_visibility(&group.stream())
        {
            return true;
        }
    }
    false
}

fn restriction_tokens_may_supply_scope(tokens: &TokenStream) -> bool {
    tokens.clone().into_iter().any(|token| match token {
        TokenTree::Group(group) => restriction_tokens_may_supply_scope(&group.stream()),
        TokenTree::Ident(ident) => matches!(normalized_ident(&ident).as_str(), "crate" | "super" | "in"),
        TokenTree::Punct(punctuation) => punctuation.as_char() == '$',
        TokenTree::Literal(_) => false,
    })
}

fn token_stream_contains_dollar(tokens: &TokenStream) -> bool {
    tokens.clone().into_iter().any(|token| match token {
        TokenTree::Group(group) => token_stream_contains_dollar(&group.stream()),
        TokenTree::Punct(punctuation) => punctuation.as_char() == '$',
        TokenTree::Ident(_) | TokenTree::Literal(_) => false,
    })
}

fn is_repetition_group(tokens: &[TokenTree], index: usize) -> bool {
    let preceded_by_dollar = index.checked_sub(1).and_then(|previous| tokens.get(previous)).is_some_and(is_dollar);
    preceded_by_dollar
        && [index.saturating_add(1), index.saturating_add(2)]
            .into_iter()
            .filter_map(|next| tokens.get(next))
            .any(is_repetition_operator)
}

fn invocation_may_supply_visibility(tokens: &TokenStream) -> Result<bool> {
    if !VisibilityCounts::from_tokens(tokens)?.is_empty() {
        return Ok(true);
    }
    let mut identifiers = BTreeSet::new();
    collect_identifiers(tokens, &mut identifiers);
    Ok(identifiers.contains("pub") && ["crate", "super", "in"].into_iter().any(|kind| identifiers.contains(kind)))
}

fn collect_identifiers(tokens: &TokenStream, identifiers: &mut BTreeSet<String>) {
    for token in tokens.clone() {
        match token {
            TokenTree::Group(group) => collect_identifiers(&group.stream(), identifiers),
            TokenTree::Ident(ident) => {
                identifiers.insert(normalized_ident(&ident));
            }
            TokenTree::Literal(_) | TokenTree::Punct(_) => {}
        }
    }
}

fn macro_references(tokens: &TokenStream) -> BTreeSet<String> {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    let mut references = BTreeSet::new();
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token, TokenTree::Punct(punctuation) if punctuation.as_char() == '!')
            && let Some(TokenTree::Ident(ident)) = index.checked_sub(1).and_then(|previous| tokens.get(previous))
        {
            references.insert(normalized_ident(ident));
        }
        if let TokenTree::Group(group) = token {
            references.extend(macro_references(&group.stream()));
        }
    }
    references
}

fn is_dollar(token: &TokenTree) -> bool {
    matches!(token, TokenTree::Punct(punctuation) if punctuation.as_char() == '$')
}

fn token_ident(token: &TokenTree) -> Option<String> {
    let TokenTree::Ident(ident) = token else {
        return None;
    };
    Some(normalized_ident(ident))
}

fn is_repetition_operator(token: &TokenTree) -> bool {
    matches!(token, TokenTree::Punct(punctuation) if matches!(punctuation.as_char(), '*' | '+' | '?'))
}
