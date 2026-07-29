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
}

fn git(workspace: &std::path::Path, arguments: &[&str]) {
    let status = Command::new("git").current_dir(workspace).args(arguments).status().expect("run git");
    assert!(status.success());
}
