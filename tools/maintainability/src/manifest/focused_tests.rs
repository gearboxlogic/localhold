use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use syn::ext::IdentExt as _;
use syn::visit::{self, Visit};
use syn::{Attribute, ItemFn, ItemMod};

use crate::scan::syntax_fingerprint;

use super::{FocusedTest, REQUIRED_ROOTS, UnsafeContract, validate_relative_rust_path};

pub(super) fn validate(workspace: &Path, contract_id: &str, test: &FocusedTest) -> Result<()> {
    let reference = &test.reference;
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
    let actual_fingerprint = collector
        .tests
        .get(test_name)
        .with_context(|| format!("unsafe contract {contract_id:?} focused test {reference:?} does not name an unconditional, non-ignored explicit test function"))?;
    if actual_fingerprint != &test.fingerprint {
        bail!(
            "unsafe contract {contract_id:?} focused test {reference:?} syntax changed: expected fingerprint {}, found {actual_fingerprint}",
            test.fingerprint
        );
    }
    Ok(())
}

pub(super) fn validate_cargo_metadata(workspace: &Path, cargo: &toml::Value, metadata: &[u8], contracts: &[UnsafeContract]) -> Result<()> {
    let workspace = fs::canonicalize(workspace).with_context(|| format!("resolve focused-test workspace {}", workspace.display()))?;
    let root_manifest = fs::canonicalize(workspace.join("Cargo.toml")).context("resolve root Cargo.toml for focused-test targets")?;
    let metadata: CargoMetadata = serde_json::from_slice(metadata).context("parse Cargo metadata for focused-test targets")?;
    let mut root_packages = Vec::new();
    for package in &metadata.packages {
        let manifest = fs::canonicalize(&package.manifest_path).with_context(|| format!("resolve Cargo metadata manifest {}", package.manifest_path.display()))?;
        if manifest == root_manifest {
            root_packages.push(package);
        }
    }
    let [root_package] = root_packages.as_slice() else {
        bail!(
            "Cargo metadata must contain exactly one root package for focused-test targets, found {}",
            root_packages.len()
        );
    };

    for contract in contracts {
        for test in &contract.focused_tests {
            validate_scheduled_reference(&workspace, cargo, root_package, &contract.id, &test.reference)?;
        }
    }
    Ok(())
}

fn validate_scheduled_reference(workspace: &Path, cargo: &toml::Value, root_package: &CargoPackage, contract_id: &str, reference: &str) -> Result<()> {
    let (source_path, _) = reference.split_once("::").context("validated focused-test reference lost its source separator")?;
    let source = fs::canonicalize(workspace.join(source_path)).with_context(|| format!("resolve unsafe contract {contract_id:?} focused test target source {source_path:?}"))?;
    let mut scheduled = Vec::new();
    for target in &root_package.targets {
        if !target.test || !target.kind.iter().any(|kind| kind == "test") {
            continue;
        }
        let target_source = fs::canonicalize(&target.src_path).with_context(|| format!("resolve Cargo metadata target source {}", target.src_path.display()))?;
        if target_source == source {
            scheduled.push(target);
        }
    }
    let [target] = scheduled.as_slice() else {
        bail!(
            "unsafe contract {contract_id:?} focused test {reference:?} must be the source of exactly one Cargo-scheduled integration-test target, found {}",
            scheduled.len()
        );
    };
    if !target.required_features.is_empty() {
        bail!("unsafe contract {contract_id:?} focused test {reference:?} cannot require opt-in Cargo features");
    }
    reject_custom_harness(cargo, contract_id, reference, &target.name)
}

fn reject_custom_harness(cargo: &toml::Value, contract_id: &str, reference: &str, target_name: &str) -> Result<()> {
    let Some(declarations) = cargo.get("test") else {
        return Ok(());
    };
    for declaration in declarations.as_array().context("Cargo.toml [[test]] declarations must be an array")? {
        let declaration = declaration.as_table().context("Cargo.toml [[test]] declaration must be a table")?;
        if declaration.get("name").and_then(toml::Value::as_str) == Some(target_name) && declaration.get("harness").and_then(toml::Value::as_bool) == Some(false) {
            bail!("unsafe contract {contract_id:?} focused test {reference:?} must use Cargo's standard test harness");
        }
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

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    manifest_path: PathBuf,
    targets: Vec<CargoTarget>,
}

#[derive(Clone, Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
    test: bool,
    #[serde(default, rename = "required-features")]
    required_features: Vec<String>,
}

#[derive(Default)]
struct TestCollector {
    modules: Vec<String>,
    excluded_context_depth: usize,
    tests: BTreeMap<String, String>,
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
            self.tests.insert(path.join("::"), syntax_fingerprint(function));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{CargoPackage, CargoTarget, FocusedTest, syntax_fingerprint, validate, validate_scheduled_reference};

    fn focused_test(reference: &str, source: &str) -> FocusedTest {
        let function: syn::ItemFn = syn::parse_str(source).expect("focused test function");
        FocusedTest {
            reference: reference.to_owned(),
            fingerprint: syntax_fingerprint(&function),
        }
    }

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

        let existing = focused_test("tests/focused.rs::nested::contract_case", "#[test] fn contract_case() {}");
        validate(workspace.path(), "contract.one", &existing).expect("existing test");
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
            let missing = FocusedTest {
                reference: reference.to_owned(),
                fingerprint: "a".repeat(64),
            };
            assert!(validate(workspace.path(), "contract.one", &missing).is_err(), "reference must fail closed: {reference}");
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
        let excluded = focused_test("tests/excluded.rs::excluded_file_case", "#[test] fn excluded_file_case() {}");
        assert!(validate(workspace.path(), "contract.one", &excluded).is_err());
    }

    #[test]
    fn focused_test_reference_ratchets_the_complete_test_syntax() {
        let workspace = tempdir().expect("temporary workspace");
        fs::create_dir(workspace.path().join("tests")).expect("tests root");
        let source = workspace.path().join("tests/focused.rs");
        let implementation = "#[test] fn contract_case() { assert_eq!(2 + 2, 4); }";
        fs::write(&source, implementation).expect("focused test source");
        let test = focused_test("tests/focused.rs::contract_case", implementation);

        validate(workspace.path(), "contract.one", &test).expect("reviewed test syntax");
        fs::write(&source, "#[test]\nfn contract_case(){assert_eq!(2+2,4);}\n").expect("reformatted focused test");
        validate(workspace.path(), "contract.one", &test).expect("equivalent token syntax");

        fs::write(&source, "#[test]\nfn contract_case() {}\n").expect("weakened focused test");
        let error = validate(workspace.path(), "contract.one", &test).expect_err("changed focused test syntax must fail closed");
        assert!(error.to_string().contains("syntax changed"), "unexpected error: {error:#}");
    }

    #[test]
    fn focused_test_reference_requires_a_standard_scheduled_cargo_target() {
        let workspace = tempdir().expect("temporary workspace");
        fs::create_dir(workspace.path().join("tests")).expect("tests root");
        let source = workspace.path().join("tests/focused.rs");
        fs::write(&source, "#[test]\nfn contract_case() {}\n").expect("focused test source");
        let cargo: toml::Value = toml::from_str("").expect("empty Cargo document");
        let target = CargoTarget {
            name: "focused".to_owned(),
            kind: vec!["test".to_owned()],
            src_path: source,
            test: true,
            required_features: Vec::new(),
        };
        let package = CargoPackage {
            manifest_path: workspace.path().join("Cargo.toml"),
            targets: vec![target],
        };
        let reference = "tests/focused.rs::contract_case";

        validate_scheduled_reference(workspace.path(), &cargo, &package, "contract.one", reference).expect("scheduled standard test target");

        let missing = CargoPackage {
            manifest_path: package.manifest_path.clone(),
            targets: Vec::new(),
        };
        assert!(validate_scheduled_reference(workspace.path(), &cargo, &missing, "contract.one", reference).is_err());

        let mut disabled = package.targets[0].clone();
        disabled.test = false;
        let disabled = CargoPackage {
            manifest_path: package.manifest_path.clone(),
            targets: vec![disabled],
        };
        assert!(validate_scheduled_reference(workspace.path(), &cargo, &disabled, "contract.one", reference).is_err());

        let mut feature_gated = package.targets[0].clone();
        feature_gated.required_features.push("opt-in".to_owned());
        let feature_gated = CargoPackage {
            manifest_path: package.manifest_path.clone(),
            targets: vec![feature_gated],
        };
        assert!(validate_scheduled_reference(workspace.path(), &cargo, &feature_gated, "contract.one", reference).is_err());

        let custom_harness: toml::Value = toml::from_str(
            "
            [[test]]
            name = 'focused'
            harness = false
            ",
        )
        .expect("custom test target");
        assert!(validate_scheduled_reference(workspace.path(), &custom_harness, &package, "contract.one", reference).is_err());
    }
}
