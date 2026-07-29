use super::*;

#[test]
fn exposed_function_signatures_are_type_exposure_evidence() -> Result<()> {
    let facts = concrete_facts(
        "mod hidden {\n\
             pub(crate) struct Adapter;\n\
             impl Adapter { pub(crate) fn open() -> SqliteStore { loop {} } }\n\
         }\n\
         pub(crate) fn adapter(value: hidden::Adapter) -> hidden::Adapter { value }\n",
    )?;
    let adapter = facts
        .public_reexports
        .iter()
        .find(|evidence| evidence.target_path == ["store_fixture", "hidden", "Adapter"])
        .expect("function type exposure");
    assert_eq!(adapter.exported_path, ["store_fixture", "adapter"]);
    assert!(adapter.direct_exposure_cfg.is_some());
    Ok(())
}

#[test]
fn exposed_function_signature_types_resolve_aliases_without_treating_generics_as_items() -> Result<()> {
    let facts = concrete_facts(
        "mod hidden { pub(crate) struct Adapter; }\n\
         use hidden::Adapter as InternalAdapter;\n\
         pub(crate) fn adapter<T>(value: T) -> InternalAdapter { loop {} }\n",
    )?;
    assert_eq!(facts.public_reexports.len(), 1);
    assert_eq!(facts.public_reexports[0].target_path, ["store_fixture", "hidden", "Adapter"]);
    Ok(())
}

#[test]
fn exposed_qualified_function_signature_types_fail_closed() -> Result<()> {
    let source = "trait Reveal { type Output; }\n\
                  struct Marker;\n";
    let Err(error) = concrete_facts(&format!("{source}pub(crate) fn adapter() -> <Marker as Reveal>::Output {{ loop {{}} }}\n")) else {
        panic!("an exposed qualified signature type must fail closed");
    };
    assert!(error.to_string().contains("exposed qualified signature type"), "{error:#}");

    let private = concrete_facts(&format!("{source}fn adapter() -> <Marker as Reveal>::Output {{ loop {{}} }}\n"))?;
    assert!(private.public_reexports.is_empty());
    Ok(())
}

#[test]
fn unresolved_historical_projections_cannot_authorize_current_exposure() -> Result<()> {
    let source = "pub(crate) fn adapter() -> <Marker as Reveal>::Output { loop {} }\n";
    let historical = historical_concrete_facts(source)?;
    assert!(historical.public_reexports.is_empty());

    let Err(error) = concrete_facts(source) else {
        panic!("current qualified exposure must still fail closed");
    };
    assert!(error.to_string().contains("exposed qualified signature type"), "{error:#}");
    Ok(())
}

#[test]
fn exposed_method_signatures_are_type_exposure_evidence() -> Result<()> {
    let facts = concrete_facts(
        "mod hidden { pub(crate) struct Adapter; }\n\
         pub(crate) struct Provider<T>(T);\n\
         impl<T> Provider<T> {\n\
             pub(crate) fn adapter(&self, value: T) -> hidden::Adapter { loop {} }\n\
         }\n",
    )?;
    let adapter = facts
        .public_reexports
        .iter()
        .find(|evidence| evidence.target_path == ["store_fixture", "hidden", "Adapter"])
        .expect("method return type exposure");
    assert_eq!(adapter.exported_path, ["store_fixture", "Provider"]);
    assert!(adapter.direct_exposure_cfg.is_some());
    assert!(facts.public_reexports.iter().all(|evidence| evidence.target_path != ["store_fixture", "T"]));
    Ok(())
}

#[test]
fn exposed_field_types_are_type_exposure_evidence() -> Result<()> {
    for (source, container) in [
        (
            "mod hidden { pub(crate) struct Adapter; }\npub(crate) struct Wrapper<T> { pub(crate) inner: hidden::Adapter, hidden: T }\n",
            "Wrapper",
        ),
        (
            "mod hidden { pub(crate) struct Adapter; }\npub(crate) enum Wrapper<T> { Value(hidden::Adapter), Hidden(T) }\n",
            "Wrapper",
        ),
        (
            "mod hidden { pub(crate) struct Adapter; }\npub(crate) union Wrapper<T: Copy> { pub(crate) inner: core::mem::ManuallyDrop<hidden::Adapter>, hidden: T }\n",
            "Wrapper",
        ),
    ] {
        let facts = concrete_facts(source)?;
        let adapter = facts
            .public_reexports
            .iter()
            .find(|evidence| evidence.target_path == ["store_fixture", "hidden", "Adapter"])
            .expect("field type exposure");
        assert_eq!(adapter.exported_path, ["store_fixture", container]);
        assert!(adapter.direct_exposure_cfg.is_some());
        assert!(facts.public_reexports.iter().all(|evidence| evidence.target_path != ["store_fixture", "T"]));
    }
    Ok(())
}

#[test]
fn exposed_constant_and_static_types_are_type_exposure_evidence() -> Result<()> {
    let facts = concrete_facts(
        "mod hidden { pub(crate) struct Adapter; }\n\
         pub(crate) const ADAPTER: hidden::Adapter = loop {};\n\
         pub(crate) static STATIC_ADAPTER: hidden::Adapter = loop {};\n",
    )?;
    let exported = facts
        .public_reexports
        .iter()
        .filter(|evidence| evidence.target_path == ["store_fixture", "hidden", "Adapter"])
        .map(|evidence| evidence.exported_path.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        exported,
        BTreeSet::from([
            vec!["store_fixture".to_owned(), "ADAPTER".to_owned()],
            vec!["store_fixture".to_owned(), "STATIC_ADAPTER".to_owned()],
        ])
    );
    Ok(())
}

#[test]
fn trait_associated_type_targets_retain_self_and_trait_exposure_requirements() -> Result<()> {
    let facts = concrete_facts(
        "mod hidden { pub(crate) struct Adapter; }\n\
         pub(crate) struct Wrapper;\n\
         impl core::ops::Deref for Wrapper {\n\
             type Target = hidden::Adapter;\n\
             fn deref(&self) -> &Self::Target { loop {} }\n\
         }\n",
    )?;
    let adapter = facts
        .public_reexports
        .iter()
        .find(|evidence| evidence.target_path == ["store_fixture", "hidden", "Adapter"])
        .expect("associated type exposure");
    assert_eq!(adapter.exported_path, ["store_fixture", "Wrapper"]);
    assert_eq!(
        adapter.required_trait_path.as_ref().map(|path| path.join("::")).as_deref(),
        Some("store_fixture::core::ops::Deref")
    );
    assert!(adapter.direct_exposure_cfg.is_some());
    Ok(())
}
