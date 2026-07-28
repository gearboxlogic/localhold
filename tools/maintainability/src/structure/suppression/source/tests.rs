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
         }\n",
        SourceCategory::Production,
    )?;
    assert_eq!(sites.len(), 3);
    assert!(sites.iter().any(|site| site.item.ends_with("::call") && site.scope == "impl-fn" && !site.macro_carried));
    assert!(sites.iter().any(|site| site.item.ends_with("::call") && site.scope == "local" && !site.macro_carried));
    assert!(sites.iter().any(|site| site.item == "generated" && site.scope == "item-macro" && site.macro_carried));
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
fn empty_reasons_are_inventoried_for_policy_rejection() -> Result<()> {
    let sites = scan("#[allow(clippy::panic)] fn unchecked() {}\n", SourceCategory::Production)?;
    assert_eq!(sites.len(), 1);
    assert!(sites[0].reason.is_empty());
    Ok(())
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
