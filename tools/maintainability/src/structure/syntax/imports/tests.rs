use std::fmt::Write as _;

use super::*;

fn imports(path: &str, source: &str) -> Result<Vec<String>> {
    let syntax = syn::parse_file(source)?;
    production_imports(&syntax, path, Some("src/lib.rs"), false)
}

fn production_imports(file: &syn::File, path: &str, crate_root: Option<&str>, rust_2015_absolute_paths: bool) -> Result<Vec<String>> {
    Ok(production_syntax_facts(
        file,
        path,
        crate_root,
        ProductionSyntaxOptions {
            collect_internal_imports: true,
            rust_2015_absolute_paths,
            require_reviewed_expansions: true,
        },
    )?
    .internal_imports)
}

fn concrete_facts(source: &str) -> Result<ProductionSyntaxFacts> {
    concrete_facts_with_expansion_policy(source, true)
}

fn historical_concrete_facts(source: &str) -> Result<ProductionSyntaxFacts> {
    concrete_facts_with_expansion_policy(source, false)
}

fn concrete_facts_with_expansion_policy(source: &str, require_reviewed_expansions: bool) -> Result<ProductionSyntaxFacts> {
    production_syntax_facts(
        &syn::parse_file(source)?,
        "src/store_fixture.rs",
        Some("src/lib.rs"),
        ProductionSyntaxOptions {
            collect_internal_imports: false,
            rust_2015_absolute_paths: false,
            require_reviewed_expansions,
        },
    )
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
                  #[cfg(doctest)]\nuse crate::server::DocTestOnly;\n\
                  #[cfg(feature = \"testing\")]\nmod support { use crate::ui; }\n\
                  #[cfg(test)]\nfn test_path() -> crate::server::TestOnly { unreachable!() }\n\
                  #[test]\nfn direct_test_path() -> crate::server::DirectTestOnly { unreachable!() }\n\
                  #[cfg_attr(all(), test)]\nfn attributed_test_path() -> crate::server::AttributedTestOnly { unreachable!() }\n\
                  #[cfg_attr(all(), cfg_attr(all(), cfg(test)))]\nuse crate::server::NestedTestOnly;\n\
                  use crate::server::LocalHoldServer;\n";
    assert_eq!(imports("src/http_transport.rs", source).expect("imports"), ["crate::server::LocalHoldServer"]);
}

#[test]
fn concrete_method_signatures_track_their_complete_impl_header() -> Result<()> {
    let inherent = concrete_facts(
        "trait Reader { fn open() -> SqliteStore; }\n\
         struct Adapter;\n\
         impl Adapter { pub fn open() -> SqliteStore { loop {} } }\n",
    )?;
    let trait_implementation = concrete_facts(
        "trait Reader { fn open() -> SqliteStore; }\n\
         struct Adapter;\n\
         impl Reader for Adapter { fn open() -> SqliteStore { loop {} } }\n",
    )?;
    assert_ne!(inherent.signature_concrete_store_sites, trait_implementation.signature_concrete_store_sites);
    Ok(())
}

#[test]
fn impl_signature_self_types_resolve_use_aliases_after_collection() -> Result<()> {
    let facts = concrete_facts(
        "mod hidden { pub(crate) struct Adapter; }\n\
         impl InternalAdapter { pub(crate) fn open() -> SqliteStore { loop {} } }\n\
         pub(crate) use hidden::Adapter;\n\
         use hidden::Adapter as InternalAdapter;\n",
    )?;
    let signature = facts.signature_concrete_store_sites.sqlite_store.first().expect("concrete impl signature");
    assert_eq!(signature.item_path, ["store_fixture", "hidden", "Adapter"]);
    assert!(signature.impl_self_type);
    assert_eq!(facts.type_declarations[0].item_path, ["store_fixture", "hidden", "Adapter"]);
    Ok(())
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
fn bare_paths_resolve_relative_to_their_rust_2018_module() -> Result<()> {
    let source = "mod nested { mod server {} use server::Type; fn call() { server::call(); } }\n\
                  use crate::server::Actual;\n";
    assert_eq!(imports("src/adapter.rs", source).expect("lexical bare paths"), ["crate::server::Actual"]);

    let syntax = syn::parse_file("use server::AtRoot;\n")?;
    assert_eq!(production_imports(&syntax, "src/core.rs", Some("src/core.rs"), false)?, ["crate::server::AtRoot"]);

    let reexport = concrete_facts("mod ui { pub(crate) use helper::open; mod helper { pub fn open() {} } }\n")?;
    assert_eq!(reexport.public_reexports[0].target_path, ["store_fixture", "ui", "helper", "open"]);
    Ok(())
}

#[test]
fn public_reexport_aliases_use_only_cfg_compatible_bindings() -> Result<()> {
    let facts = concrete_facts(
        "#[cfg(feature = \"legacy\")]\n\
         use crate::store_helper as facade;\n\
         #[cfg(not(feature = \"legacy\"))]\n\
         use crate::unrelated as facade;\n\
         #[cfg(not(feature = \"legacy\"))]\n\
         pub use facade::open;\n",
    )?;
    assert_eq!(facts.public_reexports.len(), 1);
    assert_eq!(facts.public_reexports[0].target_path, ["unrelated", "open"]);
    Ok(())
}

#[test]
fn self_restricted_uses_are_not_public_reexport_evidence() -> Result<()> {
    for visibility in ["", "pub(self)", "pub(in self)"] {
        let facts = concrete_facts(&format!("{visibility} use crate::stores::open;\n"))?;
        assert!(facts.public_reexports.is_empty(), "{visibility}");
    }
    Ok(())
}

#[test]
fn explicit_imports_take_precedence_over_compatible_globs() -> Result<()> {
    let facts = concrete_facts(
        "use crate::safe::facade;\n\
         use crate::stores::*;\n\
         pub(crate) use facade::open;\n",
    )?;
    assert_eq!(facts.public_reexports.len(), 1);
    assert_eq!(facts.public_reexports[0].target_path, ["safe", "facade", "open"]);
    Ok(())
}

#[test]
fn explicit_imports_take_precedence_over_globs_only_in_their_cfg_region() -> Result<()> {
    let facts = concrete_facts(
        "#[cfg(feature = \"legacy\")]\n\
         use crate::safe::facade;\n\
         use crate::stores::*;\n\
         pub(crate) use facade::open;\n",
    )?;
    assert_eq!(facts.public_reexports.len(), 2);
    assert_eq!(
        facts.public_reexports.iter().map(|evidence| evidence.target_path.clone()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            vec!["safe".to_owned(), "facade".to_owned(), "open".to_owned()],
            vec!["stores".to_owned(), "facade".to_owned(), "open".to_owned()],
        ])
    );
    let legacy = production_cfg_context(&[syn::parse_quote!(#[cfg(feature = "legacy")])], &ProductionCfgContext::default())?.expect("legacy cfg");
    let current = production_cfg_context(&[syn::parse_quote!(#[cfg(not(feature = "legacy"))])], &ProductionCfgContext::default())?.expect("current cfg");
    let safe = facts
        .public_reexports
        .iter()
        .find(|evidence| evidence.target_path.first().is_some_and(|segment| segment == "safe"))
        .expect("explicit target");
    let stores = facts
        .public_reexports
        .iter()
        .find(|evidence| evidence.target_path.first().is_some_and(|segment| segment == "stores"))
        .expect("glob target");
    assert!(safe.cfg.conjoin(&legacy).is_some());
    assert!(safe.cfg.conjoin(&current).is_none());
    assert!(stores.cfg.conjoin(&legacy).is_none());
    assert!(stores.cfg.conjoin(&current).is_some());
    Ok(())
}

#[test]
fn builtin_stringify_aliases_use_only_cfg_compatible_bindings() {
    for source in [
        "#[cfg(feature = \"legacy\")]\n\
         use ::core::stringify as text;\n\
         #[cfg(not(feature = \"legacy\"))]\n\
         const STORE: &str = text!(SqliteStore);\n",
        "fn open() {\n\
             #[cfg(feature = \"legacy\")]\n\
             use ::core::stringify as text;\n\
             #[cfg(not(feature = \"legacy\"))]\n\
             let _ = text!(PostgresStore);\n\
         }\n",
    ] {
        let Err(error) = concrete_facts(source) else {
            panic!("a mutually exclusive stringify import cannot classify the invocation");
        };
        assert!(error.to_string().contains("unreviewed macro expansion path text"), "{error:#}");
    }
}

#[test]
fn module_stringify_aliases_require_builtin_coverage_without_cfg_shadows() -> Result<()> {
    let shadowed = "#[cfg(feature = \"legacy\")]\n\
                    use ::core::stringify as text;\n\
                    #[cfg(not(feature = \"legacy\"))]\n\
                    use crate::other as text;\n\
                    const STORE: &str = text!(SqliteStore);\n";
    let Err(error) = concrete_facts(shadowed) else {
        panic!("a non-builtin binding in one cfg region must keep the invocation opaque");
    };
    assert!(error.to_string().contains("unreviewed macro expansion path text"), "{error:#}");

    let fully_builtin = "#[cfg(feature = \"legacy\")]\n\
                         use ::core::stringify as text;\n\
                         #[cfg(not(feature = \"legacy\"))]\n\
                         use ::std::stringify as text;\n\
                         const STORE: &str = text!(PostgresStore);\n";
    let facts = concrete_facts(fully_builtin)?;
    assert_eq!(facts.concrete_stores, ConcreteStoreCounts::default());
    Ok(())
}

#[test]
fn nested_custom_library_roots_define_relative_module_paths() -> Result<()> {
    let syntax = syn::parse_file("use super::server::Service;\n")?;
    assert_eq!(
        production_imports(&syntax, "src/core/worker.rs", Some("src/core/lib.rs"), false)?,
        ["crate::server::Service"]
    );
    Ok(())
}

#[test]
fn restricted_names_in_production_macro_tokens_fail_closed() {
    let source = "macro_rules! dependency { () => { use crate::server::Service; } }\n";
    assert!(imports("src/adapter.rs", source).unwrap_err().to_string().contains("production macro token stream"));

    let argument = "tracing::info!(\"{}\", crate::ui::label());\n";
    assert!(imports("src/adapter.rs", argument).unwrap_err().to_string().contains("production macro token stream"));

    let encoded = "numbered_placeholders!(\"crate::server::serialize\");\n";
    assert!(imports("src/adapter.rs", encoded).unwrap_err().to_string().contains("production macro token stream"));

    let test_only = "#[cfg(test)]\nmacro_rules! dependency { () => { use crate::ui::View; } }\n";
    assert!(imports("src/adapter.rs", test_only).expect("test-only macro").is_empty());
}

#[test]
fn ordinary_macro_identifiers_named_like_restricted_modules_are_allowed() {
    let source = "fn log(server: &str, ui: &str) {\n\
                  tracing::info!(server);\n\
                  let _ = format!(\"{}\", ui);\n\
                  }\n";
    assert!(imports("src/adapter.rs", source).expect("local macro identifiers").is_empty());
}

#[test]
fn restricted_names_in_production_attribute_tokens_fail_closed() {
    let source = "#[adapter(crate::server::Service)]\nfn build() {}\n";
    assert!(imports("src/adapter.rs", source).unwrap_err().to_string().contains("production attribute token stream"));

    let test_only = "#[cfg(test)]\n#[adapter(crate::ui::View)]\nfn build() {}\n";
    assert!(imports("src/adapter.rs", test_only).expect("test-only attribute").is_empty());
}

#[test]
fn string_encoded_attribute_paths_fail_closed() {
    let source = "#[serde(serialize_with = \"crate::server::serialize\")]\nstruct Record;\n";
    assert!(imports("src/adapter.rs", source).unwrap_err().to_string().contains("production attribute token stream"));

    let bound = "#[serde(bound(serialize = \"T: crate::server::Marker\"))]\nstruct Generic<T>(T);\n";
    assert!(imports("src/adapter.rs", bound).unwrap_err().to_string().contains("production attribute token stream"));

    let external_bound = "#[serde(bound(serialize = \"T: external::Marker\"))]\nstruct Generic<T>(T);\n";
    assert!(imports("src/adapter.rs", external_bound).expect("external bound").is_empty());

    let unclassifiable = "#[serde(bound(serialize = \"T: external::Marker /*\"))]\nstruct Generic<T>(T);\n";
    assert!(imports("src/adapter.rs", unclassifiable).unwrap_err().to_string().contains("not classifiable Rust syntax"));

    let external = "#[serde(serialize_with = \"::server::serialize\")]\nstruct Record;\n";
    assert!(imports("src/adapter.rs", external).expect("absolute external path").is_empty());

    let plain_text = "#[serde(rename = \"server::label\")]\nstruct Record;\n";
    assert!(imports("src/nested/adapter.rs", plain_text).expect("non-root relative label").is_empty());
}

#[test]
fn cfg_attr_scans_only_nested_attributes_that_can_apply_in_production() {
    let test_only = "#[cfg_attr(test, serde(serialize_with = \"crate::server::serialize\"))]\n\
                     #[cfg_attr(feature = \"testing\", serde(serialize_with = \"crate::ui::serialize\"))]\n\
                     struct Record;\n";
    assert!(imports("src/adapter.rs", test_only).expect("test-only nested attributes").is_empty());

    let production = "#[cfg_attr(feature = \"other\", serde(serialize_with = \"crate::server::serialize\"))]\nstruct Record;\n";
    assert!(imports("src/adapter.rs", production).unwrap_err().to_string().contains("production attribute token stream"));
}

#[test]
fn test_only_and_production_items_on_one_line_are_distinguished() {
    let source = "#[cfg(test)] fn helper() { crate::ui::test_only(); } use crate::server::Service;\n";
    assert_eq!(imports("src/adapter.rs", source).expect("same-line items"), ["crate::server::Service"]);
}

#[test]
fn absolute_external_paths_are_not_classified_as_crate_relative() -> Result<()> {
    let source = "use ::server::External;\n\
                  use ::ui::External as ExternalUi;\n\
                  fn build() -> ::server::Qualified { ::ui::qualified() }\n";
    assert!(imports("src/adapter.rs", source).expect("absolute external paths").is_empty());
    let reexport = concrete_facts(
        "mod url { pub struct Url; }\n\
         pub use ::url::Url;\n",
    )?;
    assert!(reexport.public_reexports.is_empty());
    Ok(())
}

#[test]
fn rust_2015_absolute_paths_are_classified_as_crate_relative() -> Result<()> {
    let syntax = syn::parse_file(
        "use ::server::External;\n\
         fn build() -> ::server::Qualified { ::ui::qualified() }\n",
    )?;
    assert_eq!(
        production_imports(&syntax, "src/adapter.rs", Some("src/lib.rs"), true)?,
        ["crate::server::External", "crate::server::Qualified", "crate::ui::qualified"]
    );
    Ok(())
}

#[test]
fn rust_2015_nested_bare_use_paths_are_classified_from_the_crate_root() -> Result<()> {
    let syntax = syn::parse_file("mod nested { use server::Service; }\n")?;
    assert_eq!(production_imports(&syntax, "src/adapter.rs", Some("src/lib.rs"), true)?, ["crate::server::Service"]);
    Ok(())
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

#[test]
fn restricted_imports_cannot_be_reexported() {
    assert!(
        imports("src/http_transport.rs", "pub use crate::server::LocalHoldServer;\n")
            .unwrap_err()
            .to_string()
            .contains("cannot be re-exported")
    );
    assert!(
        imports("src/http_transport.rs", "pub(crate) use crate::server::LocalHoldServer;\n")
            .unwrap_err()
            .to_string()
            .contains("cannot be re-exported")
    );
}

#[test]
fn restricted_imports_cannot_be_exposed_through_public_type_aliases() {
    for visibility in ["pub ", "pub(crate) "] {
        let source = format!("{visibility}type Server = crate::server::LocalHoldServer;\n");
        assert!(imports("src/http_transport.rs", &source).unwrap_err().to_string().contains("public type aliases"));
    }
    assert_eq!(
        imports("src/http_transport.rs", "type Server = crate::server::LocalHoldServer;\n").expect("private alias"),
        ["crate::server::LocalHoldServer"]
    );
}

#[test]
fn cfg_gated_parameters_are_excluded_by_node() {
    let source = "fn call(#[cfg(test)] _: crate::server::TestOnly) {}\n\
                  fn generic<#[cfg(feature = \"testing\")] T: crate::ui::TestOnly>() {}\n";
    assert!(imports("src/adapter.rs", source).expect("test-only parameters").is_empty());
}

#[test]
fn unreviewed_production_expansions_fail_closed() {
    assert!(
        imports("src/adapter.rs", "fn call() { inject!(); }\n")
            .unwrap_err()
            .to_string()
            .contains("unreviewed macro expansion")
    );
    assert!(
        imports("src/adapter.rs", "#[inject]\nfn call() {}\n")
            .unwrap_err()
            .to_string()
            .contains("unreviewed attribute expansion")
    );
    let test_only = "#[cfg(test)]\n#[inject]\nfn call() { inject!(); }\n";
    assert!(imports("src/adapter.rs", test_only).expect("test-only opaque expansions").is_empty());
}

#[test]
fn concrete_store_names_are_counted_only_in_production_syntax() -> Result<()> {
    let source = "use crate::store::{r#SqliteStore, PostgresStore};\n\
                  fn open(store: SqliteStore) -> PostgresStore { numbered_placeholders!(SqliteStore); store }\n\
                  #[cfg(test)] fn test_only(_: SqliteStore) { numbered_placeholders!(PostgresStore); }\n\
                  #[cfg(feature = \"testing\")] const TESTING: PostgresStore = unreachable!();\n\
                  const TEXT: &str = \"SqliteStore PostgresStore\"; // SqliteStore\n";
    let syntax = syn::parse_file(source)?;
    let facts = production_syntax_facts(
        &syntax,
        "src/store_fixture.rs",
        Some("src/lib.rs"),
        ProductionSyntaxOptions {
            collect_internal_imports: false,
            rust_2015_absolute_paths: false,
            require_reviewed_expansions: true,
        },
    )?;
    assert_eq!(
        facts.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 3,
            postgres_store: 2,
        }
    );
    Ok(())
}

#[test]
fn explicit_builtin_stringify_arguments_are_not_treated_as_resolved_syntax() -> Result<()> {
    let source = "const SQLITE: &str = ::core::stringify!(SqliteStore);\n\
                  const POSTGRES: &str = concat!(::core::stringify!(PostgresStore));\n\
                  const CODE: usize = SqliteStore::FORMAT_VERSION;\n";
    assert_eq!(
        concrete_facts(source)?.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 1,
            postgres_store: 0,
        }
    );

    let restricted = "const ROUTE: &str = concat!(::core::stringify!(crate::server::Service));\n";
    assert!(imports("src/adapter.rs", restricted)?.is_empty());

    let definition = "macro_rules! numbered_placeholders { () => { ::core::stringify!(SqliteStore) } }\nnumbered_placeholders!();\n";
    assert_eq!(concrete_facts(definition)?.concrete_stores, ConcreteStoreCounts::default());

    for imported in [
        "use ::core::stringify as text;\nconst STORE: &str = text!(SqliteStore);\n",
        "const STORE: &str = text!(PostgresStore);\nuse ::std::stringify as text;\n",
        "mod nested { use ::core::stringify; const STORE: &str = stringify!(SqliteStore); }\n",
        "fn open() { let _ = text!(SqliteStore); use ::core::stringify as text; }\n",
    ] {
        assert_eq!(concrete_facts(imported)?.concrete_stores, ConcreteStoreCounts::default());
    }
    Ok(())
}

#[test]
fn shadowable_stringify_names_do_not_hide_store_tokens() -> Result<()> {
    let ambiguous_builtin = concrete_facts("const STORE: &str = stringify!(PostgresStore);\n")?;
    assert_eq!(
        ambiguous_builtin.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 0,
            postgres_store: 1,
        }
    );

    let locally_shadowed = concrete_facts(
        "macro_rules! stringify { ($store:ty) => { <$store>::open() } }\n\
         stringify!(SqliteStore);\n",
    )?;
    assert_eq!(
        locally_shadowed.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 1,
            postgres_store: 0,
        }
    );

    let Err(imported_then_lexically_shadowed) = concrete_facts(
        "use ::core::stringify as text;\n\
         fn open() {\n\
             macro_rules! text { ($store:ty) => { <$store>::open() } }\n\
             text!(SqliteStore);\n\
         }\n",
    ) else {
        panic!("a lexically shadowed unreviewed alias must fail closed");
    };
    assert!(imported_then_lexically_shadowed.to_string().contains("unreviewed macro expansion path text"));

    let Err(imported_then_shadowed_by_nested_import) = concrete_facts(
        "use ::core::stringify as text;\n\
         fn open() {\n\
             use crate::other as text;\n\
             text!(SqliteStore);\n\
         }\n",
    ) else {
        panic!("a nested macro import must shadow an outer stringify alias");
    };
    assert!(imported_then_shadowed_by_nested_import.to_string().contains("unreviewed macro expansion path text"));

    let shadowed_reviewed_name = concrete_facts(
        "use ::core::stringify;\n\
         fn open() {\n\
             macro_rules! stringify { ($store:ty) => { <$store>::open() } }\n\
             stringify!(SqliteStore);\n\
         }\n",
    )?;
    assert_eq!(
        shadowed_reviewed_name.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 1,
            postgres_store: 0,
        }
    );

    for cfg_exclusive_shadow in [
        "use ::core::stringify as text;\n\
         #[cfg(feature = \"legacy\")]\n\
         macro_rules! text { ($store:ty) => { <$store>::open() } }\n\
         #[cfg(not(feature = \"legacy\"))]\n\
         const STORE: &str = text!(SqliteStore);\n",
        "fn open() {\n\
             use ::core::stringify as text;\n\
             #[cfg(feature = \"legacy\")]\n\
             macro_rules! text { ($store:ty) => { <$store>::open() } }\n\
             #[cfg(not(feature = \"legacy\"))]\n\
             let _ = text!(PostgresStore);\n\
         }\n",
    ] {
        assert_eq!(concrete_facts(cfg_exclusive_shadow)?.concrete_stores, ConcreteStoreCounts::default());
    }
    Ok(())
}

#[test]
fn canonical_concrete_store_declarations_require_public_production_structs() -> Result<()> {
    let facts = concrete_facts(
        "pub struct SqliteStore;\n\
         pub(crate) struct PostgresStore;\n\
         #[cfg(test)] pub struct PostgresStore;\n\
         pub enum PostgresBackend {}\n",
    )?;
    assert_eq!(facts.public_concrete_store_structs.sqlite_store.len(), 1);
    assert!(facts.public_concrete_store_structs.postgres_store.is_empty());
    assert!(
        facts
            .signature_concrete_store_sites
            .sqlite_store
            .iter()
            .any(|site| site.item_path == ["store_fixture", "SqliteStore"])
    );
    Ok(())
}

#[test]
fn canonical_declaration_fingerprints_ignore_outer_attributes_but_pin_shape() -> Result<()> {
    let plain = concrete_facts("pub struct SqliteStore { inner: Arc<SqliteInner> }")?;
    let documented = concrete_facts("/// SQLite backend.\npub struct SqliteStore { inner: Arc<SqliteInner> }")?;
    let documented_field = concrete_facts(
        "pub struct SqliteStore {\n\
             /// Shared backend state.\n\
             inner: Arc<SqliteInner>\n\
         }",
    )?;
    let conditionally_documented = historical_concrete_facts(
        "#[cfg_attr(docsrs, doc = \"SQLite backend.\")]\n\
         pub struct SqliteStore {\n\
             #[cfg_attr(docsrs, cfg_attr(all(), doc = \"Shared backend state.\"))]\n\
             inner: Arc<SqliteInner>\n\
         }",
    )?;
    let test_instrumented = concrete_facts(
        "pub struct SqliteStore<#[cfg(test)] TestProbe = usize, #[cfg(feature = \"testing\")] HarnessProbe = usize> {\n\
             inner: Arc<SqliteInner>,\n\
             #[cfg(test)] test_probe: usize,\n\
             #[cfg(feature = \"testing\")] testing_probe: usize,\n\
         }",
    )?;
    let production_instrumented = concrete_facts(
        "pub struct SqliteStore {\n\
             inner: Arc<SqliteInner>,\n\
             #[cfg(feature = \"legacy\")] legacy_probe: usize,\n\
         }",
    )?;
    let derived = concrete_facts("#[derive(Clone, Debug)]\npub struct SqliteStore { inner: Arc<SqliteInner> }")?;
    let replaced = concrete_facts("pub struct SqliteStore;")?;
    let feature_gated = concrete_facts(
        "#[cfg(feature = \"legacy\")]\n\
         pub struct SqliteStore { inner: Arc<SqliteInner> }",
    )?;
    let public_nested = concrete_facts(
        "pub mod backend {\n\
             pub struct SqliteStore { inner: Arc<SqliteInner> }\n\
         }",
    )?;
    let private_nested = concrete_facts(
        "mod backend {\n\
             pub struct SqliteStore { inner: Arc<SqliteInner> }\n\
         }",
    )?;
    let ancestor_gated = concrete_facts(
        "#[cfg(feature = \"legacy\")]\n\
         pub mod backend {\n\
             pub struct SqliteStore { inner: Arc<SqliteInner> }\n\
         }",
    )?;

    assert_eq!(plain.public_concrete_store_structs, documented.public_concrete_store_structs);
    assert_eq!(plain.public_concrete_store_structs, documented_field.public_concrete_store_structs);
    assert_eq!(plain.public_concrete_store_structs, conditionally_documented.public_concrete_store_structs);
    assert_eq!(plain.public_concrete_store_structs, test_instrumented.public_concrete_store_structs);
    assert_ne!(plain.public_concrete_store_structs, production_instrumented.public_concrete_store_structs);
    assert_ne!(plain.public_concrete_store_structs, derived.public_concrete_store_structs);
    assert_ne!(plain.public_concrete_store_structs, replaced.public_concrete_store_structs);
    assert_ne!(plain.public_concrete_store_structs, feature_gated.public_concrete_store_structs);
    assert_ne!(public_nested.public_concrete_store_structs, private_nested.public_concrete_store_structs);
    assert_ne!(public_nested.public_concrete_store_structs, ancestor_gated.public_concrete_store_structs);
    Ok(())
}

#[test]
fn path_valued_attribute_literals_count_concrete_stores() -> Result<()> {
    let source = "#[serde(serialize_with = \"crate::store::SqliteStore\")]\nstruct Record;\n\
                  #[serde(rename = \"PostgresStore\")]\nstruct Renamed;\n\
                  #[schemars(bound = \"SqliteStore: MemoryReader\")]\nstruct Bound;\n\
                  #[serde(bound(deserialize = \"SqliteStore: MemoryReader\"))]\nstruct NestedBound;\n\
                  #[serde(deserialize_with = \"PostgresStore\")]\nstruct BarePath;\n\
                  #[schemars(example = \"SqliteStore::example\")]\nstruct Example;\n\
                  #[cfg_attr(test, serde(serialize_with = \"crate::store::SqliteStore\"))]\nstruct TestOnly;\n\
                  #[cfg_attr(feature = \"other\", serde(serialize_with = \"crate::store::PostgresStore\"))]\nstruct Production;\n";
    let facts = concrete_facts(source)?;
    assert_eq!(
        facts.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 4,
            postgres_store: 2,
        }
    );
    Ok(())
}

#[test]
fn prose_and_descriptive_attribute_strings_do_not_count_concrete_stores() -> Result<()> {
    let source = "#[deprecated(note = \"use crate::store::SqliteStore instead\")]\n\
                  /// The old crate::store::PostgresStore implementation.
                  #[serde(rename = \"crate::store::SqliteStore\")]\n\
                  #[schemars(description = \"crate::store::PostgresStore\")]\n\
                  struct Described;\n";
    let facts = concrete_facts(source)?;
    assert_eq!(facts.concrete_stores, ConcreteStoreCounts::default());
    Ok(())
}

#[test]
fn schemars_extension_strings_remain_schema_data() -> Result<()> {
    let facts = concrete_facts(
        "#[schemars(extend(\"x-backend\" = \"SqliteStore\"))]\n\
         struct Labeled;\n\
         #[schemars(extend(\"x-backend\" = PostgresStore))]\n\
         struct TokenBearing;\n",
    )?;
    assert_eq!(
        facts.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 0,
            postgres_store: 1,
        }
    );
    Ok(())
}

#[test]
fn cfg_attr_store_tokens_are_counted_only_when_the_branch_can_apply_in_production() -> Result<()> {
    let facts = concrete_facts(
        "#[cfg_attr(test, serde(default = SqliteStore))]\n\
         #[cfg_attr(feature = \"testing\", serde(default = PostgresStore))]\n\
         struct TestInstrumentation;\n\
         #[cfg_attr(feature = \"other\", serde(default = SqliteStore))]\n\
         struct ProductionInstrumentation;\n",
    )?;
    assert_eq!(
        facts.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 1,
            postgres_store: 0,
        }
    );
    Ok(())
}

#[test]
fn cfg_attr_disabling_siblings_remove_store_tokens_from_production() -> Result<()> {
    let source = "#[cfg_attr(feature = \"other\", cfg(test), serde(serialize_with = \"SqliteStore::serialize\"))]\n\
                  struct Direct;\n\
                  #[cfg_attr(feature = \"other\", cfg_attr(all(), cfg(feature = \"testing\")), serde(default = \"PostgresStore::default\"))]\n\
                  struct Nested;\n";
    assert_eq!(concrete_facts(source)?.concrete_stores, ConcreteStoreCounts::default());
    Ok(())
}

#[test]
fn nested_cfg_attr_reuses_outer_condition_when_classifying_siblings() -> Result<()> {
    let source = "#[cfg_attr(feature = \"other\", cfg_attr(feature = \"other\", cfg(test)), serde(default = \"SqliteStore::default\"))]\n\
                  struct RepeatedCondition;\n";
    assert_eq!(concrete_facts(source)?.concrete_stores, ConcreteStoreCounts::default());

    let restricted = "#[cfg_attr(feature = \"other\", cfg_attr(feature = \"other\", cfg(test)), serde(default = \"crate::server::default\"))]\n\
                      struct RepeatedCondition;\n";
    assert!(imports("src/adapter.rs", restricted)?.is_empty());

    let independent = "#[cfg_attr(feature = \"other\", cfg_attr(feature = \"optional\", cfg(test)), serde(default = \"SqliteStore::default\"))]\n\
                       struct IndependentCondition;\n";
    assert_eq!(
        concrete_facts(independent)?.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 1,
            postgres_store: 0,
        }
    );
    Ok(())
}

#[test]
fn sibling_cfg_predicates_are_conjoined_for_production_attributes() -> Result<()> {
    let source = "#[cfg(feature = \"x\")]\n\
                  #[cfg(not(feature = \"x\"))]\n\
                  struct Contradictory(SqliteStore);\n\
                  #[cfg(feature = \"x\")]\n\
                  #[cfg_attr(feature = \"x\", cfg(feature = \"y\"))]\n\
                  #[cfg_attr(feature = \"x\", cfg(not(feature = \"y\")))]\n\
                  struct ContradictoryConditional(SqliteStore);\n\
                  #[cfg(feature = \"x\")]\n\
                  mod parent { #[cfg(not(feature = \"x\"))] struct ContradictoryChild(SqliteStore); }\n\
                  #[cfg(feature = \"x\")]\n\
                  #[cfg_attr(not(feature = \"x\"), serde(default = PostgresStore))]\n\
                  struct InactiveAttribute;\n\
                  #[cfg(feature = \"x\")]\n\
                  #[cfg_attr(feature = \"x\", serde(default = PostgresStore))]\n\
                  struct ActiveAttribute;\n";
    assert_eq!(
        concrete_facts(source)?.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 0,
            postgres_store: 1,
        }
    );

    let restricted = "#[cfg(feature = \"x\")]\n\
                      #[cfg_attr(not(feature = \"x\"), serde(default = \"crate::server::default\"))]\n\
                      struct InactiveAttribute;\n";
    assert!(imports("src/adapter.rs", restricted)?.is_empty());
    Ok(())
}

#[test]
fn cfg_contradictions_remain_detectable_with_many_unrelated_atoms() -> Result<()> {
    let mut source = String::new();
    for index in 0..24 {
        write!(source, "#[cfg(feature = \"unrelated-{index}\")]\nmod level_{index} {{\n").expect("write cfg fixture");
    }
    source.push_str(
        "#[cfg(feature = \"x\")]\n\
         #[cfg(not(feature = \"x\"))]\n\
         struct Dead(SqliteStore);\n",
    );
    for _ in 0..24 {
        source.push_str("}\n");
    }
    assert_eq!(concrete_facts(&source)?.concrete_stores, ConcreteStoreCounts::default());
    Ok(())
}

#[test]
fn raw_and_ordinary_cfg_identifiers_share_one_atom_identity() -> Result<()> {
    let facts = concrete_facts("#[cfg(unix)] #[cfg(not(r#unix))] fn disabled() { let _ = SqliteStore; }\n")?;
    assert_eq!(facts.concrete_stores, ConcreteStoreCounts::default());
    Ok(())
}

#[test]
fn target_family_shorthands_share_their_explicit_cfg_identity() -> Result<()> {
    let facts = concrete_facts(
        "#[cfg(unix)] #[cfg(not(target_family = \"unix\"))] struct DeadUnix(SqliteStore);\n\
         #[cfg(target_family = \"windows\")] #[cfg(not(windows))] struct DeadWindows(PostgresStore);\n\
         #[cfg(unix)] #[cfg(windows)] struct ImpossibleFamily(SqliteStore);\n",
    )?;
    assert_eq!(facts.concrete_stores, ConcreteStoreCounts::default());
    Ok(())
}

#[test]
fn raw_and_cooked_cfg_literals_share_one_atom_identity() -> Result<()> {
    let facts = concrete_facts("#[cfg(feature = \"x\")] #[cfg(not(feature = r\"x\"))] fn disabled() { let _ = SqliteStore; }\n")?;
    assert_eq!(facts.concrete_stores, ConcreteStoreCounts::default());
    Ok(())
}

#[test]
fn mutually_exclusive_target_values_disable_unreachable_store_syntax() -> Result<()> {
    let facts = concrete_facts(
        "#[cfg(target_os = \"linux\")]\n\
         mod linux {\n\
             #[cfg(target_os = \"windows\")]\n\
             struct Unreachable(SqliteStore);\n\
         }\n\
         #[cfg(any(target_os = \"linux\", target_os = \"windows\"))]\n\
         struct Reachable(PostgresStore);\n\
         #[cfg(all(target_feature = \"sse2\", target_feature = \"avx\"))]\n\
         struct MultipleTargetFeatures(PostgresStore);\n",
    )?;
    assert_eq!(
        facts.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 0,
            postgres_store: 2,
        }
    );
    Ok(())
}

#[test]
fn target_os_values_imply_their_target_family() -> Result<()> {
    let facts = concrete_facts(
        "#[cfg(target_os = \"linux\")] #[cfg(windows)] struct ImpossibleLinux(SqliteStore);\n\
         #[cfg(target_os = \"windows\")] #[cfg(unix)] struct ImpossibleWindows(PostgresStore);\n\
         #[cfg(target_os = \"emscripten\")] #[cfg(not(target_family = \"wasm\"))] struct ImpossibleEmscripten(SqliteStore);\n\
         #[cfg(target_os = \"wasi\")] #[cfg(not(target_family = \"wasm\"))] struct ImpossibleWasi(SqliteStore);\n\
         #[cfg(target_os = \"emscripten\")] #[cfg(all(unix, target_family = \"wasm\"))] struct EmscriptenCanHaveTwoFamilies(PostgresStore);\n",
    )?;
    assert_eq!(
        facts.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 0,
            postgres_store: 1,
        }
    );
    Ok(())
}

#[test]
fn concrete_store_bearing_signatures_are_inventoried_separately_from_bodies() -> Result<()> {
    let facts = concrete_facts("pub(crate) fn open() -> SqliteStore { SqliteStore::open() }\n")?;
    assert_eq!(facts.signature_concrete_store_sites.sqlite_store.len(), 1);
    assert_eq!(facts.concrete_stores.sqlite_store, 2);
    Ok(())
}

#[test]
fn store_specialized_impl_headers_are_exposure_signatures() -> Result<()> {
    let private = concrete_facts("impl Adapter<SqliteStore> { fn open() -> Self { loop {} } }\n")?;
    let exposed = concrete_facts("impl Adapter<SqliteStore> { pub(crate) fn open() -> Self { loop {} } }\n")?;
    let trait_impl = concrete_facts("impl Routed for Adapter<PostgresStore> { fn open() -> Self { loop {} } }\n")?;

    assert!(private.signature_concrete_store_sites.sqlite_store.is_empty());
    assert_eq!(exposed.signature_concrete_store_sites.sqlite_store.len(), 1);
    assert_eq!(trait_impl.signature_concrete_store_sites.postgres_store.len(), 1);
    Ok(())
}

#[test]
fn private_inherent_members_are_not_exposure_signatures() -> Result<()> {
    let private = concrete_facts("impl Adapter { fn inspect(_: &SqliteStore) {} const STORE: Option<PostgresStore> = None; }\n")?;
    let restricted = concrete_facts(
        "impl Adapter {\n\
             pub(crate) fn inspect(_: &SqliteStore) {}\n\
             pub(crate) const STORE: Option<PostgresStore> = None;\n\
         }\n",
    )?;
    let trait_impl = concrete_facts("impl Inspect for Adapter { fn inspect(_: &SqliteStore) {} const STORE: Option<PostgresStore> = None; }\n")?;

    assert!(private.signature_concrete_store_sites.sqlite_store.is_empty());
    assert!(private.signature_concrete_store_sites.postgres_store.is_empty());
    assert_eq!(restricted.signature_concrete_store_sites.sqlite_store.len(), 1);
    assert_eq!(restricted.signature_concrete_store_sites.postgres_store.len(), 1);
    assert_eq!(trait_impl.signature_concrete_store_sites.sqlite_store.len(), 1);
    assert_eq!(trait_impl.signature_concrete_store_sites.postgres_store.len(), 1);
    Ok(())
}

#[test]
fn type_and_trait_generic_bounds_are_signature_evidence() -> Result<()> {
    let facts = concrete_facts(
        "pub(crate) struct Adapter<T: Uses<SqliteStore>>(T);\n\
         pub(crate) enum Backend<T> where T: Uses<PostgresStore> { Active(T) }\n\
         pub(crate) union Slot<T: Copy + Uses<SqliteStore>> { value: T }\n\
         pub(crate) trait Routed<T>: Uses<PostgresStore> where T: Uses<SqliteStore> {}\n",
    )?;
    assert_eq!(facts.signature_concrete_store_sites.sqlite_store.len(), 3);
    assert_eq!(facts.signature_concrete_store_sites.postgres_store.len(), 2);
    Ok(())
}

#[test]
fn concrete_store_signature_identity_tracks_visibility() -> Result<()> {
    let private = concrete_facts("fn open() -> SqliteStore { SqliteStore::open() }\n")?;
    let restricted = concrete_facts("pub(crate) fn open() -> SqliteStore { SqliteStore::open() }\n")?;
    assert!(private.signature_concrete_store_sites.sqlite_store.is_empty());
    assert_eq!(restricted.signature_concrete_store_sites.sqlite_store.len(), 1);
    Ok(())
}

#[test]
fn canonical_binding_identity_tracks_trait_implementation_bodies() -> Result<()> {
    let first = concrete_facts("impl MemoryReader for SqliteStore { fn version(&self) -> u32 { 1 } }\n")?;
    let second = concrete_facts("impl MemoryReader for SqliteStore { fn version(&self) -> u32 { 2 } }\n")?;
    assert_ne!(first.binding_concrete_store_sites, second.binding_concrete_store_sites);
    assert_eq!(first.binding_concrete_store_sites.sqlite_store.len(), 1);
    assert_ne!(first.concrete_store_sites, second.concrete_store_sites);
    Ok(())
}

#[test]
fn private_helper_signatures_do_not_change_canonical_binding_identity() -> Result<()> {
    let canonical = concrete_facts("impl MemoryReader for SqliteStore { fn version(&self) -> u32 { 1 } }\n")?;
    let with_private_helper = concrete_facts(
        "impl MemoryReader for SqliteStore { fn version(&self) -> u32 { 1 } }\n\
         fn inspect(_: &SqliteStore) {}\n",
    )?;
    let with_restricted_helper = concrete_facts(
        "impl MemoryReader for SqliteStore { fn version(&self) -> u32 { 1 } }\n\
         pub(crate) fn inspect(_: &SqliteStore) {}\n",
    )?;
    assert_eq!(canonical.binding_concrete_store_sites, with_private_helper.binding_concrete_store_sites);
    assert_eq!(canonical.binding_concrete_store_sites, with_restricted_helper.binding_concrete_store_sites);
    assert_eq!(canonical.signature_concrete_store_sites, with_private_helper.signature_concrete_store_sites);
    assert_ne!(canonical.signature_concrete_store_sites, with_restricted_helper.signature_concrete_store_sites);
    Ok(())
}

#[test]
fn canonical_binding_identity_ignores_impl_and_method_documentation() -> Result<()> {
    let plain = concrete_facts("impl MemoryReader for SqliteStore { fn version(&self) -> u32 { 1 } }\n")?;
    let documented = concrete_facts(
        "/// Implementation documentation.\n\
         impl MemoryReader for SqliteStore {\n\
             /// Method documentation.\n\
             fn version(&self) -> u32 { 1 }\n\
         }\n",
    )?;
    let conditionally_documented = historical_concrete_facts(
        "#[cfg_attr(docsrs, doc = \"Implementation documentation.\")]\n\
         impl MemoryReader for SqliteStore {\n\
             #[cfg_attr(docsrs, cfg_attr(all(), doc = \"Method documentation.\"))]\n\
             fn version(&self) -> u32 { 1 }\n\
         }\n",
    )?;
    assert_eq!(plain.binding_concrete_store_sites, documented.binding_concrete_store_sites);
    assert_eq!(plain.binding_concrete_store_sites, conditionally_documented.binding_concrete_store_sites);
    Ok(())
}

#[test]
fn ordinary_occurrence_identity_ignores_documentation() -> Result<()> {
    let plain = historical_concrete_facts(
        "fn embedding_status() {\n\
             let sqlite = SqliteStore::status();\n\
             PostgresStore::record(sqlite);\n\
         }\n",
    )?;
    let documented = historical_concrete_facts(
        "#[cfg_attr(feature = \"other\", doc = \"Reports the conditional embedding status.\")]\n\
         #[cfg_attr(feature = \"other\", cfg_attr(feature = \"independent\", doc = \"Extended status details.\"))]\n\
         /// Reports the current embedding status.\n\
         fn embedding_status() {\n\
             #[cfg_attr(feature = \"other\", doc = \"The conditional local backend status.\")]\n\
             /// The local backend status.\n\
             let sqlite = SqliteStore::status();\n\
             PostgresStore::record(sqlite);\n\
         }\n",
    )?;
    let non_documentation = historical_concrete_facts(
        "#[cfg_attr(feature = \"optimized\", inline)]\n\
         fn embedding_status() {\n\
             let sqlite = SqliteStore::status();\n\
             PostgresStore::record(sqlite);\n\
         }\n",
    )?;
    assert_eq!(plain.concrete_store_sites, documented.concrete_store_sites);
    assert_ne!(plain.concrete_store_sites, non_documentation.concrete_store_sites);
    Ok(())
}

#[test]
fn canonical_binding_identity_tracks_cfg_and_ancestor_placement() -> Result<()> {
    let direct = concrete_facts("#[cfg(feature = \"legacy\")] impl MemoryReader for SqliteStore {}\n")?;
    let changed_cfg = concrete_facts("#[cfg(feature = \"current\")] impl MemoryReader for SqliteStore {}\n")?;
    let nested = concrete_facts("#[cfg(feature = \"legacy\")] mod legacy { impl MemoryReader for SqliteStore {} }\n")?;
    assert_ne!(direct.binding_concrete_store_sites, changed_cfg.binding_concrete_store_sites);
    assert_ne!(direct.binding_concrete_store_sites, nested.binding_concrete_store_sites);
    assert_eq!(direct.binding_concrete_store_sites.sqlite_store.len(), 1);
    assert_eq!(nested.binding_concrete_store_sites.sqlite_store.len(), 1);
    Ok(())
}

#[test]
fn every_serde_callback_key_is_scanned_as_rust() -> Result<()> {
    let source = "#[serde(skip_serializing_if = \"SqliteStore::is_empty\")]\n\
                  struct Skip(usize);\n\
                  #[serde(default = \"PostgresStore::default\")]\n\
                  struct Defaulted(usize);\n\
                  #[serde(getter = \"SqliteStore::get\")]\n\
                  struct Remote(usize);\n";
    assert_eq!(
        concrete_facts(source)?.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 2,
            postgres_store: 1,
        }
    );
    Ok(())
}

#[test]
fn concrete_store_names_cannot_be_hidden_by_aliases() {
    for source in [
        "type DefaultStore = SqliteStore;\n",
        "pub(crate) type DefaultStore = crate::store::PostgresStore;\n",
        "trait Backend { type Store = SqliteStore; }\n",
        "impl Backend for Adapter { type Store = PostgresStore; }\n",
    ] {
        assert!(
            imports("src/store/alias.rs", source)
                .unwrap_err()
                .to_string()
                .contains("cannot be hidden behind type aliases"),
            "{source}"
        );
    }

    for source in ["use crate::store::SqliteStore as DefaultStore;\n", "pub use crate::store::PostgresStore as DefaultStore;\n"] {
        assert!(
            imports("src/store/alias.rs", source)
                .unwrap_err()
                .to_string()
                .contains("cannot be hidden behind renamed imports"),
            "{source}"
        );
    }

    for source in [
        "macro_rules! alias { () => { type DefaultStore = SqliteStore; } }\n",
        "macro_rules! reexport { () => { pub use PostgresStore as DefaultStore; } }\n",
        "macro_rules! constructor { () => { SqliteStore::in_memory() } }\n",
        "macro_rules! bound { () => { #[serde(bound = \"crate::store::SqliteStore: Serialize\")] struct Generated; } }\n",
        "macro_rules! callback { () => { #[serde(default = r#\"crate::store::PostgresStore::default\"#)] struct Generated; } }\n",
        "macro_rules! conditional { () => { #[cfg_attr(feature = \"other\", serde(bound = \"crate::store::SqliteStore: Serialize\"))] struct Generated; } }\n",
    ] {
        assert!(
            imports("src/store/alias.rs", source)
                .unwrap_err()
                .to_string()
                .contains("macro definitions cannot inject concrete stores"),
            "{source}"
        );
    }

    for source in [
        "macro_rules! label { () => { \"SqliteStore\" } }\n",
        "macro_rules! diagnostic { () => { compile_error!(\"PostgresStore\") } }\n",
        "macro_rules! schema_data { () => { #[schemars(extend(\"x-backend\" = \"SqliteStore\"))] struct Generated; } }\n",
    ] {
        assert!(imports("src/store/alias.rs", source).is_ok(), "{source}");
    }
}

#[test]
fn macro_definitions_honor_test_only_cfg_gates() -> Result<()> {
    let facts = concrete_facts(
        "macro_rules! test_store {\n\
             () => {\n\
                 #[cfg(test)]\n\
                 type TestStore = $crate::SqliteStore;\n\
                 #[cfg(feature = \"testing\")]\n\
                 const VERSION: usize = PostgresStore::FORMAT_VERSION;\n\
             };\n\
         }\n",
    )?;
    assert_eq!(facts.concrete_stores, ConcreteStoreCounts::default());

    let production = "macro_rules! production_store { () => { #[cfg(not(test))] type DefaultStore = SqliteStore; }; }\n";
    let Err(error) = concrete_facts(production) else {
        panic!("a production macro transcriber cannot inject a concrete store");
    };
    assert!(error.to_string().contains("macro definitions cannot inject concrete stores"));
    Ok(())
}

#[test]
fn field_signature_identity_tracks_containing_type_and_variant() -> Result<()> {
    let internal = concrete_facts("struct Internal { pub(crate) store: SqliteStore }\n")?;
    let exposed = concrete_facts("pub struct Exposed { pub(crate) store: SqliteStore }\n")?;
    let first_variant = concrete_facts("pub enum Choice { First { store: PostgresStore }, Second }\n")?;
    let second_variant = concrete_facts("pub enum Choice { First, Second { store: PostgresStore } }\n")?;
    assert_ne!(internal.signature_concrete_store_sites, exposed.signature_concrete_store_sites);
    assert_ne!(first_variant.signature_concrete_store_sites, second_variant.signature_concrete_store_sites);
    Ok(())
}

#[test]
fn private_containers_and_items_are_not_exposure_signatures() -> Result<()> {
    let private = concrete_facts(
        "struct Cache<T: Uses<SqliteStore>> { store: SqliteStore, visible_inside: PostgresStore }\n\
         enum Internal { Store(PostgresStore) }\n\
         const SQLITE: SqliteStore = loop {};\n\
         static POSTGRES: PostgresStore = loop {};\n",
    )?;
    let exposed = concrete_facts(
        "pub(crate) struct Cache<T: Uses<SqliteStore>> { pub(crate) store: SqliteStore, hidden: PostgresStore }\n\
         pub(crate) enum Choice { Store(PostgresStore) }\n\
         pub(crate) const SQLITE: SqliteStore = loop {};\n\
         pub(crate) static POSTGRES: PostgresStore = loop {};\n",
    )?;

    assert!(private.signature_concrete_store_sites.sqlite_store.is_empty());
    assert!(private.signature_concrete_store_sites.postgres_store.is_empty());
    assert_eq!(exposed.signature_concrete_store_sites.sqlite_store.len(), 3);
    assert_eq!(exposed.signature_concrete_store_sites.postgres_store.len(), 2);
    Ok(())
}

#[test]
fn locally_visible_items_in_private_modules_are_latent_exposure_signatures() -> Result<()> {
    let private = concrete_facts("mod hidden { pub(crate) fn inspect(_: SqliteStore) {} }\n")?;
    let private_site = private.signature_concrete_store_sites.sqlite_store.first().expect("latent private-module signature");
    assert!(private_site.direct_exposure_cfg.is_none());

    let exposed = concrete_facts("pub(crate) mod hidden { pub(crate) fn inspect(_: SqliteStore) {} }\n")?;
    let exposed_site = exposed.signature_concrete_store_sites.sqlite_store.first().expect("directly exposed signature");
    assert!(exposed_site.direct_exposure_cfg.is_some());

    let nested_private = concrete_facts(
        "pub(crate) mod facade {\n\
             mod hidden { pub(crate) fn inspect(_: PostgresStore) {} }\n\
         }\n",
    )?;
    let nested_site = nested_private.signature_concrete_store_sites.postgres_store.first().expect("nested latent signature");
    assert!(nested_site.direct_exposure_cfg.is_none());
    Ok(())
}

#[test]
fn private_traits_and_self_restricted_items_are_not_exposure_signatures() -> Result<()> {
    let private = concrete_facts(
        "trait Internal { fn inspect(_: SqliteStore); const STORE: PostgresStore; }\n\
         pub(self) trait SelfOnly<T: Uses<SqliteStore>> { fn inspect(_: PostgresStore); }\n\
         pub(self) fn inspect(_: SqliteStore) -> PostgresStore { loop {} }\n\
         pub(self) struct Cache { pub(self) store: SqliteStore }\n\
         struct Holder;\n\
         impl Holder { pub(self) fn inspect(_: PostgresStore) {} }\n\
         impl SqliteStore {\n\
             pub(self) fn inspect_self(&self) {}\n\
             pub(in self) fn inspect_in_self(&self) {}\n\
         }\n",
    )?;
    let exposed = concrete_facts(
        "pub(crate) trait External<T: Uses<SqliteStore>> {\n\
             fn inspect(_: SqliteStore);\n\
             const STORE: PostgresStore;\n\
         }\n\
         pub(crate) fn inspect(_: SqliteStore) -> PostgresStore { loop {} }\n",
    )?;

    assert!(private.signature_concrete_store_sites.sqlite_store.is_empty());
    assert!(private.signature_concrete_store_sites.postgres_store.is_empty());
    assert_eq!(exposed.signature_concrete_store_sites.sqlite_store.len(), 3);
    assert_eq!(exposed.signature_concrete_store_sites.postgres_store.len(), 2);
    Ok(())
}

#[test]
fn signature_parameters_follow_production_cfg() -> Result<()> {
    let facts = concrete_facts(
        "pub fn inspect(#[cfg(test)] _: SqliteStore, #[cfg(feature = \"testing\")] _: PostgresStore) {}\n\
         pub struct Holder;\n\
         impl Holder { pub fn inspect(#[cfg(test)] _: SqliteStore) {} }\n\
         pub trait Reader { fn inspect(#[cfg(feature = \"testing\")] _: PostgresStore); }\n\
         pub struct Wrapper<#[cfg(test)] T = SqliteStore, #[cfg(feature = \"testing\")] U = PostgresStore> {\n\
             #[cfg(test)] sqlite: T,\n\
             #[cfg(feature = \"testing\")] postgres: U,\n\
         }\n\
         pub enum Choice<#[cfg(test)] T = SqliteStore> { #[cfg(test)] Store(T) }\n\
         pub union Either<#[cfg(feature = \"testing\")] T: Copy = PostgresStore> { #[cfg(feature = \"testing\")] store: T }\n\
         unsafe extern \"C\" { pub fn inspect(#[cfg(test)] value: SqliteStore); }\n",
    )?;
    assert!(facts.signature_concrete_store_sites.sqlite_store.is_empty());
    assert!(facts.signature_concrete_store_sites.postgres_store.is_empty());
    Ok(())
}

#[test]
fn concrete_store_sites_pin_generic_defaults_and_enclosing_syntax() -> Result<()> {
    let original = concrete_facts(
        "pub struct Service<S: MemoryReader = SqliteStore>(S);\n\
         fn register() { SqliteStore::register_extension(); }\n",
    )?;
    let whitespace_only = concrete_facts(
        "pub struct Service < S : MemoryReader = SqliteStore > (S);\n\
         fn register() {\n    SqliteStore :: register_extension();\n}\n",
    )?;
    assert_eq!(original.concrete_store_sites, whitespace_only.concrete_store_sites);
    assert_eq!(original.generic_default_concrete_store_sites.sqlite_store.len(), 1);
    assert!(
        original
            .concrete_store_sites
            .sqlite_store
            .contains(&original.generic_default_concrete_store_sites.sqlite_store[0])
    );

    let moved = concrete_facts(
        "pub struct Service<S: MemoryReader>(S, SqliteStore);\n\
         fn register() { SqliteStore::register_extension(); }\n",
    )?;
    assert_eq!(original.concrete_stores, moved.concrete_stores);
    assert_ne!(original.concrete_store_sites, moved.concrete_store_sites);
    assert!(moved.generic_default_concrete_store_sites.sqlite_store.is_empty());
    Ok(())
}

#[test]
fn const_generic_defaults_are_inventoried() -> Result<()> {
    let facts = concrete_facts(
        "struct SqliteVersion<const N: usize = { SqliteStore::FORMAT_VERSION }>;\n\
         struct PostgresVersion<const N: usize = { PostgresStore::FORMAT_VERSION }>;\n",
    )?;
    let whitespace_only = concrete_facts(
        "struct SqliteVersion < const N : usize = { SqliteStore :: FORMAT_VERSION } >;\n\
         struct PostgresVersion < const N : usize = { PostgresStore :: FORMAT_VERSION } >;\n",
    )?;
    assert_eq!(
        facts.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 1,
            postgres_store: 1,
        }
    );
    assert_eq!(facts.generic_default_concrete_store_sites, whitespace_only.generic_default_concrete_store_sites);
    assert_eq!(facts.generic_default_concrete_store_sites.sqlite_store.len(), 1);
    assert_eq!(facts.generic_default_concrete_store_sites.postgres_store.len(), 1);
    Ok(())
}

#[test]
fn generic_defaults_use_cfg_aware_syntax_traversal() -> Result<()> {
    let facts = concrete_facts(
        "struct Version<const N: usize = {\n\
             #[cfg(test)] let _test = core::mem::size_of::<SqliteStore>();\n\
             #[cfg(feature = \"testing\")] let _instrumentation = PostgresStore::FORMAT_VERSION;\n\
             #[cfg(not(test))] let production = 1;\n\
             production\n\
         }>;\n",
    )?;
    assert_eq!(facts.concrete_stores, ConcreteStoreCounts::default());
    assert_eq!(facts.generic_default_concrete_store_sites, ConcreteStoreSites::default());
    Ok(())
}

#[test]
fn generic_default_attributes_are_inventoried() -> Result<()> {
    let facts = concrete_facts(
        "struct Version<const N: usize = {\n\
             #[serde(bound = \"SqliteStore: Serialize\")]\n\
             struct AttributeBound;\n\
             1\n\
         }>;\n",
    )?;
    assert_eq!(facts.concrete_stores.sqlite_store, 1);
    assert_eq!(facts.generic_default_concrete_store_sites.sqlite_store.len(), 1);
    assert_eq!(facts.concrete_store_sites.sqlite_store, facts.generic_default_concrete_store_sites.sqlite_store);
    Ok(())
}

#[test]
fn canonical_store_names_and_test_only_aliases_remain_classifiable() -> Result<()> {
    let source = "pub use crate::store::SqliteStore;\n\
                  type MemoryMap = std::collections::HashMap<u64, String>;\n\
                  #[cfg(test)] type TestStore = PostgresStore;\n\
                  #[cfg(test)] use crate::store::SqliteStore as TestStoreAlias;\n\
                  #[cfg(test)] macro_rules! alias { () => { type TestStore = SqliteStore; } }\n";
    let syntax = syn::parse_file(source)?;
    let facts = production_syntax_facts(
        &syntax,
        "src/store/mod.rs",
        Some("src/lib.rs"),
        ProductionSyntaxOptions {
            collect_internal_imports: true,
            rust_2015_absolute_paths: false,
            require_reviewed_expansions: true,
        },
    )?;
    assert_eq!(
        facts.concrete_stores,
        ConcreteStoreCounts {
            sqlite_store: 1,
            postgres_store: 0,
        }
    );
    Ok(())
}
