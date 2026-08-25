use std::fs;
use std::process::Command;

use super::*;
use crate::structure::suppression::policy::model::{ClippyConstraint, ClippySetting};

#[test]
fn cargo_allow_scan_covers_root_and_nested_manifests() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("tool")).expect("tool directory");
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[package]\nname='root'\nversion='0.1.0'\n[lints.clippy]\nunwrap_used={level='allow',priority=-2}\npanic='warn'\n",
    )
    .expect("root manifest");
    fs::write(
        workspace.path().join("tool/Cargo.toml"),
        "[package]\nname='tool'\nversion='0.1.0'\n[lints.rust]\nunsafe_code={level='allow'}\n",
    )
    .expect("tool manifest");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    assert_eq!(
        scan_cargo_allows(workspace.path()).expect("Cargo allowances"),
        BTreeSet::from([
            ("Cargo.toml".to_owned(), "clippy".to_owned(), "unwrap_used".to_owned(), -2,),
            ("tool/Cargo.toml".to_owned(), "rust".to_owned(), "unsafe_code".to_owned(), 0,),
        ])
    );
}

#[test]
fn cargo_allow_scan_resolves_workspace_lint_inheritance() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("member")).expect("workspace member");
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers=['member']\n\n[workspace.lints.rust]\nunsafe_code='forbid'\nwarnings='allow'\n",
    )
    .expect("workspace manifest");
    fs::write(
        workspace.path().join("member/Cargo.toml"),
        "[package]\nname='member'\nversion='0.1.0'\n\n[lints]\nworkspace=true\n",
    )
    .expect("member manifest");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    assert_eq!(
        scan_cargo_allows(workspace.path()).expect("inherited Cargo allowances"),
        BTreeSet::from([("member/Cargo.toml".to_owned(), "rust".to_owned(), "warnings".to_owned(), 0,)])
    );
}

#[test]
fn cargo_allow_scan_resolves_explicit_sibling_workspaces() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("tools/member")).expect("member directory");
    fs::create_dir_all(workspace.path().join("quality")).expect("workspace directory");
    fs::write(workspace.path().join("Cargo.toml"), "[workspace]\n[workspace.lints.rust]\nwarnings='deny'\n").expect("decoy ancestor workspace");
    fs::write(workspace.path().join("quality/Cargo.toml"), "[workspace]\n[workspace.lints.rust]\nwarnings='allow'\n").expect("explicit workspace");
    fs::write(
        workspace.path().join("tools/member/Cargo.toml"),
        "[package]\nname='member'\nversion='0.1.0'\nworkspace='../../quality'\n[lints]\nworkspace=true\n",
    )
    .expect("member manifest");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    assert_eq!(
        scan_cargo_allows(workspace.path()).expect("explicit workspace allowances"),
        BTreeSet::from([("tools/member/Cargo.toml".to_owned(), "rust".to_owned(), "warnings".to_owned(), 0,)])
    );
}

#[test]
fn cargo_allow_scan_rejects_explicit_workspaces_outside_the_repository() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("tools/member")).expect("member directory");
    fs::write(
        workspace.path().join("tools/member/Cargo.toml"),
        "[package]\nname='member'\nversion='0.1.0'\nworkspace='../../../quality'\n[lints]\nworkspace=true\n",
    )
    .expect("member manifest");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = scan_cargo_allows(workspace.path()).expect_err("escaping workspace path must fail closed");
    assert!(format!("{error:#}").contains("escapes the repository"), "{error:#}");
}

#[test]
fn cargo_enabled_lints_cannot_be_removed_or_weakened() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let manifest = workspace.path().join("Cargo.toml");
    let write_manifest = |panic: Option<(&str, i64)>| {
        let panic = panic.map_or_else(String::new, |(level, priority)| format!("panic={{level='{level}',priority={priority}}}\n"));
        fs::write(
            &manifest,
            format!("[package]\nname='root'\nversion='0.1.0'\n[lints.rust]\nunsafe_code='forbid'\n[lints.clippy]\n{panic}"),
        )
        .expect("Cargo manifest");
    };
    write_manifest(Some(("warn", 1)));
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    git(
        workspace.path(),
        &["-c", "user.name=LocalHold Test", "-c", "user.email=test@localhold.invalid", "commit", "-qm", "base"],
    );
    let output = Command::new("git")
        .current_dir(workspace.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("read base revision");
    assert!(output.status.success());
    let revision = String::from_utf8(output.stdout).expect("UTF-8 revision");
    let revision = revision.trim();

    write_manifest(None);
    assert!(compare_cargo_lint_levels_previous_revision(workspace.path(), revision).is_err());
    write_manifest(Some(("allow", 1)));
    assert!(compare_cargo_lint_levels_previous_revision(workspace.path(), revision).is_err());
    write_manifest(Some(("deny", 0)));
    assert!(compare_cargo_lint_levels_previous_revision(workspace.path(), revision).is_err());
    write_manifest(Some(("deny", 1)));
    compare_cargo_lint_levels_previous_revision(workspace.path(), revision).expect("stronger lint level with stable priority");
}

#[test]
fn cargo_workspace_lint_inheritance_cannot_be_removed() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("member")).expect("member directory");
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers=['member']\n[workspace.lints.clippy]\npanic='warn'\n",
    )
    .expect("workspace manifest");
    let member = workspace.path().join("member/Cargo.toml");
    fs::write(&member, "[package]\nname='member'\nversion='0.1.0'\n[lints]\nworkspace=true\n").expect("member manifest");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    git(
        workspace.path(),
        &["-c", "user.name=LocalHold Test", "-c", "user.email=test@localhold.invalid", "commit", "-qm", "base"],
    );
    let output = Command::new("git")
        .current_dir(workspace.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("read base revision");
    let revision = String::from_utf8(output.stdout).expect("UTF-8 revision");

    fs::write(&member, "[package]\nname='member'\nversion='0.1.0'\n").expect("weakened member manifest");
    assert!(compare_cargo_lint_levels_previous_revision(workspace.path(), revision.trim()).is_err());
}

#[test]
fn clippy_constraints_are_directional() {
    compare_clippy_value("threshold", &toml::Value::Integer(4), &ClippyConstraint::MaximumInteger { value: 5 }).expect("lower threshold");
    assert!(compare_clippy_value("threshold", &toml::Value::Integer(6), &ClippyConstraint::MaximumInteger { value: 5 },).is_err());
    compare_clippy_value(
        "idents",
        &toml::Value::Array(vec![toml::Value::String("MCP".to_owned())]),
        &ClippyConstraint::StringSubset {
            values: vec!["MCP".to_owned(), "SQLite".to_owned()],
        },
    )
    .expect("smaller allowlist");
}

#[test]
fn weakening_tokens_distinguish_rust_lint_flags_from_application_options() {
    assert!(weakening_token("cargo clippy -- -A warnings"));
    assert!(weakening_token("cargo rustc -- --cap-lints=allow"));
    assert!(weakening_token("cargo rustc -- \"--cap-\"lints allow"));
    assert!(weakening_token("cargo clippy -- --allow warnings"));
    assert!(weakening_token("cargo clippy -- -W warnings"));
    assert!(weakening_token("cargo clippy -- --warn warnings"));
    assert!(weakening_token("RUSTC_BOOTSTRAP=1 cargo clippy -- -D warnings -Zcrate-attr='allow(dead_code)'"));
    assert!(weakening_token("rustc -Z crate-attr=allow(dead_code) source.rs"));
    assert!(weakening_token("cargo --config build.rustflags='-A warnings' clippy"));
    assert!(weakening_token("cargo deny --config deny.toml; cargo --config build.rustflags='-A warnings' clippy"));
    assert!(weakening_token("cargo \\\n  --config build.rustflags='-A warnings' clippy"));
    assert!(weakening_token("cargo clippy -- -*"));
    assert!(weakening_token("cargo clippy -- -?warnings"));
    assert!(weakening_token("cargo clippy -- -[A]warnings"));
    assert!(weakening_token("cargo clippy -- @(-Awarnings|-Wwarnings)"));
    assert!(weakening_token("cargo -Z unstable-options -C ../other build"));
    assert!(weakening_token("cargo --change-directory=../other build"));
    assert!(!weakening_token("cargo rustc -- -C opt-level=3"));
    assert!(weakening_token("cargo-clippy clippy -- -A warnings"));
    assert!(weakening_token("cargo clippy -- -\"\"A warnings"));
    assert!(weakening_token("cargo clippy -- \"--al\"low warnings"));
    assert!(weakening_token("cargo clippy -- -\\A warnings"));
    assert!(weakening_token_for_surface("script/check.cmd", "CARGO.EXE clippy -- -A warnings"));
    assert!(weakening_token_for_surface("script/check.ps1", "Rustc.ExE -W warnings source.rs"));
    assert!(weakening_token_for_surface("script/check.sh", "CARGO.EXE clippy -- -A warnings"));
    assert!(weakening_token("rustc @policy/lints.args source.rs"));
    assert!(weakening_token_for_surface("package.json", r#"{"scripts":{"lint":"cargo clippy -- \u002dAwarnings"}}"#));
    assert!(!weakening_token_for_surface("package.json", r#"{"scripts":{"lint":"cargo clippy -- \u002dDwarnings"}}"#));
    assert!(weakening_token_for_surface("package.json", r#"{"scripts":{"lint":42}}"#));
    assert!(weakening_token("rustc -D warnings --allow=warnings source.rs"));
    assert!(weakening_token("rustc -D warnings --warn=warnings source.rs"));
    assert!(weakening_token("rustc -D warnings --force-warn=unused_variables source.rs"));
    assert!(weakening_token("LABEL=@not-a-response rustc @policy/lints.args source.rs"));
    assert!(weakening_token("run_cargo() { cargo \"$@\"; }\nrun_cargo clippy -- -A warnings"));
    assert!(weakening_token("TOOL=cargo\ncommand \"$TOOL\" clippy -- -A warnings"));
    assert!(weakening_token_for_surface("script/check.cmd", "set TOOL=cargo\n%TOOL% clippy -- -A warnings"));
    assert!(!weakening_token_for_surface("script/check.ps1", "$actual = (Get-FileHash -Algorithm SHA256 $path).Hash\n"));
    assert!(weakening_token("printf '%s\\n' clippy -- -A warnings | xargs cargo"));
    assert!(weakening_token("printf '%s\\n' check | xargs -- cargo"));
    assert!(weakening_token("cargo clippy -- -{A,A}warnings"));
    assert!(weakening_token("cargo clippy -- --{allow,warn}=warnings"));
    assert!(weakening_token("cargo clippy -- -{A..Z}warnings"));
    assert!(!weakening_token("cargo nextest run {{ ARGS }}"));
    assert!(!weakening_token("cargo build | grep -A 2"));
    assert!(!weakening_token("printf check | xargs echo | cargo build"));
    assert!(!weakening_token("CARGO=$trusted_cargo"));
    assert!(!weakening_token("RUSTC=$trusted_rustc"));
    assert!(weakening_token("CARGO=$(cargo clippy -- -A warnings)"));
    assert!(weakening_token("LINT_FLAGS='-A warnings'\ncargo clippy -- $LINT_FLAGS"));
    assert!(weakening_token("tool=cargo; flag=-A; $tool clippy -- $flag warnings"));
    assert!(weakening_token("tool=cargo; sub=clippy; flag=-A; $tool $sub -- $flag warnings"));
    assert!(weakening_token("cargo clippy -- `printf '%s' \"$LINT_LEVEL\"` warnings"));
    assert!(weakening_token("cargo rustc -- @policy/lints.args"));
    assert!(!weakening_token("cargo run -- @application-argument"));
    assert!(weakening_token("sh -c \"cargo --config net.offline=true check # literal\""));
    assert!(!weakening_token("cargo deny --config deny.toml"));
    assert!(weakening_token(
        "echo \"$(cargo deny --version) $(cargo --config 'build.rustflags=[\\\"-A\\\",\\\"warnings\\\"]' clippy)\""
    ));
    assert!(weakening_token(
        "echo \"$(cargo deny --version) $(cargo-clippy clippy --config 'build.rustflags=[\\\"-A\\\",\\\"warnings\\\"]')\""
    ));
    assert!(!weakening_token("gitleaks --config policy.toml # cargo output"));
    assert!(!weakening_token("cargo build\ncc -Wall -Wextra"));
    assert!(!weakening_token("hold doctor --allow-downloads"));
}

#[test]
fn weakening_tokens_reject_cmd_delayed_expansion() {
    assert!(weakening_token_for_surface(
        "script/check.cmd",
        "setlocal EnableDelayedExpansion\nset FLAG=-A\ncargo clippy -- !FLAG! warnings"
    ));
    assert!(weakening_token_for_surface(
        "script/check.cmd",
        "setlocal EnableDelayedExpansion\nset TOOL=cargo\n!TOOL! clippy -- -A warnings"
    ));
}

#[test]
fn shell_continuations_cannot_split_lint_arguments_from_cargo() {
    assert!(weakening_token_for_surface("script/check.ps1", "cargo clippy -- `\r\n  -A warnings"));
    assert!(weakening_token_for_surface("script/check.ps1", "cargo clippy -- `\n  --allow=warnings"));
    assert!(weakening_token_for_surface("script/check.ps1", "cargo clippy -- --a`llow warnings"));
    assert!(weakening_token_for_surface("script/check.ps1", "ca`rgo clippy -- --allow warnings"));
    assert!(weakening_token_for_surface("script/check.ps1", "cargo clippy -- \"--a`llow\" warnings"));
    assert!(weakening_token_for_surface(
        "script/check.ps1",
        "# don't change quote state\ncargo clippy -- --a`llow warnings"
    ));
    assert!(weakening_token_for_surface(
        "script/check.ps1",
        "Write-Output \"don't change quote state\"\ncargo clippy -- --a`llow warnings"
    ));
    assert!(weakening_token_for_surface(
        "script/check.ps1",
        "<# don't change quote state #>\ncargo clippy -- --a`llow warnings"
    ));
    assert!(!weakening_token_for_surface("script/check.ps1", "Write-Output \"build``stamp\"\ncargo build"));
    assert!(!weakening_token_for_surface("script/check.ps1", "Write-Output 'cargo clippy -- --a`llow warnings'"));
    assert!(weakening_token_for_surface("script/check.cmd", "cargo clippy -- ^\r\n  -A warnings"));
    assert!(weakening_token_for_surface("script/check.bat", "cargo clippy -- ^\n  --allow=warnings"));
    assert!(weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - shell: cmd\n    run: |\n      cargo clippy -- ^\n        -A warnings\n"
    ));
    assert!(weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - shell: pwsh\n    run: cargo clippy -- ('-' + 'A') warnings\n"
    ));
    assert!(weakening_token_for_surface(
        "script/check.ps1",
        "$lintArgs = @('-' + 'A', 'warnings')\ncargo clippy -- @lintArgs\n"
    ));
    assert!(weakening_token_for_surface("script/check.ps1", "cargo clippy -- ('-' + 'A') warnings"));
}

#[test]
fn inert_ansi_c_data_cannot_recurse_as_a_rust_command() {
    assert!(!weakening_token(r#"write_manifest $'[package]\nname = "checker"'"#));
    assert!(weakening_token(r"$'cargo clippy -- -A warnings'"));
}

#[test]
fn shell_substitution_syntax_is_not_applied_to_other_command_languages() {
    assert!(!weakening_token_for_surface("script/check.py", r#"print("$(just check-quality)")"#));
    assert!(!weakening_token_for_surface(
        "script/check.py",
        "CARGO_MANIFEST_PATH = REPO_ROOT / \"Cargo.toml\"\n\
         def git_bytes(reference, source):\n\
             return subprocess.run(\n\
                 [\"git\", \"show\", f\"{reference}:{source}\"],\n\
                 check=False,\n\
             )\n"
    ));
    assert!(!weakening_token_for_surface("script/check.cmd", "echo $(just check-quality)"));
    assert!(!weakening_token_for_surface("script/check.ps1", "Write-Output \"build``stamp\""));
    assert!(weakening_token_for_surface("script/check.ps1", "Write-Output \"$(just check-quality)\""));
}

#[test]
fn python_command_arrays_cannot_split_lint_arguments_from_cargo() {
    assert!(weakening_token_for_surface("script/check.py", "exec(bytes.fromhex(\"696d706f7274206f73\"))\n"));
    assert!(weakening_token_for_surface("script/check.py", "import pickle\npickle.loads(bytes.fromhex(payload))\n"));
    assert!(weakening_token_for_surface(
        "script/check.py",
        "subprocess.run([\n    \"cargo\", # tool\n    \"clippy\",\n    \"--\",\n    \"-A\",\n    \"warnings\",\n])\n"
    ));
    assert!(weakening_token_for_surface(
        "script/check.py",
        "subprocess.run([\"cargo\", \"clippy\", \"--\", \"-\" \"A\", \"warnings\"])\n"
    ));
    assert!(weakening_token_for_surface(
        "script/check.py",
        "subprocess.run([\"cargo\", \"clippy\", \"--\", chr(45) + \"A\", \"warnings\"])\n"
    ));
    assert!(weakening_token_for_surface(
        "script/check.py",
        "os.execlp(\"cargo\", \"cargo\", \"clippy\", \"--\", \"-\" + \"A\", \"warnings\")\n"
    ));
    assert!(weakening_token_for_surface(
        "script/check.py",
        "runner = __import__(\"sub\" + \"process\")\nrunner.run([\"cargo\", \"clippy\", \"--\", chr(45) + \"A\", \"warnings\"])\n"
    ));
    assert!(weakening_token_for_surface(
        "script/check.py",
        "import ctypes\nctypes.CDLL(None).system(bytes.fromhex(\"636172676f20636c69707079202d2d202d41207761726e696e6773\"))\n"
    ));
    assert!(weakening_token_for_surface(
        "script/check.py",
        "import os\nos.system(bytes.fromhex(\"636172676f20636c69707079202d2d202d41207761726e696e6773\"))\n"
    ));
}

#[test]
fn unanalyzed_dynamic_programs_fail_closed() {
    for (path, source) in [
        ("quality/lint.pl", "system(\"cargo\", \"clippy\", \"--\", \"-\" . \"A\", \"warnings\")\n"),
        ("quality/lint.rb", "system(\"cargo\", \"clippy\", \"--\", \"-\" + \"A\", \"warnings\")\n"),
        ("quality/lint.lua", "os.execute(\"cargo clippy -- -\" .. \"A warnings\")\n"),
        ("quality/lint.php", "exec(\"cargo clippy -- -\" . \"A warnings\");\n"),
        ("quality/lint.tcl", "exec cargo clippy -- [format %cA 45] warnings\n"),
    ] {
        assert!(weakening_token_for_surface(path, source), "dynamic program was not rejected: {path}");
    }
}

#[test]
fn rust_commands_reject_opaque_manifest_paths_before_inventory_validation() {
    assert!(!weakening_token("cargo clippy --manifest-path tools/checker/Cargo.toml -- -D warnings"));
    assert!(!weakening_token("cargo clippy --manifest-path quality/checker/Cargo.toml -- -D warnings"));
    assert!(weakening_token("cargo clippy --manifest-path ../checker/Cargo.toml -- -D warnings"));
    assert!(weakening_token("cargo clippy --manifest-path"));
}

#[test]
fn command_policy_requires_manifest_membership_in_the_audited_inventory() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("tools/checker/target/generated")).expect("generated manifest directory");
    fs::write(workspace.path().join(".gitignore"), "tools/checker/target/\n").expect("ignore generated target");
    fs::write(workspace.path().join("tools/checker/Cargo.toml"), "[package]\nname='checker'\nversion='0.1.0'\n").expect("audited manifest");
    fs::write(
        workspace.path().join("tools/checker/target/generated/Cargo.toml"),
        "[package]\nname='generated'\nversion='0.1.0'\n",
    )
    .expect("ignored generated manifest");
    fs::write(workspace.path().join("script/check.sh"), "cargo test --manifest-path tools/checker/Cargo.toml\n").expect("standalone manifest execution command");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.sh"),
        "cargo metadata --manifest-path tools/checker/Cargo.toml --no-deps\n",
    )
    .expect("audited manifest metadata command");
    reject_checked_in_weakening(workspace.path()).expect("audited manifest selection");

    fs::write(
        workspace.path().join("script/check.sh"),
        "cargo metadata --manifest-path=tools/checker/target/generated/Cargo.toml --no-deps\n",
    )
    .expect("ignored manifest command");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("audited manifest inventory"), "{error:#}");
}

#[test]
fn folded_yaml_commands_are_scanned_as_executed() {
    assert!(weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - run: >-\n      cargo clippy --\n      -A warnings\n"
    ));
    assert!(weakening_token_for_surface(
        ".github/actions/check/action.yaml",
        "runs:\n  steps:\n    - 'run': > # folded command\n        cargo clippy --\n        --allow warnings\n"
    ));
    assert!(!weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - run: >\n      cargo build\n\n      echo application -A argument\n"
    ));
    assert!(weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "jobs:\n  windows:\n    runs-on: windows-latest\n    steps:\n      - run: CARGO.EXE clippy -- -A warnings\n"
    ));
}

#[test]
fn weakening_environment_channels_are_detected() {
    assert!(weakening_environment("BASH_ENV=script/ci-startup.sh"));
    assert!(weakening_environment("ENV=quality/dash-startup.sh dash -i quality/reviewed.sh"));
    assert!(weakening_environment("GCONV_PATH=quality iconv -f UTF-8 -t PWN"));
    assert!(weakening_environment("OPENSSL_CONF=quality/openssl.cnf openssl version"));
    assert!(weakening_environment("OPENSSL_MODULES=quality openssl version"));
    assert!(weakening_environment("RIPGREP_CONFIG_PATH=quality/ripgrep.conf rg lint ."));
    assert!(!weakening_environment_for_surface("script/check.py", "subprocess.run(command, env=environment)"));
    assert!(weakening_environment_for_surface("script/check.ps1", "$env:env = 'quality/dash-startup.sh'"));
    assert!(weakening_environment("LD_AUDIT=untrusted.so"));
    assert!(weakening_environment("LD_LIBRARY_PATH=untrusted"));
    assert!(weakening_environment("LD_PRELOAD=untrusted.so"));
    assert!(weakening_environment("PYTHONPATH=/tmp/injected python3 quality/safe.py"));
    assert!(weakening_environment("export RUSTFLAGS='-A warnings'\nexec \"$CHECK\""));
    assert!(weakening_environment("export RUST''FLAGS='--cap-lints allow'"));
    assert!(weakening_environment("CARGO_ENCODED_RUSTFLAGS=dynamic"));
    assert!(weakening_environment("RUSTDOCFLAGS=--cap-lints=allow"));
    assert!(weakening_environment("CARGO_ENCODED_RUSTDOCFLAGS=dynamic"));
    assert!(weakening_environment("CARGO_BUILD_RUSTDOCFLAGS=--cap-lints=allow"));
    assert!(weakening_environment("CARGO_TARGET_TEST_RUSTDOCFLAGS=--cap-lints=allow"));
    assert!(weakening_environment("CLIPPY_ARGS='--allow warnings'"));
    assert!(weakening_environment("CLIPPY_CONF_DIR=unreviewed"));
    assert!(weakening_environment("RUSTC_WRAPPER=unreviewed"));
    assert!(weakening_environment("RUSTC_BOOTSTRAP=1"));
    assert!(weakening_environment("GIT_DIR=untrusted"));
    assert!(weakening_environment("CARGO_TARGET_TEST_RUSTFLAGS=unreviewed"));
    assert!(weakening_environment("CARGO_HOME=unreviewed"));
    assert!(weakening_environment_for_surface("script/check.sh", "CARGO=/tmp/fake cargo-clippy clippy -- -D warnings"));
    assert!(weakening_environment("unset GITHUB_ACTIONS"));
    assert!(weakening_environment("unset GITHUB_EVENT_PATH"));
    assert!(weakening_environment("GITHUB_PATH=untrusted"));
    assert!(weakening_environment("GITHUB_SHA=untrusted"));
    assert!(weakening_environment("LOCALHOLD_MAINTAINABILITY_BASE_REV=$GITHUB_SHA"));
    assert!(weakening_environment("MISE_OVERRIDE_CONFIG_FILENAMES=policy.toml"));
    assert!(weakening_environment("MAKEFILES=quality/lint.rules"));
    assert!(weakening_environment_for_surface("script/check.ps1", "$env:rustflags = $dynamic"));
    assert!(weakening_environment_for_surface("script/check.cmd", "set cargo_encoded_rustflags=%DYNAMIC%"));
    assert!(weakening_environment_for_surface(
        "script/check.sh",
        "rustflags='-A warnings'\nexport rustflags\ncargo clippy"
    ));
    assert!(!weakening_environment("rustc --version"));
}

#[test]
fn reviewed_environment_scrubbers_are_exact() {
    let scrubber = format!("{}\n", BOOTSTRAP_ENVIRONMENT_LINES.join("\n"));
    assert!(scrubber_environment_references_are_exact("script/check-maintainability-bootstrap.sh", &scrubber));
    assert!(scrubber_environment_references_are_exact(
        "script/run-maintainability-gate.sh",
        &GATE_RUNNER_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact(
        "script/run-source-safety.sh",
        &RUNNER_ENVIRONMENT_LINES.join("\n")
    ));
    assert!(scrubber_environment_references_are_exact("script/install.sh", &INSTALL_ENVIRONMENT_LINES.join("\n")));
    assert!(!scrubber_environment_references_are_exact(
        "script/check-maintainability-bootstrap.sh",
        &format!("{scrubber}RUSTFLAGS='-A warnings'\n"),
    ));
    assert!(!scrubber_environment_references_are_exact(
        "script/check-maintainability-bootstrap.sh",
        &format!("{scrubber}PATH=/tmp\n"),
    ));
    assert!(!scrubber_environment_references_are_exact(
        "script/check-maintainability-bootstrap.sh",
        &BOOTSTRAP_ENVIRONMENT_LINES[1..].join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact(
        "script/tests/test_maintainability_bootstrap.sh",
        &BOOTSTRAP_TEST_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact(
        "mise.toml",
        &format!("[env]\n{}\n", MISE_ENVIRONMENT_LINES.join("\n")),
    ));
    assert!(scrubber_environment_references_are_exact(
        ".github/workflows/ci.yml",
        &CI_TRUST_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact(
        ".github/workflows/trusted-maintainability.yml",
        &TRUSTED_GATE_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(!scrubber_environment_references_are_exact(
        ".github/workflows/ci.yml",
        &CI_TRUST_ENVIRONMENT_LINES[..1].join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact(
        ".github/workflows/gpu-release-gate.yml",
        &GPU_RELEASE_REVISION_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact(
        "script/claude-review.sh",
        &CLAUDE_REVIEW_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(scrubber_environment_references_are_exact(
        "script/tests/test_claude_review.sh",
        &CLAUDE_REVIEW_TEST_ENVIRONMENT_LINES.join("\n"),
    ));
    assert!(!scrubber_environment_references_are_exact("script/tests/new-command.sh", &scrubber));
}

#[test]
fn checked_in_bootstrap_matches_its_reviewed_environment_contract() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = "script/check-maintainability-bootstrap.sh";
    let source = fs::read_to_string(repository.join(path)).expect("read checked-in maintainability bootstrap");
    if super::command::checked_in_legacy_transition_capabilities(&repository, path, &source).is_some_and(|(_, environment)| environment) {
        assert!(
            !scrubber_environment_references_are_exact(path, &source),
            "legacy bootstrap bridge no longer needs its environment exception"
        );
        return;
    }

    assert!(scrubber_environment_references_are_exact(path, &source));
}

#[test]
fn trusted_gate_environment_allowance_is_closed() {
    let reviewed = TRUSTED_GATE_ENVIRONMENT_LINES.join("\n");
    for changed in [
        reviewed.replacen("RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup", "RUSTUP_HOME: quality/rustup", 1),
        reviewed.replacen("RUSTUP_TOOLCHAIN: 1.97.0", "RUSTUP_TOOLCHAIN: fake", 1),
        format!("{reviewed}\n          RUSTUP_HOME: quality/rustup"),
    ] {
        assert!(!scrubber_environment_references_are_exact(".github/workflows/trusted-maintainability.yml", &changed,));
    }
}

#[test]
fn rustup_mirror_overrides_are_governed_environment_channels() {
    assert!(weakening_environment("RUSTUP_DIST_SERVER=https://example.invalid"));
    assert!(weakening_environment("RUSTUP_UPDATE_ROOT=https://example.invalid"));
    assert!(weakening_environment(
        "RUSTUP_HOME=quality/rustup RUSTUP_TOOLCHAIN=fake cargo clippy --locked -- -D warnings"
    ));
    assert!(weakening_environment_for_surface(
        ".github/workflows/ci.yml",
        "jobs:\n  dependency-unsafe-linux:\n    env:\n      RUSTUP_DIST_SERVER: https://example.invalid\n"
    ));
}

#[test]
fn archive_tool_environment_overrides_are_governed() {
    assert!(weakening_environment("TAR_OPTIONS=--checkpoint-action=exec=quality/helper"));
    assert!(weakening_environment_for_surface("script/check.sh", "ZIP='-T -TTsh quality/helper'"));
    assert!(weakening_environment("ZIPOPT='-T -TTsh quality/helper'"));
    assert!(!weakening_environment("document the ZIP archive format"));
}

#[test]
fn compiler_driver_environment_overrides_are_governed() {
    assert!(weakening_environment("CCC_OVERRIDE_OPTIONS='+-Xclang +-load +-Xclang +quality/payload'"));
    assert!(weakening_environment_for_surface(
        "script/check.sh",
        "CL='/clang:-Xclang /clang:-load /clang:quality/payload'"
    ));
    assert!(weakening_environment("_CL_='/clang:-Xclang /clang:-load /clang:quality/payload'"));
    assert!(!weakening_environment("document the CL compiler mode"));
}

#[test]
fn bootstrap_digest_overrides_require_the_exact_reviewed_bindings() {
    for name in ["LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256", "LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256"] {
        assert!(weakening_environment(&format!("{name}=attacker-controlled")));
    }
    let reviewed = CI_TRUST_ENVIRONMENT_LINES.join("\n");
    assert!(!scrubber_environment_references_are_exact(
        ".github/workflows/ci.yml",
        &format!("{reviewed}\n          LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256: ${{{{ github.sha }}}}\n"),
    ));
    assert!(!scrubber_environment_references_are_exact(
        ".github/workflows/ci.yml",
        &format!("{reviewed}\n          LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256: attacker-controlled\n"),
    ));
}

#[test]
fn quality_command_exceptions_require_the_exact_reviewed_lines() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in [
        "script/run-maintainability-gate.sh",
        "script/run-source-safety.sh",
        "script/install.sh",
        "script/dep-audit.sh",
        ".github/workflows/trusted-maintainability.yml",
    ] {
        let source = fs::read_to_string(repository.join(path)).expect("read reviewed quality-command source");
        if super::command::checked_in_legacy_transition_capabilities(&repository, path, &source).is_some_and(|(opaque, _)| opaque) {
            assert!(
                !super::command::reviewed_quality_command_exceptions_are_exact(path, &source, true),
                "legacy bridge no longer needs its quality-command exception: {path}"
            );
            continue;
        }
        assert!(super::command::reviewed_quality_command_exceptions_are_exact(path, &source, true), "{path}");
    }
    assert!(!super::command::reviewed_quality_command_exceptions_are_exact(
        "script/run-source-safety.sh",
        &format!("{}\n\"$cargo_command\" clippy -- -A warnings", RUNNER_COMMAND_LINES.join("\n")),
        false,
    ));
    assert!(!super::command::reviewed_quality_command_exceptions_are_exact(
        "script/run-source-safety.sh",
        &format!("{}\n\"$cargo_command\" clippy -- \\\n+            -A warnings", RUNNER_COMMAND_LINES.join("\n")),
        false,
    ));
    assert!(!super::command::reviewed_quality_command_exceptions_are_exact(
        "script/run-source-safety.sh",
        &format!("{}\ngate() {{\n    cargo test\n    true\n}}\ngate || true", RUNNER_COMMAND_LINES.join("\n")),
        false,
    ));

    let source = fs::read_to_string(repository.join("script/dep-audit.sh")).expect("read dependency audit script");
    if super::command::checked_in_legacy_transition_capabilities(&repository, "script/dep-audit.sh", &source).is_some_and(|(opaque, _)| opaque) {
        assert!(!weakening_token_for_surface("script/dep-audit.sh", &source));
        assert!(!super::command::reviewed_quality_command_exceptions_are_exact("script/dep-audit.sh", &source, true));
    } else {
        assert!(weakening_token_for_surface("script/dep-audit.sh", &source));
        assert!(super::command::reviewed_quality_command_exceptions_are_exact("script/dep-audit.sh", &source, true));
        assert!(!super::command::reviewed_quality_command_exceptions_are_exact(
            "script/dep-audit.sh",
            &source.replace("if ! run_workspace_deny; then", "if ! run_unreviewed_deny; then"),
            false,
        ));
    }
    assert!(!super::command::reviewed_quality_command_exceptions_are_exact(
        "script/dep-audit.sh",
        &source.replace("if (( failed != 0 )); then", "failed=0\nif (( failed != 0 )); then"),
        false,
    ));
}

#[test]
fn checked_in_installer_preserves_its_reviewed_build_directory_contract() {
    let installer = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../script/install.sh");
    let source = fs::read_to_string(installer).expect("read checked-in installer");

    assert!(weakening_environment_for_surface("script/install.sh", &source));
    assert!(scrubber_environment_references_are_exact("script/install.sh", &source));
    assert!(!weakening_token_for_surface("script/install.sh", &source));
    assert!(super::command::reviewed_quality_command_exceptions_are_exact("script/install.sh", &source, true));
    assert!(source.contains("case \":${PATH}:\" in\n  *\":${prefix}/bin:\"*) ;;\n  *) printf 'Add %s/bin to PATH before invoking hold by name.\\n' \"$prefix\" ;;\nesac"));
    let reviewed = without_reviewed_dispatch("script/install.sh", &source, true);
    assert!(!reviewed.contains("\"$cargo_command\" build"));

    let tampered = source.replace("--features reranker --target-dir", "--features reranker --quiet --target-dir");
    assert_eq!(without_reviewed_dispatch("script/install.sh", &tampered, false), tampered);
}

#[test]
fn executable_path_changes_are_governed_on_every_command_surface() {
    assert!(weakening_environment_for_surface("script/check.sh", "PATH=/tmp cargo clippy"));
    assert!(weakening_environment_for_surface("script/check.sh", "TOKEN=value PATH=/tmp cargo clippy"));
    assert!(weakening_environment_for_surface("package.json", r#"{"scripts":{"lint":"P\u0041TH=/tmp cargo clippy"}}"#));
    assert!(weakening_environment_for_surface("script/check.ps1", "$env:Path = 'C:\\untrusted'; cargo clippy"));
    assert!(!weakening_environment_for_surface("script/check.sh", "path=/tmp cargo clippy"));
    assert!(!weakening_environment_for_surface("script/check.ps1", "$path = Join-Path release artifact.zip"));
    assert!(weakening_environment_for_surface("script/check.sh", "PATH=/tmp node application.js"));
    assert!(weakening_environment_for_surface("mise.toml", "[env]\n_.path = ['quality/bin']\n"));
    assert!(weakening_environment_for_surface("mise.toml", "[env]\n_.file = 'quality/environment'\n"));
    assert!(!scrubber_environment_references_are_exact("mise.toml", "[env]\n_.path = ['quality/bin']\n"));
    assert!(!scrubber_environment_references_are_exact(
        "mise.toml",
        &format!("[env]\n{}\n_.file = 'quality/environment'\n", MISE_ENVIRONMENT_LINES.join("\n"))
    ));
}

#[test]
fn yaml_environment_channels_are_distinguished_from_action_inputs() {
    assert!(weakening_environment_for_surface(
        ".github/workflows/ci.yml",
        "jobs:\n  lint:\n    env:\n      Path: /tmp\n    steps:\n      - run: cargo clippy\n"
    ));
    assert!(weakening_environment_for_surface(
        ".github/workflows/ci.yml",
        "env:\n  rustflags: -A warnings\njobs:\n  lint:\n    steps:\n      - run: cargo clippy\n"
    ));
    assert!(weakening_environment_for_surface(
        ".github/workflows/ci.yml",
        "jobs:\n  lint:\n    env:\n      BASH_FUNC_just%%: '() { :; }'\n    steps:\n      - run: just check-quality\n"
    ));
    assert!(!weakening_environment_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - uses: actions/cache@example\n    with:\n      path: target\n"
    ));
    assert!(weakening_environment_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - shell: bash\n    run: printf '%s\\n' \"UkVTVENfV1JBUFBFUj0vdG1wL2ZpbHRlcg==\" >> \"$GITHUB_ENV\"\n  - run: just check-quality\n"
    ));
}

#[test]
fn powershell_quality_steps_enforce_native_exit_status() {
    assert!(weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - shell: pwsh\n    run: |\n      cargo clippy --locked -- -D warnings\n      exit 0\n"
    ));
    assert!(!weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - run: |\n      cargo clippy --locked -- -D warnings\n      if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n    shell: powershell\n"
    ));
    assert!(!weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - shell: bash\n    run: |\n      cargo clippy --locked -- -D warnings\n      exit 0\n"
    ));
    assert!(!weakening_token_for_surface(
        ".github/workflows/ci.yml",
        "steps:\n  - shell: pwsh\n    run: Write-Output 'cargo clippy -- --a`llow warnings'\n"
    ));
    assert!(weakening_token_for_surface(
        ".github/workflows/unreviewed.yml",
        "steps:\n  - shell: pwsh\n    run: $value = $(./quality/payload.ps1)\n"
    ));
}

#[test]
fn alternate_clippy_configuration_is_rejected_beside_nested_packages() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("tools/checker")).expect("nested package");
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='root'\nversion='0.1.0'\n").expect("root manifest");
    fs::write(workspace.path().join("tools/checker/Cargo.toml"), "[package]\nname='checker'\nversion='0.1.0'\n").expect("nested manifest");
    fs::write(workspace.path().join("clippy.toml"), "").expect("root Clippy configuration");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    reject_alternate_clippy_configuration(workspace.path()).expect("one canonical Clippy configuration");

    fs::write(workspace.path().join("tools/.clippy.toml"), "").expect("alternate Clippy configuration");
    assert!(
        reject_alternate_clippy_configuration(workspace.path())
            .unwrap_err()
            .to_string()
            .contains("alternate Clippy configuration")
    );
}

mod execution_surfaces;

#[test]
fn clippy_policy_requires_auditable_metadata() {
    let mut setting = clippy_setting();
    validate_clippy_configuration(&ClippyConfigurationFile {
        schema_version: 1,
        entries: vec![setting.clone()],
    })
    .expect("complete setting");
    setting.safety_invariant.clear();
    assert!(
        validate_clippy_configuration(&ClippyConfigurationFile {
            schema_version: 1,
            entries: vec![setting],
        })
        .is_err()
    );
}

fn clippy_setting() -> ClippySetting {
    ClippySetting {
        id: "clippy.threshold".to_owned(),
        key: "too-many-lines-threshold".to_owned(),
        constraint: ClippyConstraint::MaximumInteger { value: 75 },
        owner: "maintainers".to_owned(),
        issue: "issue".to_owned(),
        pull_request: "pull request".to_owned(),
        rationale: "visible debt".to_owned(),
        safety_invariant: "cannot rise".to_owned(),
        alternatives_considered: "default rejected".to_owned(),
        sentinel: "Clippy".to_owned(),
        evidence: "inventory".to_owned(),
        re_review_phase: "Phase 1".to_owned(),
    }
}

fn git(workspace: &Path, arguments: &[&str]) {
    let status = Command::new("git").current_dir(workspace).args(arguments).status().expect("run git");
    assert!(status.success());
}
