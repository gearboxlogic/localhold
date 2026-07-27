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
    let mut collector = TestCollector {
        excluded_context_depth: usize::from(TestCollector::execution_is_conditional_or_disabled(&syntax.attrs)),
        ..TestCollector::default()
    };
    collector.visit_file(&syntax);
    if !collector.tests.contains(test_name) {
        bail!("unsafe contract {contract_id:?} focused test {reference:?} does not name an unconditional, non-ignored explicit test function");
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
    excluded_context_depth: usize,
    tests: BTreeSet<String>,
}

impl TestCollector {
    fn is_test(attribute: &Attribute) -> bool {
        attribute.path().segments.last().is_some_and(|segment| segment.ident.unraw() == "test")
    }

    fn execution_is_conditional_or_disabled(attributes: &[Attribute]) -> bool {
        attributes.iter().any(|attribute| {
            attribute.path().segments.last().is_some_and(|segment| {
                let name = segment.ident.unraw();
                name == "cfg" || name == "cfg_attr" || name == "ignore"
            })
        })
    }
}

impl<'ast> Visit<'ast> for TestCollector {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if item.content.is_none() {
            return;
        }
        let excluded = Self::execution_is_conditional_or_disabled(&item.attrs);
        self.excluded_context_depth += usize::from(excluded);
        self.modules.push(item.ident.unraw().to_string());
        visit::visit_item_mod(self, item);
        self.modules.pop();
        self.excluded_context_depth -= usize::from(excluded);
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        if self.excluded_context_depth == 0 && !Self::execution_is_conditional_or_disabled(&function.attrs) && function.attrs.iter().any(Self::is_test) {
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

                #[test]
                #[ignore]
                fn ignored_case() {}

                #[cfg(any())]
                #[test]
                fn conditionally_excluded_case() {}

                #[cfg_attr(any(), ignore)]
                #[test]
                fn conditionally_configured_case() {}
            }

            #[cfg(any())]
            mod excluded_module {
                #[test]
                fn nested_case() {}
            }
            ",
        )
        .expect("focused test source");

        validate(workspace.path(), "contract.one", "tests/focused.rs::nested::contract_case").expect("existing test");
        for reference in [
            "tests/missing.rs::contract_case",
            "tests/focused.rs::missing",
            "tests/focused.rs::helper_is_not_a_test",
            "tests/focused.rs::nested::ignored_case",
            "tests/focused.rs::nested::conditionally_excluded_case",
            "tests/focused.rs::nested::conditionally_configured_case",
            "tests/focused.rs::excluded_module::nested_case",
            "../outside.rs::contract_case",
            "tests/focused.rs",
        ] {
            assert!(validate(workspace.path(), "contract.one", reference).is_err(), "reference must fail closed: {reference}");
        }

        fs::write(
            workspace.path().join("tests/excluded.rs"),
            "
            #![cfg(any())]
            #[test]
            fn excluded_file_case() {}
            ",
        )
        .expect("excluded focused test source");
        assert!(validate(workspace.path(), "contract.one", "tests/excluded.rs::excluded_file_case").is_err());
    }
}
