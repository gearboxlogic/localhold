use std::path::Path as FsPath;

use anyhow::{Context, Result, bail};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens as _;
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};
use syn::{Attribute, ItemExternCrate, ItemMod, ItemUse, Meta, Path as SynPath, UseTree};

use super::{TestLineCollector, normalized_ident};

pub fn production_internal_imports(file: &syn::File, source_path: &str, crate_root: Option<&str>, test_lines: &TestLineCollector) -> Result<Vec<String>> {
    let module = source_module(source_path, crate_root)?;
    let mut collector = ImportCollector {
        module,
        test_lines,
        imports: Vec::new(),
        error: None,
    };
    collector.visit_file(file);
    if let Some(error) = collector.error {
        return Err(error);
    }
    collector.imports.sort();
    collector.imports.dedup();
    Ok(collector.imports)
}

struct ImportCollector<'a> {
    module: Vec<String>,
    test_lines: &'a TestLineCollector,
    imports: Vec<String>,
    error: Option<anyhow::Error>,
}

impl ImportCollector<'_> {
    fn collect_use(&mut self, item: &ItemUse) -> Result<()> {
        if item.leading_colon.is_some() || self.test_lines.line_is_test(item.span().start().line) {
            return Ok(());
        }
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for path in paths {
            self.collect_path(&path)?;
        }
        Ok(())
    }

    fn collect_path(&mut self, path: &UsePath) -> Result<()> {
        self.collect_segments(&path.segments, path.renamed)
    }

    fn collect_segments(&mut self, segments: &[String], renamed: bool) -> Result<()> {
        let Some(resolved) = resolve_path(&self.module, segments)? else {
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
}

impl<'ast> Visit<'ast> for ImportCollector<'_> {
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
        if self.error.is_none() && !self.test_lines.line_is_test(item.span().start().line) && item.ident == "self" && item.rename.is_some() {
            self.error = Some(anyhow::anyhow!(
                "production crate-root extern aliases cannot be classified safely for dependency boundaries"
            ));
        }
    }

    fn visit_path(&mut self, path: &'ast SynPath) {
        if self.error.is_none() && path.leading_colon.is_none() && !self.test_lines.line_is_test(path.span().start().line) {
            let segments = path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
            let is_qualified = segments.len() > 1 || matches!(segments.first().map(String::as_str), Some("crate" | "self" | "super"));
            if is_qualified && let Err(error) = self.collect_segments(&segments, false) {
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
        if let Some(restricted) = restricted_macro_identifier(&node.tokens, self.test_lines) {
            self.error = Some(anyhow::anyhow!(
                "production macro token stream names restricted crate module {restricted:?} and cannot be classified safely"
            ));
            return;
        }
        self.visit_path(&node.path);
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if self.error.is_some() || self.test_lines.line_is_test(attribute.span().start().line) {
            return;
        }
        let tokens = match &attribute.meta {
            Meta::Path(_) => None,
            Meta::List(list) => Some(list.tokens.clone()),
            Meta::NameValue(value) => Some(value.value.to_token_stream()),
        };
        if let Some(tokens) = tokens
            && let Some(restricted) = restricted_macro_identifier(&tokens, self.test_lines)
        {
            self.error = Some(anyhow::anyhow!(
                "production attribute token stream names restricted crate module {restricted:?} and cannot be classified safely"
            ));
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

fn resolve_path(module: &[String], path: &[String]) -> Result<Option<Vec<String>>> {
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
    Ok(module.is_empty().then(|| path.to_vec()))
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

fn restricted_macro_identifier(tokens: &TokenStream, test_lines: &TestLineCollector) -> Option<String> {
    tokens.clone().into_iter().find_map(|token| match token {
        TokenTree::Group(group) => restricted_macro_identifier(&group.stream(), test_lines),
        TokenTree::Ident(ident) if !test_lines.line_is_test(ident.span().start().line) => {
            let normalized = normalized_ident(&ident);
            matches!(normalized.as_str(), "server" | "ui").then_some(normalized)
        }
        TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imports(path: &str, source: &str) -> Result<Vec<String>> {
        let syntax = syn::parse_file(source)?;
        let mut test_lines = TestLineCollector::new(source.lines().count());
        test_lines.visit_file(&syntax)?;
        production_internal_imports(&syntax, path, Some("src/lib.rs"), &test_lines)
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
        let mut test_lines = TestLineCollector::new(1);
        test_lines.visit_file(&syntax)?;
        assert_eq!(
            production_internal_imports(&syntax, "src/core.rs", Some("src/core.rs"), &test_lines)?,
            ["crate::server::AtRoot"]
        );
        Ok(())
    }

    #[test]
    fn nested_custom_library_roots_define_relative_module_paths() -> Result<()> {
        let syntax = syn::parse_file("use super::server::Service;\n")?;
        let mut test_lines = TestLineCollector::new(1);
        test_lines.visit_file(&syntax)?;
        assert_eq!(
            production_internal_imports(&syntax, "src/core/worker.rs", Some("src/core/lib.rs"), &test_lines)?,
            ["crate::server::Service"]
        );
        Ok(())
    }

    #[test]
    fn restricted_names_in_production_macro_tokens_fail_closed() {
        let source = "macro_rules! dependency { () => { use crate::server::Service; } }\n";
        assert!(imports("src/adapter.rs", source).unwrap_err().to_string().contains("production macro token stream"));

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
    fn absolute_external_paths_are_not_classified_as_crate_relative() {
        let source = "use ::server::External;\n\
                      use ::ui::External as ExternalUi;\n\
                      fn build() -> ::server::Qualified { ::ui::qualified() }\n";
        assert!(imports("src/adapter.rs", source).expect("absolute external paths").is_empty());
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
}
