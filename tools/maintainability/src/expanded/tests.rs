use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::process::Command;

use tempfile::{TempDir, tempdir};

use crate::scan::{SiteKind, SourceRange, UnsafeSite, scan_workspace};

use super::dep_info::{is_audited_compiler_input, parse as parse_dep_info, parse_make_words};
use super::{
    AuditLane, AuditOutput, Diagnostic, audit_environment_override, compare_diagnostics, is_root_manifest, parse_cargo_output, subtract_diagnostics, verify_with_target_directory,
};

fn site(range: SourceRange) -> UnsafeSite {
    UnsafeSite {
        path: "src/lib.rs".to_owned(),
        item: "operation".to_owned(),
        kind: SiteKind::Block,
        occurrence: 0,
        fingerprint: "a".repeat(64),
        boundary_fingerprint: "b".repeat(64),
        source_range: range,
    }
}

fn diagnostic(line: usize, column: usize, message: &str) -> Diagnostic {
    Diagnostic {
        target: "localhold:lib:src/lib.rs".to_owned(),
        code: "unsafe_code".to_owned(),
        path: "src/lib.rs".to_owned(),
        line,
        column,
        end_line: line,
        end_column: column + 6,
        message: message.to_owned(),
    }
}

#[test]
fn compiler_diagnostics_require_one_exact_lexical_site() {
    let sites = [site(SourceRange {
        start_line: 4,
        start_column: 9,
        end_line: 4,
        end_column: 15,
    })];
    assert!(compare_diagnostics(&sites, &[diagnostic(4, 9, "usage of an unsafe block")]).is_ok());
    assert!(compare_diagnostics(&sites, &[diagnostic(5, 9, "generated unsafe")]).is_err());
    assert!(compare_diagnostics(&sites, &[diagnostic(4, 9, "first unsafe"), diagnostic(4, 10, "second unsafe")]).is_err());
    let duplicate = diagnostic(4, 9, "same unsafe");
    assert!(compare_diagnostics(&sites, &[duplicate.clone(), duplicate]).is_err());
    let mut unsafe_operation = diagnostic(4, 9, "raw pointer dereference");
    unsafe_operation.code = "unsafe_op_in_unsafe_fn".to_owned();
    assert!(compare_diagnostics(&sites, &[unsafe_operation]).is_err());
}

#[test]
fn harness_diagnostics_subtract_one_normal_compilation_without_deduplication() {
    let repeated = diagnostic(4, 9, "normal and unit unsafe");
    let test_only = diagnostic(8, 5, "test-only unsafe");
    assert_eq!(
        subtract_diagnostics(&[repeated.clone(), repeated.clone(), test_only.clone()], std::slice::from_ref(&repeated)),
        [repeated, test_only]
    );
}

#[test]
fn compiler_environment_rejects_cargo_aliases_and_override_channels() {
    for rejected in [
        "CARGO_ALIAS_CLIPPY",
        "cargo_alias_check",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTDOCFLAGS",
        "CARGO_ENCODED_RUSTDOCFLAGS",
        "CARGO_BUILD_TARGET",
        "CLIPPY_CONF_DIR",
        "RUSTDOC",
        "RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTDOC",
        "CARGO_BUILD_RUSTDOCFLAGS",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTDOCFLAGS",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
    ] {
        assert!(audit_environment_override(rejected.as_ref()), "{rejected}");
    }
    for accepted in ["CARGO_BUILD_JOBS", "CARGO_TERM_COLOR", "RUST_BACKTRACE"] {
        assert!(!audit_environment_override(accepted.as_ref()), "{accepted}");
    }
}

#[test]
fn cargo_manifest_identity_uses_canonical_paths_and_fails_closed() {
    let fixture = tempdir().expect("temporary manifest fixture");
    let manifest = fixture.path().join("Cargo.toml");
    let other = fixture.path().join("other/Cargo.toml");
    fs::create_dir(fixture.path().join("nested")).expect("equivalent-path segment");
    fs::create_dir(fixture.path().join("other")).expect("foreign manifest directory");
    fs::write(&manifest, "").expect("root manifest");
    fs::write(&other, "").expect("foreign manifest");
    let expected = fs::canonicalize(&manifest).expect("canonical root manifest");

    let equivalent = fixture.path().join("nested/../Cargo.toml");
    assert!(is_root_manifest(&expected, &serde_json::json!({ "manifest_path": equivalent })).expect("equivalent manifest"));
    assert!(!is_root_manifest(&expected, &serde_json::json!({ "manifest_path": other })).expect("foreign manifest"));
    assert!(!is_root_manifest(&expected, &serde_json::json!({})).expect("message without manifest"));
    assert!(is_root_manifest(&expected, &serde_json::json!({ "manifest_path": fixture.path().join("missing.toml") })).is_err());
}

#[test]
fn cargo_output_is_drained_after_the_first_parse_error() {
    let output = format!("not JSON\n{}\n", "x".repeat(128 * 1024));
    let mut reader = Cursor::new(output.as_bytes());
    let mut audit = AuditOutput::default();
    let error = parse_cargo_output(
        &mut reader,
        Path::new("."),
        Path::new("Cargo.toml"),
        &AuditLane {
            label: "fixture",
            cargo_args: &[],
            target_kinds: &["lib"],
        },
        &mut audit,
    )
    .expect_err("malformed Cargo output must fail");

    assert!(error.to_string().contains("parse Cargo JSON"), "unexpected error: {error:#}");
    assert_eq!(reader.position(), output.len() as u64);
}

#[test]
fn dep_info_parser_handles_continuations_escapes_and_windows_paths() {
    let parsed = parse_dep_info("target: src/lib.rs tests/data\\ file.sql C\\:\\work\\src\\main.rs \\\n         clippy.toml\n").expect("dep-info");
    assert_eq!(parsed, ["src/lib.rs", "tests/data file.sql", r"C:\work\src\main.rs", "clippy.toml"]);
    assert_eq!(parse_make_words(r"one\\two escaped\#hash trailing\"), [r"one\two", "escaped#hash", r"trailing\"]);
    assert_eq!(parse_make_words(r"\\?\C\:\work\src\main.rs"), [r"\\?\C:\work\src\main.rs"]);
    assert_eq!(parse_make_words(r"\\?\UNC\server\share\main.rs"), [r"\\?\UNC\server\share\main.rs"]);
}

#[test]
fn audited_compiler_inputs_are_a_closed_root_set() {
    for accepted in ["Cargo.toml", "clippy.toml", "src/lib.rs", "tests/fixture.json", "benches/query.rs", "examples/client.rs"] {
        assert!(is_audited_compiler_input(accepted.as_ref()), "{accepted}");
    }
    for rejected in ["Cargo.lock", "outside.rs", "build.rs", "target/generated.rs", ".cargo/config.toml"] {
        assert!(!is_audited_compiler_input(rejected.as_ref()), "{rejected}");
    }
}

#[test]
fn compiler_and_dep_info_audits_fail_closed_on_unmatched_inputs() {
    let fixture = procedural_macro_fixture();
    write_root_source(&fixture, "opaque::safe!();");
    let error = scan_fixture(&fixture).expect_err("unreviewed safe macro must still require explicit review");
    assert!(error.to_string().contains("unreviewed macro path"), "unexpected error: {error:#}");
    write_root_source(&fixture, "tokio::safe!();");
    verify_fixture(&fixture, &[]).expect("safe procedural macro emits no unsafe diagnostics");

    write_root_source(&fixture, "tokio::join!();");
    assert!(scan_fixture(&fixture).expect("lexically reviewed macro path").is_empty());
    verify_fixture(&fixture, &[]).expect("rustc suppresses safety lints for fully synthetic external procedural-macro tokens");

    write_root_source(
        &fixture,
        "unsafe fn operation() {}\npub fn generated() {\n    // SAFETY: focused fixture operation has no preconditions.\n    unsafe { operation() }\n}",
    );
    let sites = scan_fixture(&fixture).expect("direct unsafe inventory");
    assert_eq!(sites.len(), 2);
    verify_fixture(&fixture, &sites).expect("compiler diagnostics map to direct unsafe sites");
    let error = verify_fixture(&fixture, &[]).expect_err("unmatched compiler diagnostic must fail closed");
    assert!(error.to_string().contains("compiler-expanded unsafe is absent"), "unexpected error: {error:#}");

    write_root_source(&fixture, "unsafe fn generated(pointer: *const u8) -> u8 { *pointer }");
    let sites = scan_fixture(&fixture).expect("unsafe operation fixture inventory");
    let error = verify_fixture(&fixture, &sites).expect_err("unsafe operation in unsafe function must fail closed");
    assert!(error.to_string().contains("unsafe_op_in_unsafe_fn"), "unexpected error: {error:#}");

    for source in ["opaque::unsafe_generated!();", "#[opaque::inject_allowed] fn seed() {}"] {
        write_root_source(&fixture, source);
        let error = scan_fixture(&fixture).expect_err("unreviewed generated unsafe must fail lexically");
        assert!(error.to_string().contains("unreviewed"), "unexpected error: {error:#}");
    }

    fs::write(fixture.path().join("outside.rs"), "pub fn outside() {}\n").expect("outside source");
    write_root_source(&fixture, "tokio::include_outside!();");
    let error = verify_fixture(&fixture, &[]).expect_err("generated external include must fail closed");
    assert!(error.to_string().contains("unaudited input outside.rs"), "unexpected error: {error:#}");
}

#[test]
fn binary_unit_test_diagnostics_are_audited() {
    let fixture = procedural_macro_fixture();
    write_root_source(&fixture, "tokio::safe!();");
    fs::write(
        fixture.path().join("src/main.rs"),
        "
        fn main() {}
        #[cfg(test)]
        mod tests {
            #[test]
            fn test_only_unsafe() {
                // SAFETY: the empty fixture operation has no preconditions.
                unsafe {}
            }
        }
        ",
    )
    .expect("binary source");

    let error = verify_fixture(&fixture, &[]).expect_err("binary test-only unsafe must be audited");
    assert!(error.to_string().contains("compiler-expanded unsafe is absent"), "unexpected error: {error:#}");
}

#[test]
fn test_enabled_example_diagnostics_are_audited() {
    let fixture = procedural_macro_fixture();
    write_root_source(&fixture, "tokio::safe!();");
    fs::create_dir(fixture.path().join("examples")).expect("examples root");
    fs::write(
        fixture.path().join("examples/test_enabled.rs"),
        "
        fn main() {}
        #[cfg(test)]
        #[test]
        fn test_only_unsafe() {
            // SAFETY: the empty fixture operation has no preconditions.
            unsafe {}
        }
        ",
    )
    .expect("test-enabled example source");
    append_root_manifest(
        &fixture,
        "
        [[example]]
        name = \"test-enabled\"
        path = \"examples/test_enabled.rs\"
        test = true
        ",
    );

    let error = verify_fixture(&fixture, &[]).expect_err("test-enabled example unsafe must be audited");
    assert!(error.to_string().contains("compiler-expanded unsafe is absent"), "unexpected error: {error:#}");
}

#[test]
fn explicit_examples_outside_the_examples_directory_are_audited() {
    let fixture = procedural_macro_fixture();
    write_root_source(&fixture, "tokio::safe!();");
    fs::write(
        fixture.path().join("src/audited_example.rs"),
        "
        fn main() {
            // SAFETY: the empty fixture operation has no preconditions.
            unsafe {}
        }
        ",
    )
    .expect("explicit example source");
    append_root_manifest(
        &fixture,
        "
        [[example]]
        name = \"audited-example\"
        path = \"src/audited_example.rs\"
        ",
    );

    let error = verify_fixture(&fixture, &[]).expect_err("explicit example unsafe must be audited");
    assert!(error.to_string().contains("compiler-expanded unsafe is absent"), "unexpected error: {error:#}");
}

#[test]
fn benchmark_enabled_non_bench_target_diagnostics_are_audited() {
    let fixture = procedural_macro_fixture();
    write_root_source(&fixture, "tokio::safe!();");
    fs::write(
        fixture.path().join("src/benchmark_only.rs"),
        "
        fn main() {}
        #[cfg(test)]
        fn benchmark_harness_unsafe() {
            // SAFETY: the empty fixture operation has no preconditions.
            unsafe {}
        }
        ",
    )
    .expect("benchmark-only binary source");
    append_root_manifest(
        &fixture,
        "
        [[bin]]
        name = \"benchmark-only\"
        path = \"src/benchmark_only.rs\"
        test = false
        bench = true
        ",
    );

    let error = verify_fixture(&fixture, &[]).expect_err("benchmark-enabled binary unsafe must be audited");
    assert!(error.to_string().contains("compiler-expanded unsafe is absent"), "unexpected error: {error:#}");
}

fn verify_fixture(fixture: &TempDir, sites: &[UnsafeSite]) -> anyhow::Result<()> {
    let target_directory = fixture.path().join("target");
    let output = Command::new(env!("CARGO"))
        .current_dir(fixture.path())
        .env("CARGO_TARGET_DIR", &target_directory)
        .args(["metadata", "--format-version=1", "--no-deps", "--locked"])
        .output()
        .expect("fixture Cargo metadata");
    assert!(output.status.success(), "fixture Cargo metadata failed: {}", String::from_utf8_lossy(&output.stderr));
    verify_with_target_directory(fixture.path(), sites, &output.stdout, Some(&target_directory))
}

fn append_root_manifest(fixture: &TempDir, source: &str) {
    let path = fixture.path().join("Cargo.toml");
    let mut manifest = fs::read_to_string(&path).expect("read fixture manifest");
    manifest.push_str(source);
    fs::write(path, manifest).expect("extend fixture manifest");
}

fn scan_fixture(fixture: &TempDir) -> anyhow::Result<Vec<UnsafeSite>> {
    scan_workspace(fixture.path(), &["src", "tests", "benches"].map(str::to_owned))
}

fn write_root_source(fixture: &TempDir, source: &str) {
    fs::write(fixture.path().join("src/lib.rs"), format!("{source}\n")).expect("root source");
}

fn procedural_macro_fixture() -> TempDir {
    let fixture = tempdir().expect("temporary fixture");
    for directory in ["src", "tests", "benches", "opaque-macro/src"] {
        fs::create_dir_all(fixture.path().join(directory)).expect("fixture directory");
    }
    fs::write(
        fixture.path().join("Cargo.toml"),
        r#"
[package]
name = "expanded-audit-fixture"
version = "0.1.0"
edition = "2024"

[lib]
test = false
doctest = false
bench = false

[dependencies]
tokio = { package = "opaque-macro", path = "opaque-macro" }

[lints.rust]
unsafe_code = "deny"
"#,
    )
    .expect("root manifest");
    fs::write(fixture.path().join("clippy.toml"), "").expect("fixture Clippy configuration");
    fs::write(
        fixture.path().join("opaque-macro/Cargo.toml"),
        r#"
[package]
name = "opaque-macro"
version = "0.1.0"
edition = "2024"

[lib]
proc-macro = true
"#,
    )
    .expect("macro manifest");
    fs::write(
        fixture.path().join("opaque-macro/src/lib.rs"),
        r##"
use proc_macro::TokenStream;

#[proc_macro]
pub fn safe(_: TokenStream) -> TokenStream {
    "pub fn generated() {}".parse().expect("safe expansion")
}

#[proc_macro]
pub fn unsafe_generated(_: TokenStream) -> TokenStream {
    "pub unsafe fn generated() {}".parse().expect("unsafe expansion")
}

#[proc_macro]
pub fn join(_: TokenStream) -> TokenStream {
    "pub unsafe fn generated() {}".parse().expect("synthetic unsafe expansion")
}

#[proc_macro_attribute]
pub fn inject_allowed(_: TokenStream, _: TokenStream) -> TokenStream {
    "#[allow(unsafe_code, unsafe_op_in_unsafe_fn)] pub unsafe fn generated(pointer: *const u8) -> u8 { *pointer }"
        .parse()
        .expect("suppressed unsafe expansion")
}

#[proc_macro]
pub fn include_outside(_: TokenStream) -> TokenStream {
    r#"include!(concat!(env!("CARGO_MANIFEST_DIR"), "/outside.rs"));"#
        .parse()
        .expect("include expansion")
}
"##,
    )
    .expect("macro source");
    fs::write(fixture.path().join("src/lib.rs"), "opaque_macro::safe!();\n").expect("initial root source");
    let status = Command::new(env!("CARGO"))
        .current_dir(fixture.path())
        .args(["generate-lockfile", "--offline"])
        .status()
        .expect("generate fixture lockfile");
    assert!(status.success(), "fixture lockfile generation failed");
    fixture
}
