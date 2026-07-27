use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use syn::ext::IdentExt as _;
use syn::visit::{self, Visit};
use syn::{Attribute, ItemFn, ItemMod};

use super::{REQUIRED_ROOTS, validate_relative_rust_path};

pub(super) fn validate(workspace: &Path, contract_id: &str, reference: &str) -> Result<()> {
    let (source_path, test_name) = reference
        .split_once("::")
        .with_context(|| format!("unsafe contract {contract_id:?} focused test {reference:?} must use path.rs::test_name"))?;
    validate_relative_rust_path(source_path).with_context(|| format!("unsafe contract {contract_id:?} focused test source"))?;
    let first = Path::new(source_path).components().next();
    if !matches!(first, Some(Component::Normal(root)) if root.to_str().is_some_and(|root| REQUIRED_ROOTS.contains(&root))) {
        bail!("unsafe contract {contract_id:?} focused test source {source_path:?} must remain under an audited root");
    }
    validate_test_name(test_name).with_context(|| format!("unsafe contract {contract_id:?} focused test name {test_name:?}"))?;

    let workspace = fs::canonicalize(workspace).with_context(|| format!("resolve focused-test workspace {}", workspace.display()))?;
    let source = workspace.join(source_path);
    let canonical = fs::canonicalize(&source).with_context(|| format!("resolve unsafe contract {contract_id:?} focused test source {}", source.display()))?;
    let relative = canonical
        .strip_prefix(&workspace)
        .with_context(|| format!("unsafe contract {contract_id:?} focused test source escaped the workspace: {}", canonical.display()))?;
    if relative != Path::new(source_path) {
        bail!("unsafe contract {contract_id:?} focused test source {source_path:?} must resolve to that exact audited path");
    }

    let source_text = fs::read_to_string(&canonical).with_context(|| format!("read unsafe contract {contract_id:?} focused test source {}", canonical.display()))?;
    let syntax = syn::parse_file(&source_text).with_context(|| format!("parse unsafe contract {contract_id:?} focused test source {}", canonical.display()))?;
    let mut collector = TestCollector::default();
    collector.visit_file(&syntax);
    if !collector.tests.contains(test_name) {
        bail!("unsafe contract {contract_id:?} focused test {reference:?} does not name an explicit test function");
    }
    Ok(())
}

fn validate_test_name(test_name: &str) -> Result<()> {
    let path: syn::Path = syn::parse_str(test_name).context("test name must be a Rust path")?;
    if path.leading_colon.is_some() || path.segments.is_empty() || path.segments.iter().any(|segment| !matches!(segment.arguments, syn::PathArguments::None)) {
        bail!("test name must be a normalized relative Rust path");
    }
    Ok(())
}

#[derive(Default)]
struct TestCollector {
    modules: Vec<String>,
    tests: BTreeSet<String>,
}

impl TestCollector {
    fn is_test(attribute: &Attribute) -> bool {
        attribute.path().segments.last().is_some_and(|segment| segment.ident.unraw() == "test")
    }
}

impl<'ast> Visit<'ast> for TestCollector {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if item.content.is_none() {
            return;
        }
        self.modules.push(item.ident.unraw().to_string());
        visit::visit_item_mod(self, item);
        self.modules.pop();
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        if function.attrs.iter().any(Self::is_test) {
            let mut path = self.modules.clone();
            path.push(function.sig.ident.unraw().to_string());
            self.tests.insert(path.join("::"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::validate;

    #[test]
    fn focused_test_reference_requires_an_existing_explicit_test() {
        let workspace = tempdir().expect("temporary workspace");
        fs::create_dir(workspace.path().join("tests")).expect("tests root");
        fs::write(
            workspace.path().join("tests/focused.rs"),
            "
            fn helper_is_not_a_test() {}
            mod nested {
                #[test]
                fn contract_case() {}
            }
            ",
        )
        .expect("focused test source");

        validate(workspace.path(), "contract.one", "tests/focused.rs::nested::contract_case").expect("existing test");
        for reference in [
            "tests/missing.rs::contract_case",
            "tests/focused.rs::missing",
            "tests/focused.rs::helper_is_not_a_test",
            "../outside.rs::contract_case",
            "tests/focused.rs",
        ] {
            assert!(validate(workspace.path(), "contract.one", reference).is_err(), "reference must fail closed: {reference}");
        }
    }
}
