use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use syn::ext::IdentExt as _;
use syn::visit::{self, Visit};
use syn::{
    Attribute, ExprUnsafe, ForeignItemFn, ForeignItemStatic, ImplItemFn, ItemFn, ItemForeignMod, ItemImpl, ItemMod, ItemStatic, ItemTrait, ItemUse, Macro, StaticMutability,
    TraitItemFn,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SiteKind {
    #[serde(rename = "unsafe-block")]
    Block,
    #[serde(rename = "unsafe-function")]
    Function,
    #[serde(rename = "unsafe-trait")]
    Trait,
    #[serde(rename = "unsafe-impl")]
    Impl,
    #[serde(rename = "unsafe-extern-block")]
    ExternBlock,
    #[serde(rename = "unsafe-macro-input")]
    MacroInput,
    #[serde(rename = "unsafe-attribute")]
    Attribute,
    #[serde(rename = "mutable-static")]
    MutableStatic,
    #[serde(rename = "safety-lint-exception")]
    LintException,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UnsafeSite {
    pub path: String,
    pub item: String,
    pub kind: SiteKind,
    pub occurrence: u32,
    pub fingerprint: String,
    pub boundary_fingerprint: String,
}

impl UnsafeSite {
    pub fn locator(&self) -> (&str, &str, SiteKind, u32) {
        (&self.path, &self.item, self.kind, self.occurrence)
    }
}

#[derive(Debug)]
struct PendingSite {
    item: String,
    kind: SiteKind,
    fingerprint: String,
    boundary_fingerprint: String,
}

pub fn scan_workspace(workspace: &Path, roots: &[String]) -> Result<Vec<UnsafeSite>> {
    let mut files = Vec::new();
    for root in roots {
        collect_rust_files(&workspace.join(root), &mut files)?;
    }
    collect_optional_rust_files(&workspace.join("examples"), &mut files)?;
    if workspace.join("build.rs").try_exists().context("inspect optional root build.rs")? {
        bail!("root build.rs is not supported by the source-safety gate; its module graph could escape the audited roots");
    }
    files.sort();

    let mut sites = Vec::new();
    let mut occurrences = BTreeMap::new();
    let mut violations = Vec::new();
    for path in files {
        let relative = path.strip_prefix(workspace).context("source path escaped workspace")?;
        let relative = relative.to_str().context("source path is not UTF-8")?.replace('\\', "/");
        let source = fs::read_to_string(&path).with_context(|| format!("read Rust source {}", path.display()))?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse Rust source {}", path.display()))?;
        let mut scanner = SourceScanner::default();
        scanner.visit_file(&syntax);
        if !scanner.violations.is_empty() {
            violations.push(format!("{relative}: {}", scanner.violations.join("; ")));
            continue;
        }
        for pending in scanner.sites {
            let key = (relative.clone(), pending.item.clone(), pending.kind);
            let occurrence = occurrences.entry(key).or_insert(0);
            sites.push(UnsafeSite {
                path: relative.clone(),
                item: pending.item,
                kind: pending.kind,
                occurrence: *occurrence,
                fingerprint: pending.fingerprint,
                boundary_fingerprint: pending.boundary_fingerprint,
            });
            *occurrence += 1;
        }
    }
    if !violations.is_empty() {
        bail!("unsupported Rust source inclusion:\n{}", violations.join("\n"));
    }
    Ok(sites)
}

fn collect_optional_rust_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(_) => collect_rust_files(root, files),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect optional source root {}", root.display())),
    }
}

fn collect_rust_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(root).with_context(|| format!("inspect tracked source root {}", root.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("tracked source root cannot be a symlink: {}", root.display());
    }
    if !metadata.is_dir() {
        bail!("tracked source root is not a directory: {}", root.display());
    }
    let mut entries = fs::read_dir(root)
        .with_context(|| format!("read tracked source directory {}", root.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().with_context(|| format!("inspect tracked source entry {}", path.display()))?;
        if file_type.is_symlink() {
            bail!("tracked Rust source tree cannot contain symlinks: {}", path.display());
        }
        if file_type.is_dir() {
            collect_rust_files(&path, files)?;
        } else if file_type.is_file() && path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

#[derive(Default)]
struct SourceScanner {
    scopes: Vec<String>,
    boundaries: Vec<String>,
    sites: Vec<PendingSite>,
    violations: Vec<String>,
}

impl SourceScanner {
    fn item(&self) -> String {
        self.scopes.last().cloned().unwrap_or_else(|| "<module>".to_owned())
    }

    fn push_site(&mut self, kind: SiteKind, tokens: &impl ToTokens) {
        let site_fingerprint = fingerprint(tokens);
        self.sites.push(PendingSite {
            item: self.item(),
            kind,
            boundary_fingerprint: self.boundaries.last().cloned().unwrap_or_else(|| site_fingerprint.clone()),
            fingerprint: site_fingerprint,
        });
    }

    fn visit_scoped<T>(&mut self, scope: String, node: &T, visit: impl FnOnce(&mut Self, &T)) {
        self.scopes.push(scope);
        visit(self, node);
        self.scopes.pop();
    }

    fn visit_boundary<T: ToTokens>(&mut self, scope: String, node: &T, visit: impl FnOnce(&mut Self, &T)) {
        self.boundaries.push(fingerprint(node));
        self.visit_scoped(scope, node, visit);
        self.boundaries.pop();
    }

    fn child_scope(&self, name: &str) -> String {
        self.scopes.last().map_or_else(|| name.to_owned(), |parent| format!("{parent}::{name}"))
    }
}

impl<'ast> Visit<'ast> for SourceScanner {
    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if is_safety_lint_exception(attribute) {
            self.push_site(SiteKind::LintException, attribute);
        }
        if is_unsafe_attribute(attribute) {
            self.push_site(SiteKind::Attribute, attribute);
        }
        if is_path_override(attribute) {
            self.violations.push(format!("{} uses #[path] or cfg_attr(..., path = ...)", self.item()));
        }
        visit::visit_attribute(self, attribute);
    }

    fn visit_expr_unsafe(&mut self, expression: &'ast ExprUnsafe) {
        self.push_site(SiteKind::Block, expression);
        visit::visit_expr_unsafe(self, expression);
    }

    fn visit_macro(&mut self, macro_invocation: &'ast Macro) {
        let macro_name = macro_invocation.path.segments.last().map(|segment| segment.ident.unraw().to_string());
        if macro_name.as_deref() == Some("include") || contains_structural_ident(macro_invocation.tokens.clone(), "include") {
            self.violations.push(format!("{} uses include! code expansion", self.item()));
        }
        if contains_path_attribute(macro_invocation.tokens.clone()) {
            self.violations.push(format!("{} generates a #[path] source override", self.item()));
        }
        if contains_opaque_attribute(macro_invocation.tokens.clone()) {
            self.violations
                .push(format!("{} uses opaque attribute construction that could bypass the safety inventory", self.item()));
        }
        let unsafe_assembly = ["global_asm", "llvm_asm", "naked_asm"]
            .into_iter()
            .any(|name| macro_name.as_deref() == Some(name) || contains_structural_ident(macro_invocation.tokens.clone(), name));
        if contains_macro_safety_lint_exception(macro_invocation.tokens.clone()) {
            self.push_site(SiteKind::LintException, macro_invocation);
        }
        if contains_plain_ident(macro_invocation.tokens.clone(), "unsafe") || unsafe_assembly || contains_static_item_token(macro_invocation.tokens.clone()) {
            self.push_site(SiteKind::MacroInput, macro_invocation);
        }
        visit::visit_macro(self, macro_invocation);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        let tokens = item.to_token_stream();
        if contains_structural_ident(tokens.clone(), "include") {
            self.violations
                .push(format!("{} imports include!, which can hide source expansion behind an alias", self.item()));
        }
        if ["global_asm", "llvm_asm", "naked_asm"]
            .into_iter()
            .any(|name| contains_structural_ident(tokens.clone(), name))
        {
            self.push_site(SiteKind::MacroInput, item);
        }
        visit::visit_item_use(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let scope = self.child_scope(&item.ident.to_string());
        self.visit_scoped(scope, item, |scanner, item| visit::visit_item_mod(scanner, item));
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let scope = self.child_scope(&function.sig.ident.to_string());
        self.visit_boundary(scope, function, |scanner, function| {
            if function.sig.unsafety.is_some() {
                scanner.push_site(SiteKind::Function, &function.sig);
            }
            visit::visit_item_fn(scanner, function);
        });
    }

    fn visit_item_impl(&mut self, implementation: &'ast ItemImpl) {
        let type_name = normalized_tokens(&implementation.self_ty);
        let scope = match &implementation.trait_ {
            Some((_, trait_path, _)) => format!("<{type_name} as {}>", normalized_tokens(trait_path)),
            None => type_name,
        };
        let scope = self.child_scope(&scope);
        self.visit_boundary(scope, implementation, |scanner, implementation| {
            if implementation.unsafety.is_some() {
                scanner.push_site(SiteKind::Impl, implementation);
            }
            visit::visit_item_impl(scanner, implementation);
        });
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let scope = self.child_scope(&function.sig.ident.to_string());
        self.visit_boundary(scope, function, |scanner, function| {
            if function.sig.unsafety.is_some() {
                scanner.push_site(SiteKind::Function, &function.sig);
            }
            visit::visit_impl_item_fn(scanner, function);
        });
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        let scope = self.child_scope(&item.ident.to_string());
        self.visit_boundary(scope, item, |scanner, item| {
            if item.unsafety.is_some() {
                scanner.push_site(SiteKind::Trait, item);
            }
            visit::visit_item_trait(scanner, item);
        });
    }

    fn visit_trait_item_fn(&mut self, function: &'ast TraitItemFn) {
        let scope = self.child_scope(&function.sig.ident.to_string());
        self.visit_boundary(scope, function, |scanner, function| {
            if function.sig.unsafety.is_some() {
                scanner.push_site(SiteKind::Function, &function.sig);
            }
            visit::visit_trait_item_fn(scanner, function);
        });
    }

    fn visit_item_foreign_mod(&mut self, item: &'ast ItemForeignMod) {
        let scope = self.child_scope("<extern>");
        self.visit_boundary(scope, item, |scanner, item| {
            if item.unsafety.is_some() {
                scanner.push_site(SiteKind::ExternBlock, item);
            }
            visit::visit_item_foreign_mod(scanner, item);
        });
    }

    fn visit_item_static(&mut self, item: &'ast ItemStatic) {
        let scope = self.child_scope(&item.ident.to_string());
        self.visit_boundary(scope, item, |scanner, item| {
            if matches!(item.mutability, StaticMutability::Mut(_)) {
                scanner.push_site(SiteKind::MutableStatic, item);
            }
            visit::visit_item_static(scanner, item);
        });
    }

    fn visit_foreign_item_fn(&mut self, function: &'ast ForeignItemFn) {
        let scope = self.child_scope(&function.sig.ident.to_string());
        self.visit_boundary(scope, function, |scanner, function| {
            if function.sig.unsafety.is_some() {
                scanner.push_site(SiteKind::Function, &function.sig);
            }
            visit::visit_foreign_item_fn(scanner, function);
        });
    }

    fn visit_foreign_item_static(&mut self, item: &'ast ForeignItemStatic) {
        let scope = self.child_scope(&item.ident.to_string());
        self.visit_boundary(scope, item, |scanner, item| {
            if matches!(item.mutability, StaticMutability::Mut(_)) {
                scanner.push_site(SiteKind::MutableStatic, item);
            }
            visit::visit_foreign_item_static(scanner, item);
        });
    }
}

fn is_safety_lint_exception(attribute: &Attribute) -> bool {
    let path = attribute.path().segments.last().map(|segment| segment.ident.unraw().to_string());
    let tokens = attribute.meta.to_token_stream();
    match path.as_deref() {
        Some("allow" | "expect" | "warn") => contains_safety_lint_name(&tokens),
        Some("cfg_attr") => contains_safety_lint_name(&tokens) && ["allow", "expect", "warn"].into_iter().any(|level| contains_structural_ident(tokens.clone(), level)),
        _ => false,
    }
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

fn contains_static_item_token(tokens: TokenStream) -> bool {
    contains_structural_ident(tokens, "static")
}

fn is_unsafe_attribute(attribute: &Attribute) -> bool {
    let path = attribute.path().segments.last().map(|segment| segment.ident.unraw().to_string());
    path.as_deref() == Some("unsafe") || (path.as_deref() == Some("cfg_attr") && contains_structural_ident(attribute.meta.to_token_stream(), "unsafe"))
}

fn is_path_override(attribute: &Attribute) -> bool {
    let path = attribute.path().segments.last().map(|segment| segment.ident.unraw().to_string());
    path.as_deref() == Some("path") || (path.as_deref() == Some("cfg_attr") && contains_structural_ident(attribute.meta.to_token_stream(), "path"))
}

fn contains_plain_ident(tokens: TokenStream, expected: &str) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => contains_plain_ident(group.stream(), expected),
        TokenTree::Ident(identifier) => identifier == expected,
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn contains_structural_ident(tokens: TokenStream, expected: &str) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => contains_structural_ident(group.stream(), expected),
        TokenTree::Ident(identifier) => identifier.unraw() == expected,
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn contains_opaque_attribute(tokens: TokenStream) -> bool {
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

fn contains_punctuation(tokens: TokenStream, expected: char) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => contains_punctuation(group.stream(), expected),
        TokenTree::Punct(punctuation) => punctuation.as_char() == expected,
        TokenTree::Ident(_) | TokenTree::Literal(_) => false,
    })
}

fn contains_path_attribute(tokens: TokenStream) -> bool {
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

fn first_ident_is(tokens: TokenStream, expected: &str) -> bool {
    matches!(tokens.into_iter().next(), Some(TokenTree::Ident(identifier)) if identifier.unraw() == expected)
}

fn normalized_tokens(tokens: &impl ToTokens) -> String {
    tokens.to_token_stream().to_string()
}

fn fingerprint(tokens: &impl ToTokens) -> String {
    format!("{:x}", Sha256::digest(normalized_tokens(tokens).as_bytes()))
}

#[cfg(test)]
mod tests;
