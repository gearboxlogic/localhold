use super::*;

fn scan(source: &str, category: SourceCategory) -> Result<Vec<SourceSuppression>> {
    SourceScanner::scan("src/fixture.rs", "fixture", category, &syn::parse_file(source)?)
}

#[test]
fn records_each_lint_token_with_reason_and_stable_occurrence() -> Result<()> {
    let sites = scan(
        "#![expect(clippy::pedantic, reason = \"module contract\")]\n\
         #[expect(clippy::too_many_lines, clippy::too_many_lines, clippy::too_many_arguments, reason = \"legacy boundary\")]\n\
         fn serve() {}\n",
        SourceCategory::Production,
    )?;
    assert_eq!(sites.len(), 4);
    assert_eq!(sites[0].item, "<module>");
    assert_eq!(sites[0].scope, "module");
    assert_eq!(sites[0].lint, "clippy::pedantic");
    assert_eq!(sites[0].reason, "module contract");
    assert_eq!(sites[1].item, "serve");
    assert_eq!(sites[1].scope, "item-fn");
    assert_eq!(sites[1].reason, "legacy boundary");
    assert_eq!(
        sites[1..].iter().map(|site| (site.lint.as_str(), site.occurrence)).collect::<Vec<_>>(),
        [("clippy::too_many_lines", 0), ("clippy::too_many_lines", 1), ("clippy::too_many_arguments", 0),]
    );
    assert_eq!(sites[1].fingerprint, sites[2].fingerprint);
    Ok(())
}

#[test]
fn cfg_scopes_distinguish_production_and_test_suppressions() -> Result<()> {
    let sites = scan(
        "#[cfg(test)]\n\
         #[allow(clippy::unwrap_used, reason = \"test assertion\")]\n\
         fn test_only() {}\n\
         #[cfg_attr(test, expect(clippy::indexing_slicing, reason = \"test indexing\"))]\n\
         struct ConditionalTest;\n\
         #[cfg_attr(feature = \"other\", expect(clippy::too_many_lines, reason = \"production feature\"))]\n\
         struct ProductionFeature;\n\
         #[cfg_attr(feature = \"other\", cfg_attr(feature = \"other\", cfg(test)), allow(clippy::panic, reason = \"removed when active\"))]\n\
         struct CorrelatedRemoval;\n",
        SourceCategory::Production,
    )?;
    assert_eq!(
        sites.iter().map(|site| (site.item.as_str(), site.lint.as_str(), site.category)).collect::<Vec<_>>(),
        [
            ("test_only", "clippy::unwrap_used", SourceCategory::Test),
            ("ConditionalTest", "clippy::indexing_slicing", SourceCategory::Test),
            ("ProductionFeature", "clippy::too_many_lines", SourceCategory::Production),
            ("CorrelatedRemoval", "clippy::panic", SourceCategory::Test),
        ]
    );
    Ok(())
}

#[test]
fn enclosing_test_category_propagates_and_benchmark_category_is_preserved() -> Result<()> {
    let test_sites = scan(
        "#[cfg(test)] mod support { #[expect(clippy::panic, reason = \"test helper\")] fn fail() {} }\n",
        SourceCategory::Production,
    )?;
    assert_eq!(test_sites[0].item, "support::fail");
    assert_eq!(test_sites[0].category, SourceCategory::Test);

    let benchmark_sites = scan("#[allow(clippy::unwrap_used, reason = \"benchmark setup\")] fn bench() {}\n", SourceCategory::Benchmark)?;
    assert_eq!(benchmark_sites[0].category, SourceCategory::Benchmark);
    Ok(())
}

#[test]
fn nested_scopes_and_macro_carried_attributes_remain_distinct() -> Result<()> {
    let sites = scan(
        "struct Service;\n\
         impl Service {\n\
             #[expect(clippy::too_many_arguments, reason = \"trait mirror\")]\n\
             fn call() {\n\
                 #[allow(clippy::indexing_slicing, reason = \"checked local\")]\n\
                 let value = 1;\n\
             }\n\
         }\n\
         macro_rules! generated {\n\
             () => { #[expect(clippy::too_many_lines, reason = \"macro shape\")] fn expanded() {} };\n\
         }\n\
         macro_rules! generated_inner {\n\
             () => { mod hidden { #![expect(dead_code, reason = \"macro inner\")] fn unused() {} } };\n\
         }\n",
        SourceCategory::Production,
    )?;
    assert_eq!(sites.len(), 4);
    assert!(sites.iter().any(|site| site.item.ends_with("::call") && site.scope == "impl-fn" && !site.macro_carried));
    assert!(sites.iter().any(|site| site.item.ends_with("::call") && site.scope == "local" && !site.macro_carried));
    assert!(sites.iter().any(|site| site.item == "generated" && site.scope == "item-macro" && site.macro_carried));
    assert!(sites.iter().any(|site| site.item == "generated_inner" && site.scope == "item-macro" && site.macro_carried));
    Ok(())
}

#[test]
fn malformed_macro_carried_lint_attributes_fail_closed() {
    let error = scan(
        "macro_rules! generated { () => { #[allow = clippy::panic] fn expanded() {} }; }\n",
        SourceCategory::Production,
    )
    .unwrap_err();
    let report = format!("{error:#}");
    assert!(report.contains("classify macro-carried lint attribute"));
    assert!(report.contains("allow lint suppression must use list syntax"));
}

#[test]
fn unreviewed_procedural_attributes_and_derives_fail_closed() {
    for source in ["#[evil::Suppress] fn hidden() {}\n", "#[derive(evil::Suppress)] struct Hidden;\n"] {
        let error = scan(source, SourceCategory::Production).unwrap_err();
        assert!(format!("{error:#}").contains("could emit a lint suppression"));
    }
}

#[test]
fn unreviewed_function_like_macros_fail_closed() {
    let error = scan("evil::suppress! { fn hidden() {} }\n", SourceCategory::Production).unwrap_err();
    assert!(format!("{error:#}").contains("function-like macro expansion could emit a lint suppression"));
    scan("fn render() { let _message = format!(\"{}\", 1); }\n", SourceCategory::Production).expect("reviewed standard macro");
    scan("use anyhow::bail;\nfn stop() { bail!(\"stop\"); }\n", SourceCategory::Production).expect("reviewed external macro import");

    let alias = scan("use evil::suppress as bail;\nfn stop() { bail!(\"stop\"); }\n", SourceCategory::Production).unwrap_err();
    assert!(format!("{alias:#}").contains("does not come from reviewed expansion package anyhow"));

    let shadow = scan("mod anyhow {}\nfn stop() { anyhow::bail!(\"stop\"); }\n", SourceCategory::Production).unwrap_err();
    assert!(format!("{shadow:#}").contains("shadows a reviewed expansion package"));
}

#[test]
fn macro_metavariables_cannot_construct_attributes() {
    let error = scan(
        "macro_rules! generated { ($attribute:meta) => { #[$attribute] fn expanded() {} }; }\n",
        SourceCategory::Production,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("opaque macro-carried attribute could hide a lint suppression"));
}

#[test]
fn empty_reasons_are_inventoried_for_policy_rejection() -> Result<()> {
    let sites = scan("#[allow(clippy::panic)] fn unchecked() {}\n", SourceCategory::Production)?;
    assert_eq!(sites.len(), 1);
    assert!(sites[0].reason.is_empty());
    Ok(())
}

#[test]
fn warn_overrides_are_inventoried_for_policy_rejection() -> Result<()> {
    let sites = scan(
        "#[warn(clippy::unwrap_used, reason = \"lowered deny\")]\nfn unchecked() {}\n\
         #[cfg_attr(test, warn(clippy::panic, reason = \"conditional override\"))]\nfn conditional() {}\n",
        SourceCategory::Production,
    )?;
    assert_eq!(
        sites.iter().map(|site| (site.level.as_str(), site.lint.as_str(), site.category)).collect::<Vec<_>>(),
        [("warn", "clippy::unwrap_used", SourceCategory::Production), ("warn", "clippy::panic", SourceCategory::Test),]
    );
    Ok(())
}

#[test]
fn raw_identifiers_cannot_hide_lint_attributes() -> Result<()> {
    let sites = scan(
        "#[r#allow(clippy::panic, r#reason = \"raw allow\")]\nfn allowed() {}\n\
         #[r#warn(clippy::unwrap_used, reason = \"raw warn\")]\nfn warned() {}\n\
         #[r#cfg_attr(test, r#expect(clippy::indexing_slicing, reason = \"raw conditional\"))]\nfn conditional() {}\n",
        SourceCategory::Production,
    )?;
    assert_eq!(
        sites.iter().map(|site| (site.level.as_str(), site.lint.as_str(), site.reason.as_str())).collect::<Vec<_>>(),
        [
            ("allow", "clippy::panic", "raw allow"),
            ("warn", "clippy::unwrap_used", "raw warn"),
            ("expect", "clippy::indexing_slicing", "raw conditional"),
        ]
    );
    Ok(())
}

#[test]
fn runnable_doctests_and_explicit_doc_inputs_fail_closed() {
    let doctest = scan(
        "/// ```\n/// #![allow(unused_variables)]\n/// let hidden = 1;\n/// ```\nfn documented() {}\n",
        SourceCategory::Production,
    )
    .unwrap_err();
    assert!(doctest.to_string().contains("runnable Rust doctests"));

    let indented = scan(
        "/// Paragraph.\n///\n///     #![allow(unused_variables)]\n///     let hidden = 1;\nfn documented() {}\n",
        SourceCategory::Production,
    )
    .unwrap_err();
    assert!(indented.to_string().contains("runnable Rust doctests"));

    let included = scan("#[doc = include_str!(\"guide.md\")]\nfn documented() {}\n", SourceCategory::Production).unwrap_err();
    assert!(format!("{included:#}").contains("included doctest content cannot be audited"));

    let raw_included = scan("#[r#doc = include_str!(\"guide.md\")]\nfn documented() {}\n", SourceCategory::Production).unwrap_err();
    assert!(format!("{raw_included:#}").contains("included doctest content cannot be audited"));

    let conditional = scan(
        "#[cfg_attr(feature = \"guide\", doc = include_str!(\"guide.md\"))]\nfn documented() {}\n",
        SourceCategory::Production,
    )
    .unwrap_err();
    assert!(format!("{conditional:#}").contains("included doctest content cannot be audited"));

    let raw_conditional = scan(
        "#[r#cfg_attr(feature = \"guide\", r#doc = include_str!(\"guide.md\"))]\nfn documented() {}\n",
        SourceCategory::Production,
    )
    .unwrap_err();
    assert!(format!("{raw_conditional:#}").contains("included doctest content cannot be audited"));
}

#[test]
fn stable_ids_follow_the_reviewed_item_across_file_splits() -> Result<()> {
    let syntax = syn::parse_file("#[expect(clippy::too_many_lines, reason = \"legacy handler\")]\nfn serve() {}\n")?;
    let first = SourceScanner::scan("src/first.rs", "protocol", SourceCategory::Production, &syntax)?;
    let moved = SourceScanner::scan("src/split.rs", "protocol", SourceCategory::Production, &syntax)?;
    let transferred = SourceScanner::scan("src/split.rs", "transport", SourceCategory::Production, &syntax)?;
    let changed_signature = SourceScanner::scan(
        "src/split.rs",
        "protocol",
        SourceCategory::Production,
        &syn::parse_file("#[expect(clippy::too_many_lines, reason = \"legacy handler\")]\nfn serve(input: usize) {}\n")?,
    )?;
    assert_eq!(first[0].id, moved[0].id);
    assert_ne!(first[0].id, transferred[0].id);
    assert_ne!(first[0].id, changed_signature[0].id);
    assert!(first[0].signature.is_some());
    assert!(first[0].id.starts_with("source."));
    Ok(())
}

#[test]
fn target_fingerprints_ignore_sibling_suppression_debt() -> Result<()> {
    let stacked = scan(
        "#[expect(clippy::panic, reason = \"legacy panic\")]\n\
         #[expect(clippy::todo, reason = \"legacy placeholder\")]\n\
         fn serve() { panic!() }\n",
        SourceCategory::Production,
    )?;
    let reduced = scan(
        "#[expect(clippy::todo, reason = \"legacy placeholder\")]\n\
         fn serve() { panic!() }\n",
        SourceCategory::Production,
    )?;
    let stacked_todo = stacked.iter().find(|site| site.lint == "clippy::todo").expect("stacked todo");
    assert_eq!(stacked_todo.id, reduced[0].id);
    assert_eq!(stacked_todo.target, reduced[0].target);

    let shared_attribute = scan(
        "#[expect(clippy::panic, clippy::todo, reason = \"legacy paths\")]\n\
         fn shared() { panic!() }\n",
        SourceCategory::Production,
    )?;
    let reduced_attribute = scan(
        "#[expect(clippy::todo, reason = \"legacy paths\")]\n\
         fn shared() { panic!() }\n",
        SourceCategory::Production,
    )?;
    let shared_todo = shared_attribute.iter().find(|site| site.lint == "clippy::todo").expect("shared todo");
    assert_eq!(shared_todo.id, reduced_attribute[0].id);

    let conditional = scan(
        "#[cfg_attr(test, expect(clippy::panic, clippy::todo, reason = \"conditional paths\"), inline)]\n\
         fn conditional() { panic!() }\n",
        SourceCategory::Production,
    )?;
    let reduced_conditional = scan(
        "#[cfg_attr(test, expect(clippy::todo, reason = \"conditional paths\"), inline)]\n\
         fn conditional() { panic!() }\n",
        SourceCategory::Production,
    )?;
    let conditional_todo = conditional.iter().find(|site| site.lint == "clippy::todo").expect("conditional todo");
    assert_eq!(conditional_todo.id, reduced_conditional[0].id);
    assert_eq!(conditional_todo.target, reduced_conditional[0].target);
    Ok(())
}

#[test]
fn stable_ids_pin_item_bodies_and_complete_impl_headers() -> Result<()> {
    let first = scan(
        "#[expect(clippy::panic, reason = \"protocol failure\")]\nfn serve() { panic!() }\n",
        SourceCategory::Production,
    )?;
    let changed_body = scan(
        "#[expect(clippy::panic, reason = \"protocol failure\")]\nfn serve() { println!(\"retry\") }\n",
        SourceCategory::Production,
    )?;
    assert_ne!(first[0].id, changed_body[0].id);

    let first_impl = scan(
        "trait First { fn serve(&self); }\n\
         struct Service;\n\
         impl First for Service {\n\
             #[expect(clippy::panic, reason = \"protocol failure\")]\n\
             fn serve(&self) { panic!() }\n\
         }\n",
        SourceCategory::Production,
    )?;
    let second_impl = scan(
        "trait Second { fn serve(&self); }\n\
         struct Service;\n\
         impl Second for Service {\n\
             #[expect(clippy::panic, reason = \"protocol failure\")]\n\
             fn serve(&self) { panic!() }\n\
         }\n",
        SourceCategory::Production,
    )?;
    assert_ne!(first_impl[0].id, second_impl[0].id);
    Ok(())
}

#[test]
fn argument_and_use_suppressions_pin_their_exact_targets() -> Result<()> {
    let first_argument = scan(
        "fn serve(#[expect(unused_variables, reason = \"trait shape\")] first: usize, second: usize) {}\n",
        SourceCategory::Production,
    )?;
    let second_argument = scan(
        "fn serve(first: usize, #[expect(unused_variables, reason = \"trait shape\")] second: usize) {}\n",
        SourceCategory::Production,
    )?;
    assert_ne!(first_argument[0].id, second_argument[0].id);

    let first_use = scan("#[expect(unused_imports, reason = \"platform route\")]\nuse first::Route;\n", SourceCategory::Production)?;
    let second_use = scan("#[expect(unused_imports, reason = \"platform route\")]\nuse second::Route;\n", SourceCategory::Production)?;
    assert_ne!(first_use[0].id, second_use[0].id);
    Ok(())
}

#[test]
fn stable_ids_pin_anonymous_targets_and_duplicate_cardinality() -> Result<()> {
    let first = scan(
        "fn serve() {\n\
             #[expect(clippy::indexing_slicing, reason = \"validated offset\")]\n\
             values[0];\n\
         }\n",
        SourceCategory::Production,
    )?;
    let moved = scan(
        "fn serve() {\n\
             #[expect(clippy::indexing_slicing, reason = \"validated offset\")]\n\
             other[0];\n\
         }\n",
        SourceCategory::Production,
    )?;
    assert_ne!(first[0].id, moved[0].id);
    assert!(first[0].target.is_some());

    let duplicates = scan(
        "fn serve() {\n\
             #[expect(clippy::indexing_slicing, reason = \"validated offset\")]\n\
             values[0];\n\
             #[expect(clippy::indexing_slicing, reason = \"validated offset\")]\n\
             values[0];\n\
         }\n",
        SourceCategory::Production,
    )?;
    assert_eq!(duplicates[0].id, duplicates[1].id);
    assert_eq!([duplicates[0].occurrence, duplicates[1].occurrence], [0, 1]);
    Ok(())
}
