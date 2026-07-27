use std::fs;

use tempfile::{TempDir, tempdir};

use super::{SiteKind, scan_workspace};

const AUDITED_ROOTS: [&str; 3] = ["src", "tests", "benches"];

fn test_workspace() -> TempDir {
    let workspace = tempdir().expect("temporary workspace");
    for root in AUDITED_ROOTS {
        fs::create_dir(workspace.path().join(root)).expect("tracked root");
    }
    workspace
}

fn scan_test_workspace(workspace: &TempDir) -> anyhow::Result<Vec<super::UnsafeSite>> {
    scan_workspace(workspace.path(), &AUDITED_ROOTS.map(str::to_owned))
}

fn scan_result(source: &str) -> anyhow::Result<Vec<super::UnsafeSite>> {
    let workspace = test_workspace();
    fs::write(workspace.path().join("src/sample.rs"), source).expect("sample source");
    scan_test_workspace(&workspace)
}

fn scan(source: &str) -> Vec<super::UnsafeSite> {
    scan_result(source).expect("scan succeeds")
}

fn assert_source_expansion_rejected(source: &str) {
    let error = scan_result(source).expect_err("source expansion must fail closed");
    assert!(error.to_string().contains("unsupported or unaudited Rust source construct"), "unexpected error: {error:#}");
}

fn assert_unsafe_macro_rejected(source: &str) {
    let error = scan_result(source).expect_err("macro-generated unsafe syntax must fail closed");
    assert!(error.to_string().contains("unsupported or unaudited Rust source construct"), "unexpected error: {error:#}");
}

#[test]
fn rejects_root_build_script() {
    let workspace = test_workspace();
    fs::write(workspace.path().join("build.rs"), "fn main() {}").expect("root build script");

    let error = scan_test_workspace(&workspace).expect_err("root build script must fail closed");
    assert!(error.to_string().contains("root build.rs is not supported"), "unexpected error: {error:#}");
}

#[test]
fn scans_optional_examples_root() {
    let workspace = test_workspace();
    fs::create_dir(workspace.path().join("examples")).expect("examples root");
    fs::write(workspace.path().join("examples/sample.rs"), "fn sample() { unsafe { work() } }").expect("example source");

    let sites = scan_test_workspace(&workspace).expect("scan succeeds");
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].path, "examples/sample.rs");
    assert_eq!(sites[0].kind, SiteKind::Block);
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_source_root() {
    use std::os::unix::fs::symlink;

    let workspace = test_workspace();
    fs::remove_dir(workspace.path().join("src")).expect("remove tracked root");
    fs::create_dir(workspace.path().join("real-src")).expect("symlink target");
    symlink(workspace.path().join("real-src"), workspace.path().join("src")).expect("symlinked root");

    let error = scan_test_workspace(&workspace).expect_err("symlinked root must fail closed");
    assert!(error.to_string().contains("tracked source root cannot be a symlink"), "unexpected error: {error:#}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_source_entry() {
    use std::os::unix::fs::symlink;

    let workspace = test_workspace();
    fs::write(workspace.path().join("sample.rs"), "fn sample() {}").expect("symlink target");
    symlink(workspace.path().join("sample.rs"), workspace.path().join("src/sample.rs")).expect("symlinked entry");

    let error = scan_test_workspace(&workspace).expect_err("symlinked entry must fail closed");
    assert!(
        error.to_string().contains("tracked Rust source tree cannot contain symlinks"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn ignores_non_executable_unsafe_text_and_function_pointer_types() {
    let sites = scan(
        r#"
        const LABEL: &str = "unsafe { ignored() }";
        const POINTER: unsafe extern "C" fn() = external;
        // unsafe { ignored() }
        fn r#unsafe() {}
        "#,
    );
    assert!(sites.is_empty(), "unexpected sites: {sites:?}");
}

#[test]
fn detects_executable_forms_and_exceptions() {
    let sites = scan(
        r#"
        #[expect(unsafe_code, reason = "contract")]
        #[cfg_attr(any(), expect(unsafe_op_in_unsafe_fn, reason = "contract"))]
        #[expect(clippy::undocumented_unsafe_blocks, reason = "contract")]
        #[expect(clippy::restriction, reason = "contract")]
        #[expect(clippy::all, reason = "contract")]
        #[warn(unsafe_code)]
        #[cfg_attr(any(), warn(clippy::undocumented_unsafe_blocks))]
        unsafe fn top() { unsafe { work() } }
        unsafe trait Boundary {}
        unsafe impl Boundary for Value {}
        unsafe extern "C" { unsafe fn foreign(); }
        #[unsafe(no_mangle)]
        static mut MUTABLE: u8 = 0;
        core::arch::global_asm!("");
        "#,
    );
    let kinds: Vec<_> = sites.iter().map(|site| site.kind).collect();
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::LintException).count(), 7);
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::Function).count(), 2);
    assert!(kinds.contains(&SiteKind::Block));
    assert!(kinds.contains(&SiteKind::Trait));
    assert!(kinds.contains(&SiteKind::Impl));
    assert!(kinds.contains(&SiteKind::ExternBlock));
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::MacroInput).count(), 1);
    assert!(kinds.contains(&SiteKind::Attribute));
    assert!(kinds.contains(&SiteKind::MutableStatic));
}

#[test]
fn cfg_attr_lint_exceptions_require_one_weakening_nested_attribute() {
    let sites = scan(
        r#"
        #[cfg_attr(windows, warn(dead_code), deny(unsafe_code))]
        fn unrelated_warning() {}

        #[cfg_attr(windows, deny(dead_code), warn(unsafe_code))]
        fn direct_exception() {}

        #[cfg_attr(windows, cfg_attr(target_arch = "x86_64", expect(clippy::undocumented_unsafe_blocks)))]
        fn nested_exception() {}
        "#,
    );
    assert_eq!(sites.iter().filter(|site| site.kind == SiteKind::LintException).count(), 2);
}

#[test]
fn tracks_nested_item_and_occurrence() {
    let sites = scan(
        r"
        impl Store {
            fn register() {
                unsafe { first() }
                unsafe { second() }
            }
        }
        ",
    );
    assert_eq!(sites.len(), 2);
    assert!(sites.iter().all(|site| site.item == "Store::register"));
    assert_eq!(sites[0].occurrence, 0);
    assert_eq!(sites[1].occurrence, 1);
    assert_ne!(sites[0].fingerprint, sites[1].fingerprint);
}

#[test]
fn records_the_executable_keyword_location() {
    let sites = scan("fn boundary() { unsafe { operation() } }");
    assert_eq!(sites.len(), 1);
    assert!(sites[0].source_range.contains(1, 17));
    assert!(!sites[0].source_range.contains(1, 16));
}

#[test]
fn boundary_fingerprint_covers_safe_setup_around_an_operation() {
    let first = scan("fn boundary() { let pointer = first; unsafe { call(pointer) } }");
    let second = scan("fn boundary() { let pointer = second; unsafe { call(pointer) } }");
    assert_eq!(first[0].fingerprint, second[0].fingerprint);
    assert_ne!(first[0].boundary_fingerprint, second[0].boundary_fingerprint);
}

#[test]
fn const_boundary_fingerprint_covers_safe_setup_around_an_operation() {
    let first = scan("const VALUE: usize = { let pointer = first; unsafe { call(pointer) } };");
    let second = scan("const VALUE: usize = { let pointer = second; unsafe { call(pointer) } };");
    assert_eq!(first[0].item, "VALUE");
    assert_eq!(first[0].fingerprint, second[0].fingerprint);
    assert_ne!(first[0].boundary_fingerprint, second[0].boundary_fingerprint);
}

#[test]
fn const_expression_item_boundaries_cover_safe_setup_around_an_operation() {
    for (first, second, item) in [
        (
            "type Value = [u8; { let pointer = first; unsafe { call(pointer) } }];",
            "type Value = [u8; { let pointer = second; unsafe { call(pointer) } }];",
            "Value",
        ),
        (
            "enum Value { Entry = { let pointer = first; unsafe { call(pointer) } } }",
            "enum Value { Entry = { let pointer = second; unsafe { call(pointer) } } }",
            "Value",
        ),
        (
            "struct Value { bytes: [u8; { let pointer = first; unsafe { call(pointer) } }] }",
            "struct Value { bytes: [u8; { let pointer = second; unsafe { call(pointer) } }] }",
            "Value",
        ),
        (
            "union Value { bytes: [u8; { let pointer = first; unsafe { call(pointer) } }] }",
            "union Value { bytes: [u8; { let pointer = second; unsafe { call(pointer) } }] }",
            "Value",
        ),
        (
            "impl Store { const VALUE: usize = { let pointer = first; unsafe { call(pointer) } }; }",
            "impl Store { const VALUE: usize = { let pointer = second; unsafe { call(pointer) } }; }",
            "Store::VALUE",
        ),
        (
            "impl Store { type Value = [u8; { let pointer = first; unsafe { call(pointer) } }]; }",
            "impl Store { type Value = [u8; { let pointer = second; unsafe { call(pointer) } }]; }",
            "Store::Value",
        ),
        (
            "trait Store { const VALUE: usize = { let pointer = first; unsafe { call(pointer) } }; }",
            "trait Store { const VALUE: usize = { let pointer = second; unsafe { call(pointer) } }; }",
            "Store::VALUE",
        ),
        (
            "trait Store { type Value = [u8; { let pointer = first; unsafe { call(pointer) } }]; }",
            "trait Store { type Value = [u8; { let pointer = second; unsafe { call(pointer) } }]; }",
            "Store::Value",
        ),
        (
            "trait Value = Store<{ let pointer = first; unsafe { call(pointer) } }>;",
            "trait Value = Store<{ let pointer = second; unsafe { call(pointer) } }>;",
            "Value",
        ),
    ] {
        let first = scan(first);
        let second = scan(second);
        assert_eq!(first[0].item, item);
        assert_eq!(first[0].fingerprint, second[0].fingerprint);
        assert_ne!(first[0].boundary_fingerprint, second[0].boundary_fingerprint);
    }
}

#[test]
fn rejects_source_expansion_outside_audited_files() {
    assert_source_expansion_rejected(r#"#[path = "../outside.rs"] mod outside;"#);
    assert_source_expansion_rejected(r#"#[r#path = "../outside.rs"] mod outside;"#);
    assert_source_expansion_rejected(r#"include!("../outside.rs");"#);
    assert_source_expansion_rejected(r#"r#include!("../outside.rs");"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { () => { include!("../outside.rs"); } }"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { () => { #[path = "../outside.rs"] mod outside; } }"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { () => { #![path = "../outside.rs"] mod outside; } }"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { () => { #![cfg_attr(any(), path = "../outside.rs")] mod outside; } }"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($m:ident) => { $m!("../outside.rs") } } expand!(include);"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($a:ident) => { #[$a = "../outside.rs"] mod outside; } } expand!(path);"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($a:ident ;) => { #[$a = "../outside.rs"] mod outside; } } expand!(path;);"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($a:ident, $m:ident) => { #[$a = "../outside.rs"] $m outside; } } expand!(path, mod);"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($a:tt) => { # $a mod outside; } } expand!([path = "../outside.rs"]);"#);
    assert_source_expansion_rejected(r"macro_rules! expand { ($a:tt) => { #! $a } } expand!([allow(unsafe_code)]);");
    assert_source_expansion_rejected(r#"use std::include as source; source!("../outside.rs");"#);
}

#[test]
fn standalone_assembly_invocations_are_fingerprinted() {
    let first = scan(r#"core::arch::global_asm!(".byte 0");"#);
    let second = scan(r#"core::arch::global_asm!(".byte 1");"#);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].kind, SiteKind::MacroInput);
    assert_ne!(first[0].fingerprint, second[0].fingerprint);
    for source in [r#"opaque::global_asm!("");"#, r#"global_asm!("");"#, r#"core::arch::llvm_asm!("");"#] {
        let error = scan_result(source).expect_err("only reviewed full assembly macro paths may be inventoried");
        assert!(error.to_string().contains("unreviewed macro path"), "unexpected error: {error:#}");
    }
}

#[test]
fn rejects_imported_or_aliased_assembly_macros() {
    for source in [
        r#"use std::arch::global_asm; global_asm!("");"#,
        r#"use std::arch::global_asm as assembly; assembly!("");"#,
        r#"pub use std::arch::{r#global_asm as assembly}; assembly!("");"#,
        r#"use std::arch::asm as assembly; fn sample() { unsafe { assembly!(""); } }"#,
    ] {
        let error = scan_result(source).expect_err("assembly imports must fail closed");
        assert!(error.to_string().contains("imports an assembly macro"), "unexpected error: {error:#}");
    }
}

#[test]
fn rejects_macro_generated_unsafe_constructs() {
    for source in [
        r"
        macro_rules! unsafe_wrap { ($body:block) => { unsafe $body } }
        fn sample() { unsafe_wrap!({ *pointer }); }
        ",
        r#"
        macro_rules! assemble { ($code:expr) => { core::arch::global_asm!($code); } }
        fn sample() { assemble!(".byte 0"); }
        "#,
        r#"
        macro_rules! assemble { ($code:expr) => { core::arch::asm!($code); } }
        fn sample() { unsafe { assemble!("nop"); } }
        "#,
        r"
        macro_rules! hide {
            () => { #[allow(unsafe_code)] #[allow(dead_code)] static mut VALUE: i32 = 0; }
        }
        hide!();
        ",
        r"
        #[allow(unsafe_code, dead_code)]
        mod hidden {
            macro_rules! make_static { ($m:ident) => { static $m VALUE: i32 = 0; } }
            make_static!(mut);
        }
        ",
        r"
        macro_rules! identity { ($body:block) => { $body } }
        fn sample() { identity!({ unsafe { work() } }); }
        ",
    ] {
        assert_unsafe_macro_rejected(source);
    }
}

#[test]
fn rejects_opaque_token_pasting_independent_of_macro_name() {
    for source in [r"pastey::paste! { fn [< un safe >]() {} }", r"aliased::builder! { nested! { [< un safe _ code >] } }"] {
        let error = scan_result(source).expect_err("token pasting must fail closed");
        assert!(error.to_string().contains("opaque token-pasting macro input"), "unexpected error: {error:#}");
    }
}

#[test]
fn rejects_unreviewed_macro_and_attribute_paths() {
    for source in [
        "fn sample() { opaque::expand!(); }",
        "#[opaque::expand] fn sample() {}",
        "#[cfg_attr(all(), opaque::expand)] fn sample() {}",
        "#[derive(UnknownDerive)] struct Sample;",
    ] {
        let error = scan_result(source).expect_err("unreviewed expansion path must fail closed");
        assert!(error.to_string().contains("unreviewed"), "unexpected error: {error:#}");
    }
    assert!(scan_result("#[tokio::test] fn sample() { serde_json::json!({}); }").is_ok());
}

#[test]
fn rejects_aliases_and_modules_that_impersonate_reviewed_expansions() {
    for source in [
        "use evil::info; fn sample() { info!(\"hidden\"); }",
        "use evil::safe as info; fn sample() { info!(\"hidden\"); }",
        "pub mod bridge { pub use evil::expand as harmless; } use crate::bridge::harmless as json; json!();",
        "mod bridge { pub use evil::expand as harmless; use self::harmless as json; json!(); }",
        "mod bridge { pub use evil::expand as harmless; mod nested { use super::harmless as json; json!(); } }",
        "pub mod bridge { pub use evil::expand as harmless; } use crate::bridge::{harmless as transport_test}; transport_test!();",
        "pub mod bridge { pub use evil::expand as harmless; } use crate::bridge::harmless as Deserialize;",
        "pub mod bridge { pub use evil::expand as harmless; } use crate::bridge::harmless as test;",
        "use evil as tokio; #[tokio::test] async fn sample() {}",
        "mod tokio { pub use evil::test; } #[tokio::test] async fn sample() {}",
        "mod r#tokio { pub use opaque::join; } r#tokio::join!();",
        "use evil::*; fn sample() { info!(\"hidden\"); }",
        "extern crate evil as tokio; #[tokio::test] async fn sample() {}",
        "use opaque::transport_test; transport_test!(noop, sample, |harness| async move {});",
        "use opaque::write; fn sample() { write!(buffer, \"hidden\"); }",
        "use opaque::Clone; #[derive(Clone)] struct Sample;",
        "use opaque::Error; #[derive(Error)] struct Sample;",
        "macro_rules! write { ($($token:tt)*) => {} } fn sample() { write!(buffer); }",
    ] {
        let error = scan_result(source).expect_err("expansion identity impersonation must fail closed");
        assert!(
            error.to_string().contains("shadow")
                || error.to_string().contains("reviewed expansion")
                || error.to_string().contains("glob import")
                || error.to_string().contains("extern crate")
                || error.to_string().contains("impersonates")
                || error.to_string().contains("unreviewed"),
            "unexpected error: {error:#}"
        );
    }
    assert!(scan_result("use super::helpers::transport_test; transport_test!();").is_ok());
}

#[test]
fn rejects_token_pasting_in_attribute_input() {
    let error = scan_result("#[builder([< un safe >])] fn sample() {}").expect_err("attribute token pasting must fail closed");
    assert!(error.to_string().contains("opaque token-pasting attribute input"), "unexpected error: {error:#}");
}

#[test]
fn rejects_runnable_rust_doctests_but_allows_ignored_examples() {
    for source in [
        "/// ```\n/// fn runnable() {}\n/// ```\nfn sample() {}",
        "//! ```\n//! fn inner_runnable() {}\n//! ```\nfn sample() {}",
        "/// ```rust,no_run\n/// fn compiled() {}\n/// ```\nfn sample() {}",
        "/// ```{.rust}\n/// fn class_style_runnable() {}\n/// ```\nfn sample() {}",
        "/// ```{.text}\n/// fn class_style_is_still_rust() {}\n/// ```\nfn sample() {}",
        "/// ```{.rust .ignore}\n/// fn class_names_are_not_rustdoc_attributes() {}\n/// ```\nfn sample() {}",
        "/// ```standalone_crate\n/// fn standalone_runnable() {}\n/// ```\nfn sample() {}",
        "/// ```test_harness\n/// fn harness_runnable() {}\n/// ```\nfn sample() {}",
        "/// ```ignore-x86_64\n/// fn runnable_on_other_targets() {}\n/// ```\nfn sample() {}",
        "/// ```ignore,ignore-x86_64\n/// fn target_ignore_overrides_global_ignore() {}\n/// ```\nfn sample() {}",
        "/// > ```rust\n/// > fn quoted_runnable() {}\n/// > ```\nfn sample() {}",
        "/// > > ```rust\n/// > > fn nested_quoted_runnable() {}\n/// > > ```\nfn sample() {}",
        "///     fn indented() {}\nfn sample() {}",
        "/**\n * ```rust\n * fn block_doc() {}\n * ```\n */\nfn sample() {}",
        "/*!\n * ```rust\n * fn inner_block_doc() {}\n * ```\n */\nfn sample() {}",
    ] {
        let error = scan_result(source).expect_err("runnable doctest must fail closed");
        assert!(error.to_string().contains("runnable Rust doctests"), "unexpected error: {error:#}");
    }
    assert!(scan_result("/// ```ignore\n/// fn ignored() {}\n/// ```\nfn sample() {}").is_ok());
    for language in ["text", "json", "sh", "toml"] {
        assert!(scan_result(&format!("/// ```{language}\n/// unsafe is prose here\n/// ```\nfn sample() {{}}")).is_ok());
    }
    assert!(scan_result("/// ```custom,language-c\n/// int main(void) { return 0; }\n/// ```\nfn sample() {}").is_ok());
    assert!(scan_result("/// ```custom,{.language-c}\n/// int main(void) { return 0; }\n/// ```\nfn sample() {}").is_ok());
    assert!(scan_result("/// ```custom,rust\n/// fn custom_rust() {}\n/// ```\nfn sample() {}").is_ok());
    assert!(scan_result("/// ```custom,no_run\n/// fn custom_no_run() {}\n/// ```\nfn sample() {}").is_ok());
    assert!(scan_result("/// ```text\n///     indented prose\n/// ```\nfn sample() {}").is_ok());
    assert!(scan_result("/// ````text\n/// embedded Markdown:\n/// ```rust\n/// fn not_a_doctest() {}\n/// ```\n/// ````\nfn sample() {}").is_ok());
    assert!(
        scan_result(
            r####"
            const COOKED: &str = "fixture
            /// ```
            /// fn not_a_doctest() {}
            /// ```";
            const RAW: &str = r###"fixture
            /// ```
            /// fn also_not_a_doctest() {}
            /// ```"###;
            "####,
        )
        .is_ok()
    );
    let error = scan_result("#[doc = \"```\\nfn hidden() {}\\n```\"] fn sample() {}").expect_err("explicit doc attribute must fail closed");
    assert!(error.to_string().contains("explicit #[doc] attribute"), "unexpected error: {error:#}");
    let error = scan_result("#[r#doc = \"```\\nfn hidden() {}\\n```\"] fn sample() {}").expect_err("raw explicit doc attribute must fail closed");
    assert!(error.to_string().contains("explicit #[doc] attribute"), "unexpected error: {error:#}");
    let error = scan_result("#[cfg_attr(all(), doc = \"```\\nfn hidden() {}\\n```\")] fn sample() {}").expect_err("nested explicit doc attribute must fail closed");
    assert!(error.to_string().contains("unreviewed attribute path"), "unexpected error: {error:#}");
}

#[test]
fn rejects_doc_attributes_generated_by_macros() {
    for source in [
        r#"macro_rules! transport_test { () => { #[doc = "```\nunsafe { hidden() }\n```"] fn generated() {} } }"#,
        r#"macro_rules! transport_test { () => { #![doc = "```\nunsafe { hidden() }\n```"] } }"#,
        r#"macro_rules! transport_test { () => { #[r#doc = "```\nunsafe { hidden() }\n```"] fn generated() {} } }"#,
        r#"macro_rules! transport_test { () => { #![cfg_attr(any(), doc = "```\nunsafe { hidden() }\n```")] } }"#,
        r#"macro_rules! transport_test { () => { #[cfg_attr(doc, doc = "```\nunsafe { hidden() }\n```")] fn generated() {} } }"#,
        r#"macro_rules! transport_test { () => { #[cfg_attr(any(), cfg_attr(doc, r#doc = "```\nunsafe { hidden() }\n```"))] fn generated() {} } }"#,
    ] {
        let error = scan_result(source).expect_err("macro-generated documentation must fail closed");
        assert!(error.to_string().contains("generates a #[doc] attribute"), "unexpected error: {error:#}");
    }
    assert!(scan_result("macro_rules! transport_test { () => { #[cfg_attr(doc, inline)] fn generated() {} } }").is_ok());
}

#[test]
fn rejects_unreviewed_nested_macro_delegation() {
    for source in [
        "use evil::emit_docs; macro_rules! transport_test { () => { emit_docs!() }; } transport_test!();",
        "use evil::emit_docs; macro_rules! transport_test { ($body:block) => { fn generated() $body }; } transport_test!({ emit_docs!(); });",
        "macro_rules! transport_test { ($emit:ident) => { $emit!() }; } transport_test!(emit_docs);",
        "macro_rules! transport_test { () => { macro_rules! hidden { () => {} } }; } transport_test!();",
    ] {
        let error = scan_result(source).expect_err("nested macro delegation must fail closed");
        assert!(error.to_string().contains("unreviewed nested macro"), "unexpected error: {error:#}");
    }
    for source in [
        "macro_rules! numbered_placeholders { () => { concat!(stringify!(1), stringify!(2)) }; } numbered_placeholders!();",
        "macro_rules! numbered_placeholders { () => { $crate::concat_placeholders!(1) }; } numbered_placeholders!();",
    ] {
        assert!(scan_result(source).is_ok());
    }
    for source in [
        "macro_rules! numbered_placeholders { () => { $crate::unreviewed!() }; } numbered_placeholders!();",
        "macro_rules! numbered_placeholders { ($emit:ident) => { $emit!() }; } numbered_placeholders!(emit_docs);",
    ] {
        let error = scan_result(source).expect_err("dynamic or unreviewed hygienic macro path must fail closed");
        assert!(error.to_string().contains("unreviewed nested macro"), "unexpected error: {error:#}");
    }
}

#[test]
fn rejects_unreviewed_attributes_generated_by_macros() {
    for source in [
        "macro_rules! transport_test { () => { #[evil::emit_docs] fn generated() {} }; } transport_test!();",
        "macro_rules! transport_test { () => { #[derive(evil::Docs)] struct Generated; }; } transport_test!();",
        "macro_rules! transport_test { () => { #[cfg_attr(any(), evil::emit_docs)] fn generated() {} }; } transport_test!();",
    ] {
        let error = scan_result(source).expect_err("generated attribute paths must use the closed reviewed set");
        assert!(error.to_string().contains("generates an unreviewed attribute path"), "unexpected error: {error:#}");
    }
    assert!(scan_result("macro_rules! transport_test { () => { #[tokio::test] async fn generated() {} }; } transport_test!();").is_ok());
}

#[test]
fn rejects_name_bindings_generated_by_macros() {
    for source in [
        "macro_rules! transport_test { () => { use evil::expand as Deserialize; }; } transport_test!();",
        "macro_rules! transport_test { () => { mod serde { pub use evil::expand as Deserialize; } }; } transport_test!();",
        "macro_rules! transport_test { () => { extern crate evil as serde; }; } transport_test!();",
    ] {
        let error = scan_result(source).expect_err("generated name bindings must fail closed");
        assert!(
            error.to_string().contains("effect on reviewed expansion names cannot be audited"),
            "unexpected error: {error:#}"
        );
    }
    assert!(scan_result("macro_rules! transport_test { () => { fn generated() {} }; } transport_test!();").is_ok());
}

#[test]
fn rejects_macro_invocations_inside_unsafe_blocks() {
    assert_unsafe_macro_rejected(
        r"
        macro_rules! dereference { ($pointer:expr) => { *$pointer } }
        fn sample(pointer: *const u8) { unsafe { dereference!(pointer); } }
        ",
    );
}

#[test]
fn rejects_macro_invocations_inside_unsafe_extern_blocks() {
    let error = scan_result("unsafe extern \"C\" { transport_test!(); }").expect_err("macro expansion inside unsafe extern must fail closed");
    assert!(error.to_string().contains("invokes a macro inside an unsafe context"), "unexpected error: {error:#}");
}

#[test]
fn rejects_macro_invocations_inside_unsafe_function_bodies() {
    assert_unsafe_macro_rejected(
        r"
        macro_rules! dereference { ($pointer:expr) => { *$pointer } }
        #[allow(unsafe_code, unsafe_op_in_unsafe_fn)]
        unsafe fn read(pointer: *const u8) -> u8 { dereference!(pointer) }
        ",
    );
}

#[test]
fn rejects_macro_invocations_inside_unsafe_traits_and_impls() {
    for source in ["unsafe trait Boundary { transport_test!(); }", "unsafe impl Boundary for Value { transport_test!(); }"] {
        let error = scan_result(source).expect_err("macro expansion inside unsafe trait or impl must fail closed");
        assert!(error.to_string().contains("invokes a macro inside an unsafe context"), "unexpected error: {error:#}");
    }
}
