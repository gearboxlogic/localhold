use super::*;

#[test]
fn command_policy_rejects_relative_dispatch_after_directory_changes() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(workspace.path().join("payload.txt"), "printf '%s\\n' safe\n").expect("benign root payload");
    fs::write(workspace.path().join("quality/payload.txt"), "cargo clippy -- -A warnings\n").expect("weakening relocated payload");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for source in ["cd quality; bash payload.txt\n", "trap 'cd quality' DEBUG; bash payload.txt\n"] {
        fs::write(workspace.path().join("script/check.sh"), source).expect("directory-relative dispatch");
        git(workspace.path(), &["add", "."]);
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("opaque interpreter program"), "{source}: {error:#}");
    }

    fs::write(workspace.path().join("script/check.sh"), "printf '%s\\n' safe\n").expect("safe shell control");
    for source in [
        "trap { Set-Location quality; continue }; throw 'x'; bash payload.txt\n",
        "trap { if ($true) { Set-Location quality }; continue }; throw 'x'; bash payload.txt\n",
    ] {
        fs::write(workspace.path().join("script/check.ps1"), source).expect("PowerShell directory-changing trap");
        git(workspace.path(), &["add", "."]);
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("opaque interpreter program"), "{source}: {error:#}");
    }
}
