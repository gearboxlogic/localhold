use super::*;

#[test]
fn command_policy_rejects_extra_parsed_mise_environment_channels() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let reviewed = concat!(
        "[env]\n",
        "CARGO_HOME = '{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \"/.cache\") }}/localhold/cargo'\n",
        "RUSTUP_HOME = '{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \"/.cache\") }}/localhold/rustup'\n",
        "_.path = ['{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \"/.cache\") }}/localhold/cargo/bin']\n",
    );
    fs::write(workspace.path().join("mise.toml"), reviewed).expect("reviewed mise configuration");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    reject_checked_in_weakening(workspace.path()).expect("reviewed mise environment");

    for extra in [
        "\"RUST\\u0046LAGS\" = '-A warnings'\n",
        "PATH = 'quality/bin'\n",
        "[tasks.check.env]\npath = 'quality/bin'\n",
        "[tasks.check.env]\n\"RUST\\u0046LAGS\" = '-A warnings'\n",
    ] {
        fs::write(workspace.path().join("mise.toml"), format!("{reviewed}\n{extra}")).expect("tampered mise configuration");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening environment channel"), "{extra}: {error:#}");
    }
    for changed in [reviewed.replace("CARGO_HOME", "cargo_home"), reviewed.replace("RUSTUP_HOME", "Rustup_Home")] {
        fs::write(workspace.path().join("mise.toml"), changed).expect("case-changed mise configuration");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening environment channel"), "{error:#}");
    }
}

#[test]
fn command_policy_follows_literal_mise_task_files() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("mise.toml"), "[tasks.check]\nfile = 'quality/helper'\n").expect("mise task");
    fs::write(workspace.path().join("quality/helper"), "cargo clippy -- -A warnings\n").expect("task file");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("quality/helper"), "{error:#}");
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    for metadata in [
        "#MISE env={RUSTFLAGS='-A warnings'}\n",
        "# [MISE] \"e\\u006ev\"={RUSTFLAGS='-A warnings'}\n",
        "#MISE tools={\n#MISE rust='nightly'\n#MISE }\n",
        "# [MISE] cache.command_inputs=['quality/key.sh']\n",
        "//MISE \"d\\u0069r\"='../outside'\n",
        "#MISE depends=[\n#MISE 'setup'\n#MISE ]\n",
        "#MISE description=\"{{ \\u0065xec(command='quality/helper') }}\"\n",
        "#MISE alias=\"\"\"\n#MISE {{ read_file (\n#MISE path='quality/helper'\n#MISE ) }}\n#MISE \"\"\"\n",
        "#MISE description=\"{% if \\u0065xec(command='quality/helper') %}check{% endif %}\"\n",
        "#MISE alias=\"\"\"\n#MISE {% if read_file (\n#MISE path='quality/helper'\n#MISE ) %}check{% endif %}\n#MISE \"\"\"\n",
    ] {
        fs::write(workspace.path().join("quality/helper"), format!("{metadata}cargo clippy -- -D warnings\n")).expect("task metadata");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("quality/helper"), "{metadata}: {error:#}");
    }
    for prefix in [
        "#USAGE",
        "#[USAGE]",
        "# [USAGE]",
        "//USAGE",
        "//[USAGE]",
        "// [USAGE]",
        "::USAGE",
        "::[USAGE]",
        ":: [USAGE]",
    ] {
        let metadata = format!("{prefix} mount \"quality/generate-spec\"\n");
        fs::write(workspace.path().join("quality/helper"), format!("{metadata}cargo clippy -- -D warnings\n")).expect("usage metadata");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("quality/helper"), "{metadata}: {error:#}");
    }
}

#[test]
fn command_policy_rejects_batch_file_task_usage_metadata() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("mise.toml"), "[tasks.check]\nfile = 'quality/helper.bat'\n").expect("mise task");
    fs::write(
        workspace.path().join("quality/helper.bat"),
        "::[USAGE] mount \"quality/generate-spec\"\r\n@cargo clippy -- -D warnings\r\n",
    )
    .expect("batch task file");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("quality/helper.bat"), "{error:#}");
}

#[test]
fn command_policy_rejects_unresolved_mise_task_templates() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::write(
        workspace.path().join("mise.toml"),
        "[settings]\nexperimental = true\n[task_templates.quality]\nrun = 'cargo clippy -- -A warnings'\nenv = { RUSTFLAGS = '-A warnings' }\n[tasks.check]\nextends = 'quality'\n",
    )
    .expect("mise task template");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("mise.toml"), "{error:#}");
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn command_policy_rejects_unresolved_mise_task_context() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    git(workspace.path(), &["init", "-q"]);
    for task in [
        "run = [{ task = 'lint', env = { RUSTFLAGS = '-A warnings' } }]",
        "run = [{ task = 'lint', args = ['--', '-A', 'warnings'] }]",
        "run = 'cargo clippy -- -D warnings'\ndepends = ['RUSTFLAGS=-A setup']",
        "run = './quality/check.sh'\ndir = '../outside'",
    ] {
        fs::write(workspace.path().join("mise.toml"), format!("[tasks.check]\n{task}\n")).expect("mise task context");
        git(workspace.path(), &["add", "."]);
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("mise.toml"), "{task}: {error:#}");
        assert!(error.to_string().contains("opaque interpreter program"), "{task}: {error:#}");
    }
}

#[test]
fn command_policy_rejects_unmodeled_mise_execution_channels() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    git(workspace.path(), &["init", "-q"]);
    for source in [
        "env_file = '.env'\n",
        "dotenv = '.env'\n",
        "env_path = 'quality/bin'\n",
        "[vars._]\nsource = 'quality/vars.sh'\n",
        "[tasks.check]\nrun = 'cargo clippy'\ntools = { rust = 'nightly' }\n",
        "[tasks.check]\nrun = 'cargo clippy'\ncache = { command_inputs = ['quality/key.sh'] }\n",
        "[tasks.check]\nrun = 'cargo clippy'\ncache = true\n",
        "[tasks.check]\nrun = 'cargo clippy'\nsources = ['src/**']\noutputs = ['target/check']\n",
        "[tasks.check]\nrun = 'cargo clippy'\nusage = 'complete \"plugin\" run=\"quality/generate-spec\"'\n",
        "[tasks.check.vars._]\nfile = 'quality/vars.toml'\n",
        "[task_config.cache]\ncommand_inputs = ['quality/key.sh']\n",
        "[hooks]\nenter = { task = 'check' }\n",
        "[[watch_files]]\npatterns = ['src/**']\ntask = 'check'\n",
        "[monorepo]\nconfig_roots = ['../outside']\n",
        "[monorepo.task_defaults.check]\nenv = { RUSTFLAGS = '-A warnings' }\n",
        "monorepo_root = '../outside'\n",
        "[deps.quality]\nauto = true\nrun = 'cargo clippy -- -A warnings'\n",
        "[bootstrap.hooks]\npre = 'cargo clippy -- -A warnings'\n",
        "[dotfiles]\nrepo = 'https://example.invalid/config'\n",
        "[oci.quality]\nimage = 'example.invalid/quality'\n",
        "[plugins]\nrust = 'https://example.invalid/plugin'\n",
        "[alias]\nrust = 'custom-rust'\n",
        "[tool_alias]\nrust = 'custom-rust'\n",
        "[tools.rust]\npath = '../outside/rustc'\n",
        "[tools.rust]\ninstall_env = { RUSTFLAGS = '-A warnings' }\n",
        "[tools.rust]\nversion = '1.97.0'\nbinary_path = '../outside/rustc'\n",
        "[settings]\ntrusted_config_paths = ['../outside']\n",
        "[settings]\nshorthands_file = '../outside.toml'\n",
    ] {
        fs::write(workspace.path().join("mise.toml"), source).expect("mise execution channel");
        git(workspace.path(), &["add", "."]);
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("mise.toml"), "{source}: {error:#}");
    }
}
