use std::fs;
use std::process::Command;

use super::reject_tooling_suppressions;

#[test]
fn maintainer_tooling_rejects_real_suppressions_but_ignores_fixture_text() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let source = workspace.path().join("tools/checker/src");
    fs::create_dir_all(&source).expect("tool source directory");
    fs::write(
        source.join("main.rs"),
        "const FIXTURE: &str = \"#[allow(clippy::panic)] fn generated() {}\";\nfn main() { let _ = FIXTURE; }\n",
    )
    .expect("tool source");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    reject_tooling_suppressions(workspace.path()).expect("attribute-like fixture text is not syntax");

    fs::write(source.join("main.rs"), "#[expect(clippy::too_many_lines, reason = \"checker shortcut\")]\nfn main() {}\n").expect("suppressed tool source");
    let error = reject_tooling_suppressions(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("must remain suppression-free"));

    fs::write(
        source.join("main.rs"),
        "macro_rules! generated { () => { mod hidden { #![expect(dead_code, reason = \"macro inner\")] fn unused() {} } }; }\ngenerated!();\nfn main() {}\n",
    )
    .expect("macro-carried inner suppression");
    let error = reject_tooling_suppressions(workspace.path()).unwrap_err();
    assert!(format!("{error:#}").contains("must remain suppression-free"));

    fs::write(source.join("checks.inc"), "#[allow(dead_code)] fn hidden() {}\n").expect("included source");
    fs::write(source.join("main.rs"), "include!(\"checks.inc\");\nfn main() {}\n").expect("source include");
    let error = reject_tooling_suppressions(workspace.path()).unwrap_err();
    assert!(format!("{error:#}").contains("include!"));

    fs::create_dir_all(source.join("tests")).expect("nested Rust source directory");
    fs::write(source.join("tests/checks.rs"), "fn check() {}\n").expect("nested Rust source");
    fs::write(source.join("main.rs"), "#[path = \"tests/checks.rs\"] mod checks;\nfn main() {}\n").expect("audited module path");
    reject_tooling_suppressions(workspace.path()).expect("normalized Rust module paths remain in the audited inventory");

    fs::write(source.join("main.rs"), "#[path = \"checks.inc\"] mod checks;\nfn main() {}\n").expect("module path override");
    let error = reject_tooling_suppressions(workspace.path()).unwrap_err();
    assert!(format!("{error:#}").contains("audited source tree"));

    fs::write(
        source.join("main.rs"),
        "macro_rules! generated { () => { #[path = \"checks.inc\"] mod checks; }; }\nfn main() {}\n",
    )
    .expect("macro-carried module path override");
    let error = reject_tooling_suppressions(workspace.path()).unwrap_err();
    assert!(format!("{error:#}").contains("macro-carried module path"));

    fs::write(source.join("main.rs"), "std::include!(\"checks.inc\");\nfn main() {}\n").expect("qualified source include");
    let error = reject_tooling_suppressions(workspace.path()).unwrap_err();
    assert!(format!("{error:#}").contains("include!"));
}

fn git(workspace: &std::path::Path, arguments: &[&str]) {
    let status = Command::new("git").current_dir(workspace).args(arguments).status().expect("run git");
    assert!(status.success());
}
