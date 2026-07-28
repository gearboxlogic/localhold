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
                  #[cfg_attr(all(), cfg_attr(all(), cfg(test)))]\nuse crate::server::NestedTestOnly;\n\
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
    assert_eq!(production_imports(&syntax, "src/core.rs", Some("src/core.rs"), false)?, ["crate::server::AtRoot"]);
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
fn absolute_external_paths_are_not_classified_as_crate_relative() {
    let source = "use ::server::External;\n\
                  use ::ui::External as ExternalUi;\n\
                  fn build() -> ::server::Qualified { ::ui::qualified() }\n";
    assert!(imports("src/adapter.rs", source).expect("absolute external paths").is_empty());
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
fn path_valued_attribute_literals_count_concrete_stores() -> Result<()> {
    let source = "#[serde(serialize_with = \"crate::store::SqliteStore\")]\nstruct Record;\n\
                  #[serde(rename = \"PostgresStore\")]\nstruct Renamed;\n\
                  #[cfg_attr(test, serde(serialize_with = \"crate::store::SqliteStore\"))]\nstruct TestOnly;\n\
                  #[cfg_attr(feature = \"other\", serde(serialize_with = \"crate::store::PostgresStore\"))]\nstruct Production;\n";
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
            sqlite_store: 1,
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
    ] {
        assert!(
            imports("src/store/alias.rs", source)
                .unwrap_err()
                .to_string()
                .contains("macro-generated aliases or renamed imports"),
            "{source}"
        );
    }
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
