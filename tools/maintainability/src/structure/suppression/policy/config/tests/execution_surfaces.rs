use super::*;

mod dispatch_cases;

#[test]
fn command_surfaces_include_scripts_outside_the_legacy_script_directory() {
    for path in [
        "Justfile",
        "justfile",
        ".JUSTFILE",
        "module.just",
        ".mise.toml",
        "mise.development.toml",
        ".mise.windows.local.toml",
        "mise/config.local.toml",
        ".mise/config.production.toml",
        ".config/mise/config.ci.local.toml",
        ".config/mise/conf.d/quality.toml",
        ".CONFIG/MISE/CONF.D/QUALITY.TOML",
        ".rtx.toml",
        ".github/workflows/ci.yml",
        ".github/actions/check/action.yaml",
        ".cargo/config",
        "nested/.cargo/config.toml",
        "nested/.CARGO/CONFIG.TOML",
        "script/release.py",
        "tools/ci/action.js",
        "tools/ci/action.cjs",
        "tools/ci/action.mjs",
        "tools/ci/check.sh",
        "tools/ci/check.PS1",
        "Makefile",
        "build/lint.mk",
        "package.json",
    ] {
        assert!(is_execution_surface(path), "missing command surface {path}");
    }
    assert!(!is_execution_surface("CONTRIBUTING.md"));
    assert!(!is_execution_surface("src/lib.rs"));
}

#[test]
fn command_policy_rejects_cargo_configuration_relocation() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(workspace.path().join("script/check.sh"), "cargo check\n").expect("safe command");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    reject_checked_in_weakening(workspace.path()).expect("safe Cargo command");

    fs::create_dir_all(workspace.path().join(".cargo")).expect("Cargo configuration directory");
    fs::write(workspace.path().join(".cargo/config.toml"), "[build]\nrustflags = ['-A', 'warnings']\n").expect("Cargo configuration");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("Cargo configuration"));
    fs::remove_dir_all(workspace.path().join(".cargo")).expect("remove Cargo configuration");

    fs::write(workspace.path().join("script/check.sh"), "CARGO_HOME=$DYNAMIC_HOME cargo check\n").expect("Cargo home injection");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("environment channel"));

    fs::write(workspace.path().join("script/check.sh"), "cargo -Z unstable-options -C ../other check\n").expect("Cargo directory relocation");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"));

    fs::remove_file(workspace.path().join("script/check.sh")).expect("delete command surface");
    reject_checked_in_weakening(workspace.path()).expect("deleted command surfaces are absent");
}

#[test]
fn command_policy_scans_extensionless_scripts() {
    for (source, executable) in [("cargo clippy -- -A warnings\n", true), ("#!/bin/sh\ncargo clippy -- -A warnings\n", false)] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join("tools")).expect("tool directory");
        fs::write(workspace.path().join("tools/run-lints"), source).expect("extensionless lint script");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", "."]);
        if executable {
            git(workspace.path(), &["update-index", "--chmod=+x", "tools/run-lints"]);
        }

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"));
    }

    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Justfile"), "lint:\n    bash quality/lint.txt\n").expect("interpreter invocation");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("non-executable lint script");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");
}

#[test]
fn command_policy_rejects_unanalyzed_interpreter_programs() {
    for (interpreter, program, source) in [
        (
            "python3",
            "quality/lint.txt",
            "import os\nos.system(bytes.fromhex('636172676f20636c69707079202d2d202d41207761726e696e6773'))\n",
        ),
        ("perl", "quality/lint.pl", "system(\"cargo\", \"clippy\", \"--\", \"-\" . \"A\", \"warnings\")\n"),
        ("ruby", "quality/lint.rb", "system(\"cargo\", \"clippy\", \"--\", \"-\" + \"A\", \"warnings\")\n"),
        ("tclsh", "quality/lint.tcl", "exec cargo clippy -- [format %cA 45] warnings\n"),
    ] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
        fs::write(workspace.path().join("Justfile"), format!("lint:\n    {interpreter} {program}\n")).expect("interpreter invocation");
        fs::write(workspace.path().join(program), source).expect("non-executable lint program");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", "."]);

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
    }
}

#[test]
fn command_policy_rejects_cargo_run_helpers() {
    for cargo in ["cargo", "cargo.exe"] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join("quality/helper/src")).expect("helper source directory");
        fs::write(
            workspace.path().join("Justfile"),
            format!("lint:\n    {cargo} run --manifest-path quality/helper/Cargo.toml\n"),
        )
        .expect("Cargo helper invocation");
        fs::write(
            workspace.path().join("quality/helper/Cargo.toml"),
            "[package]\nname = \"lint-helper\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .expect("helper manifest");
        fs::write(
            workspace.path().join("quality/helper/src/main.rs"),
            "fn main() { std::process::Command::new(\"cargo\").args([\"clippy\", \"--\", \"-A\", \"warnings\"]).status().unwrap(); }\n",
        )
        .expect("helper source");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", "."]);

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
    }
}

#[test]
fn command_policy_scans_timeout_wrapped_programs() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Justfile"), "lint:\n    timeout 10 sh quality/lint.txt\n").expect("timeout invocation");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("non-executable lint program");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");
}

#[test]
fn command_policy_scans_nice_wrapped_programs() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Justfile"), "lint:\n    nice sh quality/lint.txt\n").expect("nice invocation");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("non-executable lint program");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");
}

#[test]
fn command_policy_scans_nohup_wrapped_programs() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Justfile"), "lint:\n    nohup sh quality/lint.txt\n").expect("nohup invocation");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("non-executable lint program");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");
}

#[test]
fn command_policy_rejects_script_command_indirection() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Justfile"), "lint:\n    script -q -e -c 'sh quality/lint.txt' /dev/null\n").expect("script command invocation");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("hidden lint program");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn command_policy_rejects_setpriv_command_indirection() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Justfile"), "lint:\n    setpriv --no-new-privs sh quality/lint.txt\n").expect("setpriv invocation");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("hidden lint program");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn command_policy_rejects_sed_program_files() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Justfile"), "lint:\n    sed -f quality/lint.sed /etc/hosts\n").expect("sed invocation");
    fs::write(workspace.path().join("quality/lint.sed"), "1e cargo clippy -- -A warnings\n").expect("non-executable sed program");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn command_policy_scans_awk_program_files() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Justfile"), "lint:\n    awk -f quality/lint.awk /etc/hosts\n").expect("awk invocation");
    fs::write(workspace.path().join("quality/lint.awk"), r#"{ system("cargo clippy -- -A warnings") }"#).expect("non-executable awk program");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn just_templates_cannot_construct_compiler_invocations() {
    assert!(weakening_token_for_surface(
        "Justfile",
        "lint_level := \"A\"\ncheck:\n    cargo clippy -- -{{ lint_level }} warnings\n"
    ));
    assert!(weakening_token_for_surface(
        "Justfile",
        "compiler := \"clippy\"\nlint_level := \"A\"\ncheck:\n    cargo {{ compiler }} -- -{{ lint_level }} warnings\n"
    ));
    assert!(weakening_token_for_surface("Justfile", "check:\n    {{ cargo }} clippy -- -D warnings\n"));
    assert!(weakening_token_for_surface("Justfile", "check:\n    {{cargo}} clippy -- -D warnings\n"));
    assert!(weakening_token_for_surface("Justfile", "check:\n    -just check-quality\n"));
    assert!(weakening_token_for_surface("Justfile", "check:\n    @-cargo clippy -- -D warnings\n"));
    assert!(weakening_token_for_surface("Justfile", "check:\n    -env -u RUSTFLAGS just check-quality\n"));
    assert!(!weakening_token_for_surface("Justfile", "check:\n    @cargo clippy -- -D warnings\n"));
    assert!(!weakening_token_for_surface("Justfile", "check:\n    cargo nextest run {{ ARGS }}\n"));
    assert!(!weakening_token_for_surface(
        "mise.toml",
        "CARGO_HOME = \"{{ env.XDG_CACHE_HOME | default(value=env.HOME) }}/localhold/cargo\"\n"
    ));
}

#[test]
fn command_policy_rejects_find_and_xargs_command_indirection() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("quality/args.txt"), "quality/lint.txt\n").expect("xargs input");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("weakening command");
    fs::write(workspace.path().join("Justfile"), "lint:\n    find /tmp -maxdepth 0 -exec sh quality/lint.txt \\;\n").expect("find invocation");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    fs::write(workspace.path().join("Justfile"), "lint:\n    xargs -a quality/args.txt sh\n").expect("xargs invocation");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn command_policy_rejects_unparsed_shell_dispatchers() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("opaque command payload");
    fs::write(workspace.path().join("script/check.sh"), "coproc sh quality/lint.txt\n").expect("initial shell dispatcher");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for &(command, reason) in dispatch_cases::SHELL_DISPATCH_CASES {
        fs::write(workspace.path().join("script/check.sh"), command).expect("opaque shell dispatcher");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains(reason), "{command}: {error:#}");
    }

    fs::write(
        workspace.path().join("script/check.sh"),
        "printf '%s\\n' '<(sh quality/lint.txt)'\ncat <<'DOC'\n$(sh quality/lint.txt)\nDOC\n",
    )
    .expect("inert shell examples");
    reject_checked_in_weakening(workspace.path()).expect("quoted shell examples are inert");
}

#[test]
fn command_policy_rejects_dynamic_powershell_call_dispatch() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(
        workspace.path().join("script/check.ps1"),
        "$tool = 'cargo'\n$subcommand = 'clippy'\n$flag = '-A'\n& $tool $subcommand -- $flag warnings\n",
    )
    .expect("dynamic PowerShell call");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.ps1"),
        "[scriptblock]::Create((-join (99,97,114,103,111 | ForEach-Object {[char]$_}))).Invoke()\n",
    )
    .expect("dynamic PowerShell script block");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.ps1"),
        "[System.Diagnostics.Process]::Start($tool, $arguments).WaitForExit()\n",
    )
    .expect(".NET process dispatch");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    for source in [
        "Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{CommandLine=$command}\n",
        "Invoke-WmiMethod -Class Win32_Process -Name Create -ArgumentList $command\n",
        "[wmiclass]'Win32_Process'::Create($command)\n",
    ] {
        fs::write(workspace.path().join("script/check.ps1"), source).expect("CIM or WMI process dispatch");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"), "{source}: {error:#}");
    }

    fs::write(
        workspace.path().join("script/check.ps1"),
        "& cargo clippy -- -D warnings\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n",
    )
    .expect("checked static PowerShell call");
    reject_checked_in_weakening(workspace.path()).expect("status-checked static PowerShell call is analyzable");
}

#[test]
fn command_policy_rejects_python_command_wrapper_dispatch() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(
        workspace.path().join("script/check.py"),
        "import subprocess\nsubprocess.run([\"env\", \"sh\", \"-c\", bytes.fromhex(\"636172676f20636c69707079202d2d202d41207761726e696e6773\").decode()])\n",
    )
    .expect("Python command-wrapper argv call");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "import subprocess\nsubprocess.run([\"git\", \"status\"])\nrunner = subprocess.run\nrunner(bytes.fromhex(\"636172676f\").decode(), shell=True)\n",
    )
    .expect("assigned Python process callable");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "exec(bytes.fromhex(\"696d706f7274206f733b206f732e73797374656d2827636172676f20636c69707079202d2d202d41207761726e696e67732729\"))\n",
    )
    .expect("Python dynamic code evaluation");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "__import__(\"os\").system(bytes.fromhex(\"636172676f20636c69707079202d2d202d41207761726e696e6773\").decode())\n",
    )
    .expect("Python dynamic import");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(workspace.path().join("script/check.py"), "import runpy\nrunpy.run_path('quality/lint.txt')\n").expect("Python runpy execution");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "import io, pickle\npickle.Unpickler(io.BytesIO(bytes.fromhex(payload))).load()\n",
    )
    .expect("Python Unpickler execution");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");
}

#[test]
fn command_policy_governs_opaque_shell_programs_and_selected_makefiles() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Justfile"), "shell:\n    bash -c \"$(cat quality/lint.txt)\"\n").expect("opaque shell dispatch");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("unreviewed shell program");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    fs::write(
        workspace.path().join("Justfile"),
        r#"eval:
    command eval "$(printf '\143\141\162\147\157\040\143\154\151\160\160\171\040\055\055\040\055\101\040\167\141\162\156\151\156\147\163')"
"#,
    )
    .expect("opaque eval dispatch");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    fs::write(
        workspace.path().join("Justfile"),
        "transient:\n    printf '%s\\n' '#!/bin/sh' 'cargo clippy -- -A warnings' > quality/run-lints\n    chmod +x quality/run-lints\n    quality/run-lints\n",
    )
    .expect("transient relative program");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("tracked path inventory"), "{error:#}");

    fs::write(workspace.path().join("Justfile"), "make:\n    make -f quality/lint.rules\n").expect("Make dispatch");
    fs::write(workspace.path().join("quality/lint.rules"), "lint:\n\tcargo clippy -- -A warnings\n").expect("selected Makefile");
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");
}

#[test]
fn javascript_command_surfaces_fail_closed_instead_of_using_shell_parsing() {
    for extension in ["js", "cjs", "mjs"] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join("quality")).expect("command directory");
        let program = format!("quality/check.{extension}");
        fs::write(workspace.path().join("Justfile"), format!("lint:\n    ./{program}\n")).expect("relative JavaScript invocation");
        fs::write(
            workspace.path().join(&program),
            "execFileSync(\"cargo\", [\n  \"clippy\",\n  \"--\",\n  \"-A\",\n  \"warnings\",\n]);\n",
        )
        .expect("JavaScript command surface");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", "."]);
        git(workspace.path(), &["update-index", "--chmod=+x", &program]);

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("JavaScript command surface"), "{extension}: {error:#}");
    }
}

#[test]
fn local_composite_actions_are_scanned_in_any_directory() {
    for command in ["cargo clippy -- -A warnings", "CARGO.EXE clippy -- -A warnings"] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join("actions/lint")).expect("action directory");
        fs::write(
            workspace.path().join("actions/lint/action.yml"),
            format!("name: lint\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: {command}\n"),
        )
        .expect("composite action");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", "."]);

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"));
    }
}

#[test]
fn local_node_actions_are_rejected_before_unscanned_entrypoints_can_run() {
    for entrypoint in ["index.js", "dist/entrypoint"] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let action = workspace.path().join("actions/lint");
        fs::create_dir_all(action.join("dist")).expect("action directory");
        fs::write(action.join("action.yml"), format!("name: lint\nruns:\n  using: node20\n  main: {entrypoint}\n")).expect("Node action metadata");
        fs::write(action.join(entrypoint), "require('node:child_process').execSync('cargo clippy -- -A warnings');\n").expect("Node action entrypoint");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", "."]);

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("only composite local actions are supported"));
    }
}

#[test]
fn remote_actions_require_a_reviewed_exact_revision() {
    for reference in [
        "attacker/lint-action@1111111111111111111111111111111111111111",
        "actions/checkout@main",
        "docker://attacker/lint:latest",
        "${{ github.event.pull_request.head.ref }}",
    ] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join(".github/workflows")).expect("workflow directory");
        fs::write(
            workspace.path().join(".github/workflows/lint.yml"),
            format!("name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: {reference}\n"),
        )
        .expect("workflow");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", "."]);

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("reviewed exact-revision allowlist"));
    }
}

#[test]
fn reviewed_remote_and_repository_local_actions_are_accepted() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join(".github/workflows")).expect("workflow directory");
    fs::write(
        workspace.path().join(".github/workflows/lint.yml"),
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0\n  delegated:\n    uses: ./.github/workflows/check.yml\n",
    )
    .expect("workflow");
    fs::write(
        workspace.path().join(".github/workflows/check.yml"),
        "name: check\non: workflow_call\njobs:\n  check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo check\n",
    )
    .expect("local workflow");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    reject_checked_in_weakening(workspace.path()).expect("reviewed action references");
}

#[test]
fn local_action_references_must_resolve_to_tracked_files() {
    for (reference, generated_path, generated_source) in [
        (
            "./.github/actions/generated",
            ".github/actions/generated/action.yml",
            "name: generated\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: |\n        cargo check\n",
        ),
        (
            "./.github/workflows/generated.yml",
            ".github/workflows/generated.yml",
            "name: generated\non: workflow_call\njobs:\n  check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: |\n          cargo check\n",
        ),
    ] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join(".github/workflows")).expect("workflow directory");
        fs::write(
            workspace.path().join(".github/workflows/lint.yml"),
            format!("name: lint\non: push\njobs:\n  delegated:\n    uses: {reference}\n"),
        )
        .expect("workflow");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", ".github/workflows/lint.yml"]);
        fs::create_dir_all(workspace.path().join(generated_path).parent().expect("generated parent")).expect("generated directory");
        fs::write(workspace.path().join(generated_path), generated_source).expect("untracked generated command surface");

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("tracked"), "{error:#}");
    }
}

#[test]
fn github_yaml_rejects_unsupported_execution_metadata() {
    for source in [
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    env:\n      COMMAND: &lint cargo clippy -- -A warnings\n    steps:\n      - run: *lint\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - &lint run: cargo clippy -- -A warnings\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - !audit run: cargo clippy -- -A warnings\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - { run: \"cargo clippy -- -A warnings\" }\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - shell: bash -c 'cargo clippy -- -A warnings' -- {0}\n        run: just maintainability\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - shell: |\n          bash -c 'cargo clippy -- -A warnings' -- {0}\n        run: just maintainability\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: windows-latest\n    defaults:\n      run:\n        shell: pwsh\n    steps:\n      - run: cargo clippy --locked -- -D warnings\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: >\n          # hidden by incorrect folding\n            cargo clippy -- -A warnings\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - working-directory: misc\n        run: rustc check.rs\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - 'working-directory' : misc\n        run: rustc check.rs\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    defaults: {run: {working-directory: misc}}\n    steps:\n      - run: rustc check.rs\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps: [{run: cargo clippy -- -A warnings}]\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: \"cargo clippy --\n          -A warnings\"\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo clippy --\n          -A warnings\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - \"r\\u0075n\": cargo clippy -- -A warnings\n",
        "name: lint\non: push\njobs:\n  lint:\n    strategy:\n      matrix:\n        command:\n          - cargo clippy -- -A warnings\n    runs-on: ubuntu-latest\n    steps:\n      - run: ${{ matrix.command }}\n",
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: |\n          cargo ${{ matrix.subcommand }}\n",
        "name: lint\non: push\njobs:\n  lint:\n    container: attacker.example/runner:latest\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo check\n",
    ] {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join(".github/workflows")).expect("workflow directory");
        fs::write(workspace.path().join(".github/workflows/lint.yml"), source).expect("workflow");
        git(workspace.path(), &["init", "-q"]);
        git(workspace.path(), &["add", "."]);

        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(
            error.to_string().contains("anchors or aliases")
                || error.to_string().contains("unsupported shell template")
                || error.to_string().contains("unsupported default PowerShell shell")
                || error.to_string().contains("folded run scalar")
                || error.to_string().contains("working-directory")
                || error.to_string().contains("flow mapping or complex sequence")
                || error.to_string().contains("inline run scalar")
                || error.to_string().contains("quoted mapping key")
                || error.to_string().contains("dynamic run expression")
                || error.to_string().contains("job container"),
            "{error:#}"
        );
    }
}

#[test]
fn github_yaml_rejects_node_preload_environment_options() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join(".github/workflows")).expect("workflow directory");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(
        workspace.path().join(".github/workflows/lint.yml"),
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    env:\n      NODE_OPTIONS: --require=${{ github.workspace }}/quality/lint.data\n    steps:\n      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0\n      - run: just check-quality\n",
    )
    .expect("workflow");
    fs::write(
        workspace.path().join("quality/lint.data"),
        "require('node:fs').writeFileSync('Justfile', 'check-quality:; @true');\n",
    )
    .expect("Node preload");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening environment channel"), "{error:#}");
}

#[test]
fn github_yaml_rejects_unaudited_python_run_bodies() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join(".github/workflows")).expect("workflow directory");
    fs::write(
        workspace.path().join(".github/workflows/lint.yml"),
        "name: lint\non: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - shell: python\n        run: |\n          import os\n          os.system(bytes.fromhex('7368207175616c6974792f6c696e742e747874').decode())\n",
    )
    .expect("Python workflow");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("unsupported shell template"), "{error:#}");
}

#[test]
fn command_policy_rejects_sourced_environment_files() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("policy")).expect("policy directory");
    fs::write(workspace.path().join("policy/lints.env"), "export RUSTFLAGS=--cap-lints=allow\n").expect("lint environment");
    fs::write(workspace.path().join("script/check.sh"), ". policy/lints.env\ncargo clippy -- -D warnings\n").expect("dot-sourced lint environment");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for command in [
        ". policy/lints.env",
        "source policy/lints.env",
        "time -p source policy/lints.env",
        ">source.log source policy/lints.env",
        "load() { source policy/lints.env; }; load",
        "load () { source policy/lints.env; }; load",
        "case yes in yes) source policy/lints.env;; esac",
        "case yes in no|yes) source policy/lints.env;; esac",
        "case yes in no) :;; yes) source policy/lints.env;; esac",
        "case yes in\n  yes) . policy/lints.env ;;\nesac",
        "noglob source policy/lints.env",
        "nocorrect . policy/lints.env",
    ] {
        fs::write(workspace.path().join("script/check.sh"), format!("{command}\ncargo clippy -- -D warnings\n")).expect("sourced lint environment");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        if matches!(command, ". policy/lints.env" | "source policy/lints.env") {
            assert!(error.to_string().contains("sourced-file indirection"), "{command}: {error:#}");
        }
    }
}

#[test]
fn command_policy_rejects_opaque_dynamic_command_names() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("script/check.sh"), "command=$(cat quality/lint.txt)\n$command\n").expect("dynamic command surface");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("opaque command payload");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn make_include_indirection_is_rejected() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("build")).expect("Make fragment directory");
    fs::write(workspace.path().join("Makefile"), "include build/lint.mk\n").expect("Makefile");
    fs::write(workspace.path().join("build/lint.mk"), "lint:\n\tcargo clippy -- -A warnings\n").expect("Make fragment");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("include indirection"));
}

#[test]
fn make_command_producing_expansions_are_rejected() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("Makefile"), "lint:\n\t$(shell cat quality/lint.txt)\n").expect("Makefile");
    fs::write(workspace.path().join("quality/lint.txt"), "cargo clippy -- -A warnings\n").expect("hidden lint recipe");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("command-producing expansion"), "{error:#}");
}

#[test]
fn make_recipe_shell_selection_is_rejected() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("Makefile"), "SHELL := /bin/true\nlint:\n\tcargo clippy -- -D warnings\n").expect("Makefile");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("recipe shell selection"), "{error:#}");
}

#[test]
fn yaml_source_labels_are_not_shell_indirection() {
    assert!(!has_sourced_file_indirection(
        ".github/workflows/ci.yml",
        "steps:\n  - name: Restore source cache\n    run: cargo clippy\n"
    ));
    assert!(!has_sourced_file_indirection("script/install.sh", "Builds LocalHold from the locked source tree\n"));
    assert!(has_sourced_file_indirection(
        "script/check.sh",
        "if MODE=strict source policy/lints.env; then cargo clippy; fi\n"
    ));
    assert!(has_sourced_file_indirection(
        ".github/workflows/ci.yml",
        "steps:\n  - run: |\n      . policy/lints.env\n      cargo clippy\n"
    ));
    assert!(has_sourced_file_indirection(".github/workflows/ci.yml", "steps:\n  - run: source policy/lints.env\n"));
}

#[test]
fn command_policy_rejects_directly_compiled_rust_helpers() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(workspace.path().join("script/check.sh"), "rustc script/check.rs\n").expect("direct compiler command");
    fs::write(workspace.path().join("script/check.rs"), "fn main() {}\n").expect("direct Rust source");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque command helper"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.rs"),
        "fn main() { std::process::Command::new(\"cargo\").args([\"clippy\", \"--\", \"-A\", \"warnings\"]).status().unwrap(); }\n",
    )
    .expect("process-spawning Rust helper");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque command helper"));

    fs::write(workspace.path().join("script/check.sh"), "rustc --version\n").expect("compiler version command");
    reject_checked_in_weakening(workspace.path()).expect("informational compiler command");

    fs::write(workspace.path().join("script/check.sh"), "rustc \"$DIRECT_SOURCE\"\n").expect("opaque direct compiler command");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"));

    fs::create_dir(workspace.path().join("misc")).expect("alternate compiler directory");
    fs::write(workspace.path().join("check.rs"), "fn main() {}\n").expect("root Rust source");
    fs::write(workspace.path().join("misc/check.rs"), "fn main() {}\n").expect("alternate Rust source");
    fs::write(workspace.path().join("script/check.sh"), "cd misc && rustc --version\n").expect("relocated informational command");
    reject_checked_in_weakening(workspace.path()).expect("relocated informational compiler command");

    fs::write(workspace.path().join("script/check.sh"), "cd misc && rustc check.rs\n").expect("relocated compiler command");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("without auditable repository-relative .rs inputs"));

    fs::write(workspace.path().join("misc/Cargo.toml"), "[package]\nname='unchecked'\nversion='0.1.0'\n").expect("alternate package manifest");
    fs::write(workspace.path().join("script/check.sh"), "cd misc && cargo clippy -- -D warnings\n").expect("relocated Cargo command");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("without auditable repository-relative .rs inputs"));

    fs::write(workspace.path().join("script/check.sh"), "env --chdir=quality cargo clippy -- -D warnings\n").expect("env-relocated Cargo command");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("without auditable repository-relative .rs inputs"));

    fs::write(workspace.path().join("script/check.sh"), "env -C misc rustc check.rs\n").expect("env-relocated compiler command");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("without auditable repository-relative .rs inputs"));

    let reviewed_root = r#"set -e
repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
readonly repository_root
cd -- "$repository_root"
cargo clippy -- -D warnings
"#;
    fs::write(workspace.path().join("script/check.sh"), reviewed_root).expect("audited repository-root command");
    reject_checked_in_weakening(workspace.path()).expect("audited repository-root Cargo command");

    fs::write(
        workspace.path().join("script/check.sh"),
        format!("{reviewed_root}repository_root=quality\ncd -- \"$repository_root\"\ncargo clippy -- -D warnings\n"),
    )
    .expect("reassigned repository-root command");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("without auditable repository-relative .rs inputs"));

    fs::write(workspace.path().join("script/check.sh"), "rustc --version\n").expect("safe shell command");
    fs::write(
        workspace.path().join("script/check.py"),
        "import subprocess\nsubprocess.run([\n    \"rustc\",\n    \"check.rs\",\n], cwd=\"misc\", check=True)\n",
    )
    .expect("Python compiler command");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("script/check.py"));
    assert!(error.to_string().contains("without auditable repository-relative .rs inputs"));
}
