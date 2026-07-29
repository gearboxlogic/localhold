use anyhow::Context as _;

use super::*;

#[test]
fn restricted_visibilities_are_counted_only_in_production_syntax() -> Result<()> {
    let source = "pub(crate) struct CrateVisible;\n\
                  pub(in crate::adapter) fn narrowed() {}\n\
                  mod nested { pub(super) fn parent_visible() {} pub(in super::deeper) fn deeper() {} }\n\
                  #[cfg(test)] pub(crate) fn test_only() {}\n\
                  #[cfg(feature = \"testing\")] pub(super) fn testing_only() {}\n\
                  const TEXT: &str = \"pub(crate) pub(super)\";\n";
    assert_eq!(concrete_facts(source)?.visibilities, VisibilityCounts { pub_crate: 2, pub_super: 2 });
    Ok(())
}

#[test]
fn macro_token_visibilities_are_counted_without_plain_text() -> Result<()> {
    let source = "macro_rules! define_memory_columns { () => { pub(crate) fn generated() {} pub(in super) struct Parent; } }\n\
                  define_memory_columns!();\n\
                  const TEXT: &str = \"pub(crate)\";\n";
    assert_eq!(concrete_facts(source)?.visibilities, VisibilityCounts { pub_crate: 1, pub_super: 1 });
    Ok(())
}

#[test]
fn restricted_visibility_macro_requires_one_direct_production_invocation() -> Result<()> {
    let definition = "macro_rules! define_memory_columns { () => { pub(crate) fn generated() {} } }\n";
    let uninvoked = concrete_facts(definition).err().context("uninvoked visibility macro should fail")?;
    assert!(uninvoked.to_string().contains("exactly one direct production invocation"));

    let duplicated = concrete_facts(&format!("{definition}define_memory_columns!();\ndefine_memory_columns!();\n"))
        .err()
        .context("duplicated visibility macro invocation should fail")?;
    assert!(duplicated.to_string().contains("observed 2"));
    Ok(())
}

#[test]
fn visibility_macro_invocations_are_resolved_by_module_and_path() -> Result<()> {
    let separate_modules = "mod one {\n\
                                macro_rules! define_memory_columns { () => { pub(crate) struct Generated; } }\n\
                                define_memory_columns!();\n\
                            }\n\
                            mod two {\n\
                                macro_rules! define_memory_columns { () => { struct Private; } }\n\
                                define_memory_columns!();\n\
                            }\n";
    assert_eq!(concrete_facts(separate_modules)?.visibilities, VisibilityCounts { pub_crate: 1, pub_super: 0 });

    let external_collision = "mod local {\n\
                                  macro_rules! poll { () => { pub(crate) struct Generated; } }\n\
                              }\n\
                              futures::poll!();\n";
    let error = concrete_facts(external_collision)
        .err()
        .context("qualified external macro must not satisfy a local visibility macro invocation")?;
    assert!(error.to_string().contains("observed 0"), "{error:#}");
    Ok(())
}

#[test]
fn restricted_visibility_cannot_be_repeated_or_vary_by_macro_arm() -> Result<()> {
    let repeated = "macro_rules! define_memory_columns { ($($name:ident),+) => { $(pub(crate) struct $name;)+ } }\n\
                    define_memory_columns!(One, Two);\n";
    let error = concrete_facts(repeated).err().context("repeated visibility should fail")?;
    assert!(error.to_string().contains("cannot repeat restricted visibility"));

    let multiple_arms = "macro_rules! define_memory_columns {\n\
                            () => { pub(crate) struct One; };\n\
                            ($name:ident) => { pub(crate) struct $name; };\n\
                        }\n\
                        define_memory_columns!();\n";
    let error = concrete_facts(multiple_arms).err().context("multi-arm visibility should fail")?;
    assert!(error.to_string().contains("exactly one expansion arm"));
    Ok(())
}

#[test]
fn restricted_visibility_macro_cannot_be_invoked_indirectly() -> Result<()> {
    let source = "macro_rules! define_memory_columns { () => { pub(crate) struct Generated; } }\n\
                  macro_rules! concat_with_sep { () => { define_memory_columns!(); } }\n\
                  concat_with_sep!();\n";
    let error = concrete_facts(source).err().context("indirect visibility macro invocation should fail")?;
    assert!(error.to_string().contains("cannot be invoked indirectly"));
    Ok(())
}

#[test]
fn restricted_visibility_macro_cannot_define_a_nested_macro() -> Result<()> {
    let source = "macro_rules! outer {\n\
                      () => {\n\
                          macro_rules! generated { () => { pub(crate) struct Generated; } }\n\
                          generated!();\n\
                          generated!();\n\
                      }\n\
                  }\n\
                  outer!();\n";
    let error = concrete_facts(source).err().context("nested visibility macro definition should fail")?;
    assert!(error.to_string().contains("cannot define nested macros"), "{error:#}");
    Ok(())
}

#[test]
fn restricted_visibility_macro_cannot_be_imported_or_exported() -> Result<()> {
    let imported = "mod definitions {\n\
                        macro_rules! define_memory_columns {\n\
                            ($name:ident) => { pub(crate) struct $name; }\n\
                        }\n\
                        define_memory_columns!(Direct);\n\
                        pub(super) use define_memory_columns;\n\
                    }\n\
                    mod consumer {\n\
                        use super::definitions::define_memory_columns;\n\
                        define_memory_columns!(Indirect);\n\
                    }\n";
    let error = concrete_facts(imported).err().context("imported visibility macro should fail")?;
    assert!(error.to_string().contains("cannot be imported"), "{error:#}");

    let exported = "#[cfg_attr(feature = \"other\", macro_export)]\n\
                    macro_rules! define_memory_columns { () => { pub(crate) struct Generated; } }\n\
                    define_memory_columns!();\n";
    let error = concrete_facts(exported).err().context("exported visibility macro should fail")?;
    assert!(error.to_string().contains("cannot be exported"), "{error:#}");

    let test_only_export = "#[cfg_attr(test, macro_export)]\n\
                            macro_rules! define_memory_columns { () => { pub(crate) struct Generated; } }\n\
                            define_memory_columns!();\n";
    assert_eq!(concrete_facts(test_only_export)?.visibilities, VisibilityCounts { pub_crate: 1, pub_super: 0 });
    Ok(())
}

#[test]
fn macro_arguments_cannot_supply_or_construct_restricted_visibility() -> Result<()> {
    let direct = "macro_rules! define_memory_columns { ($($tokens:tt)*) => { $($tokens)* } }\n\
                  define_memory_columns!(pub(crate) struct Generated;);\n";
    let error = concrete_facts(direct).err().context("direct visibility macro input should fail")?;
    assert!(error.to_string().contains("invocation arguments cannot supply"));

    let constructed = "macro_rules! define_memory_columns { ($prefix:ident, $scope:ident) => { $prefix($scope) struct Generated; } }\n\
                       define_memory_columns!(pub, crate);\n";
    let error = concrete_facts(constructed).err().context("constructed visibility macro input should fail")?;
    assert!(error.to_string().contains("cannot construct restricted visibility from metavariables"));
    Ok(())
}

#[test]
fn macro_transcribers_cannot_compose_restricted_visibility_from_one_argument() -> Result<()> {
    for source in [
        "macro_rules! define_memory_columns { ($scope:ident) => { pub($scope) struct Generated; } }\n\
         define_memory_columns!(crate);\n",
        "macro_rules! define_memory_columns { ($scope:ident) => { pub(in $scope) struct Generated; } }\n\
         define_memory_columns!(crate);\n",
        "macro_rules! define_memory_columns { ($restriction:tt) => { pub $restriction struct Generated; } }\n\
         define_memory_columns!((crate));\n",
        "macro_rules! define_memory_columns { ($prefix:ident) => { $prefix(crate) struct Generated; } }\n\
         define_memory_columns!(pub);\n",
    ] {
        let error = concrete_facts(source).err().context("metavariable-composed visibility should fail")?;
        assert!(error.to_string().contains("cannot construct restricted visibility from metavariables"));
    }
    Ok(())
}

#[test]
fn stringify_arguments_do_not_create_visibility_macro_evidence() -> Result<()> {
    let source = "macro_rules! define_memory_columns { () => { pub(crate) struct Generated; ::core::stringify!(pub(super) struct Text); } }\n\
                  define_memory_columns!();\n";
    assert_eq!(concrete_facts(source)?.visibilities, VisibilityCounts { pub_crate: 1, pub_super: 0 });
    Ok(())
}

#[test]
fn cfg_attr_visibility_tokens_follow_production_cfg_semantics() -> Result<()> {
    let source = "#[cfg_attr(test, serde(something(pub(crate))))]\n\
                  #[cfg_attr(feature = \"testing\", serde(something(pub(super))))]\n\
                  struct TestOnly;\n\
                  #[cfg_attr(feature = \"other\", serde(something(pub(crate))))]\n\
                  #[cfg_attr(all(), cfg_attr(feature = \"other\", serde(something(pub(super)))))]\n\
                  #[cfg_attr(feature = \"other\", cfg_attr(feature = \"independent\", cfg(test)), serde(something(pub(super))))]\n\
                  struct ProductionCapable;\n";
    assert_eq!(concrete_facts(source)?.visibilities, VisibilityCounts { pub_crate: 1, pub_super: 2 });
    Ok(())
}

#[test]
fn cfg_attr_disabling_siblings_remove_visibility_tokens_from_production() -> Result<()> {
    let source = "#[cfg_attr(feature = \"other\", cfg(test), serde(something(pub(crate))))]\n\
                  struct Direct;\n\
                  #[cfg_attr(feature = \"other\", cfg_attr(all(), cfg(feature = \"testing\")), serde(something(pub(super))))]\n\
                  struct Nested;\n\
                  #[cfg_attr(feature = \"other\", cfg_attr(feature = \"other\", cfg(test)), serde(something(pub(crate))))]\n\
                  struct RepeatedCondition;\n";
    assert_eq!(concrete_facts(source)?.visibilities, VisibilityCounts::default());
    Ok(())
}
