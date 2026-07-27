use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use proc_macro2::Span;
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use syn::ext::IdentExt as _;
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};
use syn::{
    Attribute, ExprUnsafe, ForeignItemFn, ForeignItemStatic, ImplItemFn, ItemExternCrate, ItemFn, ItemForeignMod, ItemImpl, ItemMacro, ItemMod, ItemStatic, ItemTrait, ItemUse,
    Macro, StaticMutability, TraitItemFn,
};

use self::documentation::unsupported_runnable_doctest;
use self::files::{collect_optional as collect_optional_rust_files, collect_required as collect_rust_files};
use self::policy::{
    contains_assembly_macro, contains_opaque_attribute, contains_path_attribute, contains_structural_ident, contains_unaudited_macro_syntax, is_path_override,
    is_reserved_expansion_root, is_safety_lint_exception, is_standalone_assembly_macro, is_trusted_attribute, is_trusted_local_macro_name, is_trusted_macro, is_unsafe_attribute,
    macro_name, untrusted_import,
};

pub const REVIEWED_EXPANSION_PACKAGES: [&str; 14] = [
    "criterion",
    "futures",
    "insta",
    "ort",
    "proptest",
    "rand",
    "rmcp",
    "rusqlite",
    "schemars",
    "serde",
    "serde_json",
    "thiserror",
    "tokio",
    "tracing",
];
pub const RESERVED_LOCAL_MACROS: [&str; 5] = ["concat_placeholders", "concat_with_sep", "define_memory_columns", "numbered_placeholders", "transport_test"];

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
    #[serde(skip_serializing)]
    pub source_range: SourceRange,
}

impl UnsafeSite {
    pub fn locator(&self) -> (&str, &str, SiteKind, u32) {
        (&self.path, &self.item, self.kind, self.occurrence)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceRange {
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

impl SourceRange {
    fn from_span(span: Span) -> Self {
        let start = span.start();
        let end = span.end();
        Self {
            start_line: start.line,
            start_column: start.column + 1,
            end_line: end.line,
            end_column: end.column + 1,
        }
    }

    pub fn contains(self, line: usize, column: usize) -> bool {
        let point = (line, column);
        let start = (self.start_line, self.start_column);
        let end = (self.end_line, self.end_column);
        point >= start && (point < end || start == end && point == start)
    }

    pub const fn width(self) -> (usize, usize) {
        (
            self.end_line.saturating_sub(self.start_line),
            if self.start_line == self.end_line {
                self.end_column.saturating_sub(self.start_column)
            } else {
                self.end_column
            },
        )
    }
}

#[derive(Debug)]
struct PendingSite {
    item: String,
    kind: SiteKind,
    fingerprint: String,
    boundary_fingerprint: String,
    source_range: SourceRange,
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
        if let Some(reason) = unsupported_runnable_doctest(&source) {
            violations.push(format!("{relative}: {reason}"));
            continue;
        }
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
                source_range: pending.source_range,
            });
            *occurrence += 1;
        }
    }
    if !violations.is_empty() {
        bail!("unsupported or unaudited Rust source construct:\n{}", violations.join("\n"));
    }
    Ok(sites)
}

#[derive(Default)]
struct SourceScanner {
    scopes: Vec<String>,
    boundaries: Vec<String>,
    unsafe_context_depth: usize,
    sites: Vec<PendingSite>,
    violations: Vec<String>,
}

impl SourceScanner {
    fn item(&self) -> String {
        self.scopes.last().cloned().unwrap_or_else(|| "<module>".to_owned())
    }

    fn push_site(&mut self, kind: SiteKind, tokens: &impl ToTokens, span: Span) {
        let site_fingerprint = fingerprint(tokens);
        self.sites.push(PendingSite {
            item: self.item(),
            kind,
            boundary_fingerprint: self.boundaries.last().cloned().unwrap_or_else(|| site_fingerprint.clone()),
            fingerprint: site_fingerprint,
            source_range: SourceRange::from_span(span),
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

    fn with_unsafe_context(&mut self, visit: impl FnOnce(&mut Self)) {
        self.unsafe_context_depth += 1;
        visit(self);
        self.unsafe_context_depth -= 1;
    }

    fn child_scope(&self, name: &str) -> String {
        self.scopes.last().map_or_else(|| name.to_owned(), |parent| format!("{parent}::{name}"))
    }
}

impl<'ast> Visit<'ast> for SourceScanner {
    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if attribute.path().segments.len() == 1
            && attribute.path().segments[0].ident.unraw() == "doc"
            && !attribute.span().source_text().is_some_and(|source| {
                let source = source.trim_start();
                source.starts_with("///") || source.starts_with("//!") || source.starts_with("/**") || source.starts_with("/*!")
            })
        {
            self.violations
                .push(format!("{} uses an explicit #[doc] attribute, whose doctest content is not audited", self.item()));
        }
        if policy::contains_token_paste_syntax(&attribute.meta.to_token_stream()) {
            self.violations.push(format!("{} uses opaque token-pasting attribute input", self.item()));
        }
        if !is_trusted_attribute(attribute) {
            self.violations
                .push(format!("{} uses an unreviewed attribute path {}", self.item(), attribute.path().to_token_stream()));
        }
        if is_safety_lint_exception(attribute) {
            self.push_site(SiteKind::LintException, attribute, attribute.span());
        }
        if is_unsafe_attribute(attribute) {
            self.push_site(SiteKind::Attribute, attribute, attribute.span());
        }
        if is_path_override(attribute) {
            self.violations.push(format!("{} uses #[path] or cfg_attr(..., path = ...)", self.item()));
        }
        visit::visit_attribute(self, attribute);
    }

    fn visit_expr_unsafe(&mut self, expression: &'ast ExprUnsafe) {
        self.push_site(SiteKind::Block, expression, expression.unsafe_token.span);
        self.with_unsafe_context(|scanner| visit::visit_expr_unsafe(scanner, expression));
    }

    fn visit_macro(&mut self, macro_invocation: &'ast Macro) {
        let name = macro_name(macro_invocation);
        if policy::contains_token_paste_syntax(&macro_invocation.tokens) {
            self.violations.push(format!("{} uses opaque token-pasting macro input", self.item()));
        }
        if name.as_deref() == Some("include") || contains_structural_ident(macro_invocation.tokens.clone(), "include") {
            self.violations.push(format!("{} uses include! code expansion", self.item()));
        }
        if contains_path_attribute(macro_invocation.tokens.clone()) {
            self.violations.push(format!("{} generates a #[path] source override", self.item()));
        }
        if contains_opaque_attribute(macro_invocation.tokens.clone()) {
            self.violations
                .push(format!("{} uses opaque attribute construction that could bypass the safety inventory", self.item()));
        }
        if self.unsafe_context_depth > 0 {
            self.violations
                .push(format!("{} invokes a macro inside an unsafe context, whose expansion cannot be audited", self.item()));
        } else if is_standalone_assembly_macro(macro_invocation) {
            self.push_site(SiteKind::MacroInput, macro_invocation, macro_invocation.span());
        } else if !is_trusted_macro(macro_invocation) {
            self.violations
                .push(format!("{} invokes an unreviewed macro path {}", self.item(), macro_invocation.path.to_token_stream()));
        } else if contains_unaudited_macro_syntax(&macro_invocation.tokens) {
            self.violations
                .push(format!("{} uses macro-generated unsafe, assembly, mutable-static, or safety-lint syntax", self.item()));
        }
        visit::visit_macro(self, macro_invocation);
    }

    fn visit_item_macro(&mut self, item: &'ast ItemMacro) {
        if macro_name(&item.mac).as_deref() == Some("macro_rules") && !item.ident.as_ref().is_some_and(|name| is_trusted_local_macro_name(&name.to_string())) {
            self.violations.push(format!("{} defines an unreviewed local macro", self.item()));
        }
        visit::visit_item_macro(self, item);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        let tokens = item.to_token_stream();
        if let Some(reason) = untrusted_import(item) {
            self.violations.push(format!("{} uses {reason}", self.item()));
        }
        if contains_structural_ident(tokens.clone(), "include") {
            self.violations
                .push(format!("{} imports include!, which can hide source expansion behind an alias", self.item()));
        }
        if contains_assembly_macro(&tokens) {
            self.violations
                .push(format!("{} imports an assembly macro; invoke it through its explicit qualified path", self.item()));
        }
        visit::visit_item_use(self, item);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast ItemExternCrate) {
        self.violations.push(format!(
            "{} uses extern crate {}, whose alias and macro import semantics are not supported",
            self.item(),
            item.ident
        ));
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if is_reserved_expansion_root(&item.ident.unraw().to_string()) {
            self.violations
                .push(format!("{} declares module {}, which shadows a reviewed expansion package", self.item(), item.ident));
        }
        let scope = self.child_scope(&item.ident.to_string());
        self.visit_scoped(scope, item, |scanner, item| visit::visit_item_mod(scanner, item));
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let scope = self.child_scope(&function.sig.ident.to_string());
        self.visit_boundary(scope, function, |scanner, function| {
            if let Some(unsafety) = &function.sig.unsafety {
                scanner.push_site(SiteKind::Function, &function.sig, unsafety.span);
                scanner.with_unsafe_context(|scanner| visit::visit_item_fn(scanner, function));
            } else {
                visit::visit_item_fn(scanner, function);
            }
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
            if let Some(unsafety) = &implementation.unsafety {
                scanner.push_site(SiteKind::Impl, implementation, unsafety.span);
            }
            visit::visit_item_impl(scanner, implementation);
        });
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let scope = self.child_scope(&function.sig.ident.to_string());
        self.visit_boundary(scope, function, |scanner, function| {
            if let Some(unsafety) = &function.sig.unsafety {
                scanner.push_site(SiteKind::Function, &function.sig, unsafety.span);
                scanner.with_unsafe_context(|scanner| visit::visit_impl_item_fn(scanner, function));
            } else {
                visit::visit_impl_item_fn(scanner, function);
            }
        });
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        let scope = self.child_scope(&item.ident.to_string());
        self.visit_boundary(scope, item, |scanner, item| {
            if let Some(unsafety) = &item.unsafety {
                scanner.push_site(SiteKind::Trait, item, unsafety.span);
            }
            visit::visit_item_trait(scanner, item);
        });
    }

    fn visit_trait_item_fn(&mut self, function: &'ast TraitItemFn) {
        let scope = self.child_scope(&function.sig.ident.to_string());
        self.visit_boundary(scope, function, |scanner, function| {
            if let Some(unsafety) = &function.sig.unsafety {
                scanner.push_site(SiteKind::Function, &function.sig, unsafety.span);
                scanner.with_unsafe_context(|scanner| visit::visit_trait_item_fn(scanner, function));
            } else {
                visit::visit_trait_item_fn(scanner, function);
            }
        });
    }

    fn visit_item_foreign_mod(&mut self, item: &'ast ItemForeignMod) {
        let scope = self.child_scope("<extern>");
        self.visit_boundary(scope, item, |scanner, item| {
            if let Some(unsafety) = &item.unsafety {
                scanner.push_site(SiteKind::ExternBlock, item, unsafety.span);
            }
            visit::visit_item_foreign_mod(scanner, item);
        });
    }

    fn visit_item_static(&mut self, item: &'ast ItemStatic) {
        let scope = self.child_scope(&item.ident.to_string());
        self.visit_boundary(scope, item, |scanner, item| {
            if matches!(item.mutability, StaticMutability::Mut(_)) {
                scanner.push_site(SiteKind::MutableStatic, item, item.span());
            }
            visit::visit_item_static(scanner, item);
        });
    }

    fn visit_foreign_item_fn(&mut self, function: &'ast ForeignItemFn) {
        let scope = self.child_scope(&function.sig.ident.to_string());
        self.visit_boundary(scope, function, |scanner, function| {
            if let Some(unsafety) = &function.sig.unsafety {
                scanner.push_site(SiteKind::Function, &function.sig, unsafety.span);
            }
            visit::visit_foreign_item_fn(scanner, function);
        });
    }

    fn visit_foreign_item_static(&mut self, item: &'ast ForeignItemStatic) {
        let scope = self.child_scope(&item.ident.to_string());
        self.visit_boundary(scope, item, |scanner, item| {
            if matches!(item.mutability, StaticMutability::Mut(_)) {
                scanner.push_site(SiteKind::MutableStatic, item, item.span());
            }
            visit::visit_foreign_item_static(scanner, item);
        });
    }
}

fn normalized_tokens(tokens: &impl ToTokens) -> String {
    tokens.to_token_stream().to_string()
}

fn fingerprint(tokens: &impl ToTokens) -> String {
    format!("{:x}", Sha256::digest(normalized_tokens(tokens).as_bytes()))
}

mod documentation;
mod files;
mod policy;
#[cfg(test)]
mod tests;
