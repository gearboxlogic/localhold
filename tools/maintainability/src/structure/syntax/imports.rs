use anyhow::{Context, Result, bail};
use syn::spanned::Spanned as _;
use syn::visit::Visit;
use syn::{ItemExternCrate, ItemMod, ItemUse, UseTree};

use super::TestLineCollector;

pub fn production_internal_imports(file: &syn::File, source_path: &str, test_lines: &TestLineCollector) -> Result<Vec<String>> {
    let module = source_module(source_path)?;
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
        if self.test_lines.line_is_test(item.span().start().line) {
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
        let resolved = resolve_path(&self.module, &path.segments)?;
        if resolved.is_empty() && path.renamed {
            bail!("production crate-root import aliases cannot be classified safely for dependency boundaries");
        }
        if matches!(resolved.first().map(String::as_str), Some("server" | "ui")) {
            self.imports.push(format!("crate::{}", resolved.join("::")));
        }
        Ok(())
    }
}

impl<'ast> Visit<'ast> for ImportCollector<'_> {
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        if self.error.is_none()
            && let Err(error) = self.collect_use(item)
        {
            self.error = Some(error);
        }
    }

    fn visit_item_extern_crate(&mut self, item: &'ast ItemExternCrate) {
        if self.error.is_none() && !self.test_lines.line_is_test(item.span().start().line) && item.ident == "self" && item.rename.is_some() {
            self.error = Some(anyhow::anyhow!(
                "production crate-root extern aliases cannot be classified safely for dependency boundaries"
            ));
        }
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
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

fn resolve_path(module: &[String], path: &[String]) -> Result<Vec<String>> {
    let Some(first) = path.first().map(String::as_str) else {
        bail!("production use path has no segments");
    };
    if first == "crate" {
        return Ok(path[1..].to_vec());
    }
    if first == "self" {
        let mut resolved = module.to_vec();
        resolved.extend_from_slice(&path[1..]);
        return Ok(resolved);
    }
    if first == "super" {
        let mut resolved = module.to_vec();
        let mut consumed = 0;
        while path.get(consumed).is_some_and(|segment| segment == "super") {
            resolved.pop().context("production use path escapes its crate root")?;
            consumed += 1;
        }
        resolved.extend_from_slice(&path[consumed..]);
        return Ok(resolved);
    }
    Ok(path.to_vec())
}

fn source_module(source_path: &str) -> Result<Vec<String>> {
    let relative = source_path.strip_prefix("src/").context("production internal imports must originate under src/")?;
    let mut parts = relative.split('/').map(str::to_owned).collect::<Vec<_>>();
    let file = parts.pop().context("production Rust source has no filename")?;
    let stem = file.strip_suffix(".rs").context("production source path is not a Rust file")?;
    if !matches!(stem, "lib" | "main" | "mod") {
        parts.push(stem.to_owned());
    }
    Ok(parts)
}

fn normalized_ident(ident: &syn::Ident) -> String {
    let value = ident.to_string();
    value.strip_prefix("r#").unwrap_or(&value).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imports(path: &str, source: &str) -> Result<Vec<String>> {
        let syntax = syn::parse_file(source)?;
        let mut test_lines = TestLineCollector::new(source.lines().count());
        test_lines.visit_file(&syntax)?;
        production_internal_imports(&syntax, path, &test_lines)
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
                      use crate::server::LocalHoldServer;\n";
        assert_eq!(imports("src/http_transport.rs", source).expect("imports"), ["crate::server::LocalHoldServer"]);
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
    }
}
