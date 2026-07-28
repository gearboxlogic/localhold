use anyhow::{Context, Result};
use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens as _;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::{Attribute, Expr, Meta, Token};

use super::normalized_ident;

mod sat;
use sat::is_satisfiable;

#[derive(Clone, Debug)]
enum Predicate {
    Constant(bool),
    Atom { identity: String, exclusive_group: Option<String> },
    All(Vec<Self>),
    Any(Vec<Self>),
    Not(Box<Self>),
}

struct CfgAttr {
    condition: Meta,
    nested: Vec<Meta>,
}

#[derive(Clone, Debug, Default)]
pub(in crate::structure) struct ProductionCfgContext {
    constraints: Vec<Predicate>,
}

impl ProductionCfgContext {
    pub(in crate::structure) fn identity(&self) -> String {
        let mut identity = String::new();
        for constraint in &self.constraints {
            constraint.write_identity(&mut identity);
            identity.push(';');
        }
        identity
    }

    pub(in crate::structure) fn conjoin(&self, other: &Self) -> Option<Self> {
        let mut constraints = self.constraints.clone();
        constraints.extend(other.constraints.clone());
        let context = Self { constraints };
        context_is_satisfiable(&context, None).then_some(context)
    }

    pub(in crate::structure) fn disjunction(contexts: impl IntoIterator<Item = Self>) -> Option<Self> {
        let alternatives = contexts.into_iter().map(|context| Predicate::All(context.constraints)).collect::<Vec<_>>();
        match alternatives.as_slice() {
            [] => None,
            [Predicate::All(constraints)] => Some(Self { constraints: constraints.clone() }),
            _ => Some(Self {
                constraints: vec![Predicate::Any(alternatives)],
            }),
        }
    }
}

impl Predicate {
    fn write_identity(&self, output: &mut String) {
        match self {
            Self::Constant(value) => output.push(if *value { '1' } else { '0' }),
            Self::Atom { identity: atom, .. } => {
                output.push('a');
                output.push_str(&atom.len().to_string());
                output.push(':');
                output.push_str(atom);
            }
            Self::All(nested) => write_nested_identity('&', nested, output),
            Self::Any(nested) => write_nested_identity('|', nested, output),
            Self::Not(nested) => {
                output.push('!');
                nested.write_identity(output);
            }
        }
    }
}

fn write_nested_identity(kind: char, nested: &[Predicate], output: &mut String) {
    output.push(kind);
    output.push('[');
    for predicate in nested {
        predicate.write_identity(output);
        output.push(',');
    }
    output.push(']');
}

pub(in crate::structure) fn attributes_disable_production(attributes: &[Attribute]) -> Result<bool> {
    Ok(production_cfg_context(attributes, &ProductionCfgContext::default())?.is_none())
}

pub(in crate::structure) fn production_cfg_context(attributes: &[Attribute], inherited: &ProductionCfgContext) -> Result<Option<ProductionCfgContext>> {
    let mut context = inherited.clone();
    for attribute in attributes {
        if attribute.path().is_ident("cfg") {
            let meta = parse_single_meta(attribute).context("parse cfg predicate for line classification")?;
            context.constraints.push(predicate(&meta)?);
        } else if attribute.path().is_ident("cfg_attr")
            && let Meta::List(list) = &attribute.meta
        {
            collect_cfg_attr_constraints(&list.tokens, Predicate::Constant(true), &mut context.constraints)?;
        }
    }
    Ok(context_is_satisfiable(&context, None).then_some(context))
}

pub(in crate::structure) fn production_cfg_attr_metas(tokens: &proc_macro2::TokenStream, context: &ProductionCfgContext) -> Result<Vec<Meta>> {
    let mut metas = Vec::new();
    collect_production_cfg_attr_metas(tokens, context, Predicate::Constant(true), &mut metas)?;
    Ok(metas)
}

fn collect_cfg_attr_constraints(tokens: &proc_macro2::TokenStream, parent_activation: Predicate, output: &mut Vec<Predicate>) -> Result<()> {
    let attribute = parse_cfg_attr(tokens, "production constraint classification")?;
    let activation = Predicate::All(vec![parent_activation, predicate(&attribute.condition)?]);
    for meta in attribute.nested {
        if meta.path().is_ident("cfg") {
            let required = predicate(&parse_cfg_meta(&meta)?)?;
            output.push(Predicate::Any(vec![Predicate::Not(Box::new(activation.clone())), required]));
        } else if meta.path().is_ident("cfg_attr")
            && let Meta::List(list) = meta
        {
            collect_cfg_attr_constraints(&list.tokens, activation.clone(), output)?;
        }
    }
    Ok(())
}

fn collect_production_cfg_attr_metas(tokens: &proc_macro2::TokenStream, context: &ProductionCfgContext, parent_activation: Predicate, output: &mut Vec<Meta>) -> Result<()> {
    let attribute = parse_cfg_attr(tokens, "production attribute classification")?;
    let activation = Predicate::All(vec![parent_activation, predicate(&attribute.condition)?]);
    if !context_is_satisfiable(context, Some(activation.clone())) {
        return Ok(());
    }

    for meta in attribute.nested {
        if meta.path().is_ident("cfg") {
            continue;
        }
        if meta.path().is_ident("cfg_attr") {
            let Meta::List(list) = meta else {
                continue;
            };
            collect_production_cfg_attr_metas(&list.tokens, context, activation.clone(), output)?;
        } else {
            output.push(meta);
        }
    }
    Ok(())
}

fn parse_cfg_attr(tokens: &proc_macro2::TokenStream, label: &str) -> Result<CfgAttr> {
    let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(tokens.clone())
        .with_context(|| format!("parse cfg_attr arguments for {label}"))?;
    let mut arguments = arguments.into_iter();
    let condition = arguments.next().context("cfg_attr condition is required")?;
    Ok(CfgAttr {
        condition,
        nested: arguments.collect(),
    })
}

fn parse_cfg_meta(meta: &Meta) -> Result<Meta> {
    let Meta::List(list) = meta else {
        anyhow::bail!("nested cfg predicate must use list syntax");
    };
    let predicates = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .context("parse nested cfg_attr cfg predicate")?;
    if predicates.len() != 1 {
        anyhow::bail!("nested cfg attribute must contain exactly one predicate");
    }
    predicates.into_iter().next().context("nested cfg predicate disappeared")
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

fn context_is_satisfiable(context: &ProductionCfgContext, condition: Option<Predicate>) -> bool {
    let mut constraints = context.constraints.clone();
    constraints.extend(condition);
    any_assignment_satisfies(&Predicate::All(constraints))
}

fn predicate(meta: &Meta) -> Result<Predicate> {
    match meta {
        Meta::Path(path) if path.is_ident("test") => Ok(Predicate::Constant(false)),
        Meta::NameValue(value) if is_testing_feature(value) => Ok(Predicate::Constant(false)),
        Meta::List(list) if list.path.is_ident("all") => Ok(Predicate::All(parse_predicate_list(list)?)),
        Meta::List(list) if list.path.is_ident("any") => Ok(Predicate::Any(parse_predicate_list(list)?)),
        Meta::List(list) if list.path.is_ident("not") => {
            let mut predicates = parse_predicate_list(list)?;
            if predicates.len() != 1 {
                anyhow::bail!("cfg not predicate must contain exactly one argument");
            }
            Ok(Predicate::Not(Box::new(predicates.remove(0))))
        }
        Meta::Path(_) | Meta::NameValue(_) | Meta::List(_) => Ok(Predicate::Atom {
            identity: normalized_atom_identity(&meta.to_token_stream()),
            exclusive_group: exclusive_cfg_group(meta),
        }),
    }
}

fn exclusive_cfg_group(meta: &Meta) -> Option<String> {
    let Meta::NameValue(value) = meta else {
        return None;
    };
    let key = value.path.get_ident().map(normalized_ident)?;
    matches!(
        key.as_str(),
        "panic" | "target_abi" | "target_arch" | "target_endian" | "target_env" | "target_os" | "target_pointer_width" | "target_vendor"
    )
    .then_some(key)
}

fn normalized_atom_identity(tokens: &TokenStream) -> String {
    let mut identity = String::new();
    write_normalized_tokens(tokens, &mut identity);
    identity
}

fn write_normalized_tokens(tokens: &TokenStream, identity: &mut String) {
    for token in tokens.clone() {
        match token {
            TokenTree::Group(group) => {
                identity.push(match group.delimiter() {
                    Delimiter::Parenthesis => '(',
                    Delimiter::Brace => '{',
                    Delimiter::Bracket => '[',
                    Delimiter::None => '_',
                });
                write_normalized_tokens(&group.stream(), identity);
                identity.push(')');
            }
            TokenTree::Ident(ident) => write_identity_part('i', &normalized_ident(&ident), identity),
            TokenTree::Literal(literal) => write_normalized_literal(&literal, identity),
            TokenTree::Punct(punctuation) => {
                identity.push('p');
                identity.push(punctuation.as_char());
                identity.push(match punctuation.spacing() {
                    Spacing::Alone => 'a',
                    Spacing::Joint => 'j',
                });
            }
        }
    }
}

fn write_normalized_literal(literal: &proc_macro2::Literal, identity: &mut String) {
    let spelling = literal.to_string();
    if let Ok(syn::Lit::Str(value)) = syn::parse_str::<syn::Lit>(&spelling) {
        write_identity_part('s', &value.value(), identity);
    } else {
        write_identity_part('l', &spelling, identity);
    }
}

fn write_identity_part(kind: char, value: &str, identity: &mut String) {
    identity.push(kind);
    identity.push_str(&value.len().to_string());
    identity.push(':');
    identity.push_str(value);
}

fn parse_predicate_list(list: &syn::MetaList) -> Result<Vec<Predicate>> {
    Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .context("parse cfg predicate arguments")?
        .iter()
        .map(predicate)
        .collect()
}

fn is_testing_feature(value: &syn::MetaNameValue) -> bool {
    if !value.path.is_ident("feature") {
        return false;
    }
    let Expr::Lit(expression) = &value.value else {
        return false;
    };
    let syn::Lit::Str(feature) = &expression.lit else {
        return false;
    };
    feature.value() == "testing"
}

fn any_assignment_satisfies(predicate: &Predicate) -> bool {
    is_satisfiable(predicate)
}
