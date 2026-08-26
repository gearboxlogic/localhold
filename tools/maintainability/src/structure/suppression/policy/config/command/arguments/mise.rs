use std::path::{Component, Path};

use toml::Value;

const FILE_TASK_USAGE_PREFIXES: [&str; 9] = [
    "#USAGE",
    "#[USAGE]",
    "# [USAGE]",
    "//USAGE",
    "//[USAGE]",
    "// [USAGE]",
    "::USAGE",
    "::[USAGE]",
    ":: [USAGE]",
];

pub(super) struct Analysis {
    pub(super) commands: Vec<String>,
    pub(super) unresolved: bool,
    pub(super) environment_weakening: bool,
    weakening_environment: Vec<WeakeningEnvironment>,
}

#[derive(Debug, PartialEq)]
struct WeakeningEnvironment {
    location: String,
    name: String,
    value: Value,
}

pub(super) fn analyze(source: &str) -> Result<Analysis, toml::de::Error> {
    let root = source.parse::<toml::Table>()?;
    let mut analysis = Analysis {
        commands: Vec::new(),
        unresolved: root.keys().any(|key| !modeled_root_key(key)) || root.values().any(template_executes_code),
        environment_weakening: false,
        weakening_environment: Vec::new(),
    };
    if ["env_file", "dotenv", "env_path"].iter().any(|key| root.contains_key(*key)) {
        analysis.unresolved = true;
        analysis.environment_weakening = true;
    }
    if let Some(tasks) = root.get("tasks") {
        collect_tasks(tasks, &mut analysis);
    }
    if let Some(hooks) = root.get("hooks") {
        collect_hooks(hooks, &mut analysis);
    }
    if let Some(watch_files) = root.get("watch_files") {
        collect_command_entries(watch_files, &["run", "run_windows", "shell"], &mut analysis);
    }
    if let Some(tools) = root.get("tools") {
        collect_tools(tools, &mut analysis);
    }
    if let Some(aliases) = root.get("shell_alias") {
        collect_strings(aliases, &mut analysis.commands, &mut analysis.unresolved);
    }
    if let Some(settings) = root.get("settings") {
        collect_settings(settings, &mut analysis);
    }
    if let Some(task_config) = root.get("task_config") {
        collect_task_config(task_config, &mut analysis);
    }
    if let Some(environment) = root.get("env") {
        collect_environment(environment, "env", &mut analysis);
    }
    Ok(analysis)
}

fn modeled_root_key(key: &str) -> bool {
    matches!(key, "tasks" | "hooks" | "watch_files" | "tools" | "shell_alias" | "settings" | "task_config" | "env")
}

pub(super) fn reviewed_environment_is_exact(source: &str) -> bool {
    let Ok(analysis) = analyze(source) else {
        return false;
    };
    if analysis.unresolved {
        return false;
    }
    let cache = "{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \"/.cache\") }}/localhold";
    let expected = [
        WeakeningEnvironment {
            location: "env".to_owned(),
            name: "CARGO_HOME".to_owned(),
            value: Value::String(format!("{cache}/cargo")),
        },
        WeakeningEnvironment {
            location: "env".to_owned(),
            name: "RUSTUP_HOME".to_owned(),
            value: Value::String(format!("{cache}/rustup")),
        },
        WeakeningEnvironment {
            location: "env".to_owned(),
            name: "_.path".to_owned(),
            value: Value::Array(vec![Value::String(format!("{cache}/cargo/bin"))]),
        },
    ];
    analysis.weakening_environment.len() == expected.len()
        && expected
            .iter()
            .all(|expected| analysis.weakening_environment.iter().filter(|observed| *observed == expected).count() == 1)
}

pub(super) fn file_task_metadata_is_unresolved(source: &str) -> bool {
    let mut metadata = Vec::new();
    for line in source.lines().map(str::trim_start) {
        if FILE_TASK_USAGE_PREFIXES.iter().any(|prefix| line.starts_with(prefix)) {
            return true;
        }
        if let Some(value) = ["#MISE", "# [MISE]", "//MISE", "// [MISE]"].into_iter().find_map(|prefix| line.strip_prefix(prefix)) {
            metadata.push(value);
        }
    }
    if metadata.is_empty() {
        return false;
    }
    metadata.join("\n").parse::<toml::Table>().map_or(true, |table| {
        table.values().any(template_executes_code) || table.keys().any(|key| !matches!(key.as_str(), "description" | "alias" | "aliases" | "hide" | "quiet"))
    })
}

fn template_executes_code(value: &Value) -> bool {
    match value {
        Value::String(source) => template_blocks(source).any(|template| template_function_called(template, "exec") || template_function_called(template, "read_file")),
        Value::Array(values) => values.iter().any(template_executes_code),
        Value::Table(table) => table.values().any(template_executes_code),
        _ => false,
    }
}

fn template_blocks(source: &str) -> impl Iterator<Item = &str> {
    [("{{", "}}"), ("{%", "%}")].into_iter().flat_map(|(opening, closing)| {
        source
            .split(opening)
            .skip(1)
            .filter_map(move |template| template.split_once(closing).map(|(block, _)| block))
    })
}

fn template_function_called(template: &str, name: &str) -> bool {
    template.match_indices(name).any(|(index, _)| {
        let prefix_is_boundary = template[..index]
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_alphanumeric() && character != '_');
        let suffix = &template[index + name.len()..];
        prefix_is_boundary && suffix.trim_start().starts_with('(')
    })
}

fn collect_tasks(value: &Value, analysis: &mut Analysis) {
    let Some(tasks) = value.as_table() else {
        analysis.unresolved = true;
        return;
    };
    for (name, task) in tasks {
        match task {
            Value::String(command) => analysis.commands.push(command.clone()),
            Value::Table(_) => collect_task(name, task, analysis),
            _ => analysis.unresolved = true,
        }
    }
}

fn collect_task(name: &str, value: &Value, analysis: &mut Analysis) {
    collect_command_fields(value, &["run", "run_windows", "shell"], analysis);
    let Some(task) = value.as_table() else {
        return;
    };
    if ["depends", "depends_post", "wait_for", "dir", "tools", "vars", "sources", "outputs", "cache", "usage"]
        .iter()
        .any(|field| task.contains_key(*field))
    {
        analysis.unresolved = true;
    }
    if let Some(environment) = task.get("env") {
        collect_environment(environment, &format!("tasks.{name}.env"), analysis);
    }
    let Some(file) = task.get("file") else {
        return;
    };
    let Some(path) = file.as_str().filter(|path| is_literal_task_path(path)) else {
        analysis.unresolved = true;
        return;
    };
    analysis.commands.push(format!("source {path}"));
}

fn is_literal_task_path(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['\\', '$', '`', '"', '\'', ';', '|', '&', '<', '>', '(', ')', '{', '}'])
        && !value.chars().any(char::is_whitespace)
        && !Path::new(value).is_absolute()
        && Path::new(value).components().all(|component| matches!(component, Component::Normal(_)))
}

fn collect_hooks(value: &Value, analysis: &mut Analysis) {
    match value {
        Value::Table(hooks) => {
            for hook in hooks.values() {
                collect_hook(hook, analysis);
            }
        }
        _ => analysis.unresolved = true,
    }
}

fn collect_hook(value: &Value, analysis: &mut Analysis) {
    match value {
        Value::String(command) => analysis.commands.push(command.clone()),
        Value::Array(hooks) => {
            for hook in hooks {
                collect_hook(hook, analysis);
            }
        }
        Value::Table(table) => {
            if table.contains_key("task") || table.contains_key("tasks") {
                analysis.unresolved = true;
            }
            collect_command_fields(value, &["run", "run_windows", "script", "scripts", "shell"], analysis);
        }
        _ => analysis.unresolved = true,
    }
}

fn collect_command_fields(value: &Value, fields: &[&str], analysis: &mut Analysis) {
    let Some(table) = value.as_table() else {
        analysis.unresolved = true;
        return;
    };
    for field in fields {
        let Some(value) = table.get(*field) else {
            continue;
        };
        if *field == "shell" {
            collect_shell(value, analysis);
        } else {
            collect_run_value(value, analysis);
        }
    }
}

fn collect_command_entries(value: &Value, fields: &[&str], analysis: &mut Analysis) {
    match value {
        Value::Array(entries) => {
            for entry in entries {
                collect_command_entries(entry, fields, analysis);
            }
        }
        Value::Table(table) => {
            if table.contains_key("task") || table.contains_key("tasks") {
                analysis.unresolved = true;
            }
            collect_command_fields(value, fields, analysis);
        }
        _ => analysis.unresolved = true,
    }
}

fn collect_run_value(value: &Value, analysis: &mut Analysis) {
    match value {
        Value::String(command) => analysis.commands.push(command.clone()),
        Value::Array(entries) => {
            for entry in entries {
                match entry {
                    Value::String(command) => analysis.commands.push(command.clone()),
                    Value::Table(reference) if reference.contains_key("task") || reference.contains_key("tasks") => analysis.unresolved = true,
                    _ => analysis.unresolved = true,
                }
            }
        }
        _ => analysis.unresolved = true,
    }
}

fn collect_shell(value: &Value, analysis: &mut Analysis) {
    let mut words = Vec::new();
    collect_flat_strings(value, &mut words, &mut analysis.unresolved);
    if !words.is_empty() {
        words.push("true".to_owned());
        analysis.commands.push(words.join(" "));
    }
}

fn collect_settings(value: &Value, analysis: &mut Analysis) {
    let Some(settings) = value.as_table() else {
        analysis.unresolved = true;
        return;
    };
    for (key, value) in settings {
        match key.as_str() {
            "experimental" | "lockfile" => {}
            "unix_default_inline_shell_args" | "windows_default_inline_shell_args" => collect_shell(value, analysis),
            "github" => collect_github_settings(value, analysis),
            _ => analysis.unresolved = true,
        }
    }
}

fn collect_github_settings(value: &Value, analysis: &mut Analysis) {
    let Some(github) = value.as_table() else {
        analysis.unresolved = true;
        return;
    };
    for (key, value) in github {
        if key == "credential_command" {
            collect_run_value(value, analysis);
        } else {
            analysis.unresolved = true;
        }
    }
}

fn collect_task_config(value: &Value, analysis: &mut Analysis) {
    let Some(config) = value.as_table() else {
        analysis.unresolved = true;
        return;
    };
    if let Some(shell) = config.get("shell") {
        collect_shell(shell, analysis);
    }
    if config.contains_key("includes") || config.contains_key("dir") || config.contains_key("cache") {
        analysis.unresolved = true;
    }
}

fn collect_environment(value: &Value, location: &str, analysis: &mut Analysis) {
    let Some(environment) = value.as_table() else {
        analysis.unresolved = true;
        return;
    };
    for (name, value) in environment.iter().filter(|(name, _)| name.as_str() != "_") {
        if name.eq_ignore_ascii_case("PATH") || super::super::environment::is_case_insensitive_weakening_environment_assignment_name(name) {
            analysis.environment_weakening = true;
            analysis.weakening_environment.push(WeakeningEnvironment {
                location: location.to_owned(),
                name: name.clone(),
                value: value.clone(),
            });
        }
    }
    let Some(directives) = environment.get("_") else {
        return;
    };
    let Some(directives) = directives.as_table() else {
        analysis.environment_weakening = true;
        analysis.unresolved = true;
        return;
    };
    for (directive, value) in directives {
        match directive.as_str() {
            "source" => {
                let mut paths = Vec::new();
                collect_source_paths(value, &mut paths, &mut analysis.unresolved);
                analysis.commands.extend(paths.into_iter().map(|path| format!("source {path}")));
            }
            "path" => {
                analysis.environment_weakening = true;
                analysis.weakening_environment.push(WeakeningEnvironment {
                    location: location.to_owned(),
                    name: "_.path".to_owned(),
                    value: value.clone(),
                });
                require_string_tree(value, &mut analysis.unresolved);
            }
            _ => {
                analysis.environment_weakening = true;
                analysis.weakening_environment.push(WeakeningEnvironment {
                    location: location.to_owned(),
                    name: format!("_.{directive}"),
                    value: value.clone(),
                });
                analysis.unresolved = true;
            }
        }
    }
}

fn require_string_tree(value: &Value, unresolved: &mut bool) {
    match value {
        Value::String(_) => {}
        Value::Array(values) => {
            for value in values {
                require_string_tree(value, unresolved);
            }
        }
        _ => *unresolved = true,
    }
}

fn collect_source_paths(value: &Value, paths: &mut Vec<String>, unresolved: &mut bool) {
    match value {
        Value::String(path) => paths.push(path.clone()),
        Value::Array(values) => {
            for value in values {
                collect_source_paths(value, paths, unresolved);
            }
        }
        Value::Table(options) => match options.get("path").or_else(|| options.get("value")) {
            Some(value) => collect_source_paths(value, paths, unresolved),
            None => *unresolved = true,
        },
        _ => *unresolved = true,
    }
}

fn collect_tools(value: &Value, analysis: &mut Analysis) {
    let Some(tools) = value.as_table() else {
        analysis.unresolved = true;
        return;
    };
    for tool in tools.values() {
        collect_tool(tool, analysis);
    }
}

fn collect_tool(value: &Value, analysis: &mut Analysis) {
    match value {
        Value::String(_) => analysis.unresolved |= path_backed_tool_tree(value),
        Value::Array(values) => {
            for value in values {
                collect_tool(value, analysis);
            }
        }
        Value::Table(properties) => {
            for (key, value) in properties {
                match key.as_str() {
                    "version" => {
                        require_string_tree(value, &mut analysis.unresolved);
                        analysis.unresolved |= path_backed_tool_tree(value);
                    }
                    "os" => require_string_tree(value, &mut analysis.unresolved),
                    "postinstall" => collect_run_value(value, analysis),
                    _ => analysis.unresolved = true,
                }
            }
        }
        _ => analysis.unresolved = true,
    }
}

fn path_backed_tool_tree(value: &Value) -> bool {
    match value {
        Value::String(version) => version.trim_start().to_ascii_lowercase().starts_with("path:"),
        Value::Array(values) => values.iter().any(path_backed_tool_tree),
        _ => false,
    }
}

fn collect_strings(value: &Value, commands: &mut Vec<String>, unresolved: &mut bool) {
    match value {
        Value::String(command) => commands.push(command.clone()),
        Value::Table(table) => {
            for value in table.values() {
                collect_strings(value, commands, unresolved);
            }
        }
        _ => *unresolved = true,
    }
}

fn collect_flat_strings(value: &Value, values: &mut Vec<String>, unresolved: &mut bool) {
    match value {
        Value::String(value) => values.push(value.clone()),
        Value::Array(entries) => {
            for entry in entries {
                collect_flat_strings(entry, values, unresolved);
            }
        }
        _ => *unresolved = true,
    }
}

#[cfg(test)]
mod tests {
    use super::{FILE_TASK_USAGE_PREFIXES, analyze, file_task_metadata_is_unresolved, reviewed_environment_is_exact};

    #[test]
    fn path_directives_are_environment_weakening_but_not_commands() {
        let analysis = analyze("[env]\nCARGO_HOME = '{{ env.HOME }}/cargo'\n_.path = ['{{ env.HOME }}/bin']\n").expect("mise config");
        assert!(analysis.commands.is_empty());
        assert!(!analysis.unresolved);
        assert!(analysis.environment_weakening);
    }

    #[test]
    fn decoded_and_task_local_environment_names_are_case_insensitive() {
        for source in [
            "[env]\n\"RUST\\u0046LAGS\" = '-A warnings'\n",
            "[env]\n\"P\\u0041TH\" = 'quality/bin'\n",
            "[env]\nrustflags = '-A warnings'\n",
            "[tasks.check]\nrun = 'cargo clippy'\n[tasks.check.env]\npath = 'quality/bin'\n",
            "[tasks.check]\nrun = 'cargo clippy'\n[tasks.check.env]\n\"RUST\\u0046LAGS\" = '-A warnings'\n",
        ] {
            let analysis = analyze(source).expect("mise config");
            assert!(analysis.environment_weakening, "{source}");
        }
        assert!(!analyze("[env]\nDOCUMENTATION_MODE = '1'\n").expect("mise config").environment_weakening);
    }

    #[test]
    fn reviewed_environment_requires_exact_root_assignments_and_values() {
        let reviewed = concat!(
            "[env]\n",
            "CARGO_HOME = '{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \"/.cache\") }}/localhold/cargo'\n",
            "RUSTUP_HOME = '{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \"/.cache\") }}/localhold/rustup'\n",
            "_.path = ['{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \"/.cache\") }}/localhold/cargo/bin']\n",
        );
        assert!(reviewed_environment_is_exact(reviewed));
        for changed in [
            format!("{reviewed}\n\"RUST\\u0046LAGS\" = '-A warnings'\n"),
            format!("{reviewed}\nPATH = 'quality/bin'\n"),
            format!("{reviewed}\n[tasks.check.env]\npath = 'quality/bin'\n"),
            reviewed.replace("/localhold/cargo'", "/other/cargo'"),
            reviewed.replace("CARGO_HOME", "cargo_home"),
            reviewed.replace("RUSTUP_HOME", "Rustup_Home"),
        ] {
            assert!(!reviewed_environment_is_exact(&changed), "{changed}");
        }
    }

    #[test]
    fn executable_mise_fields_are_collected() {
        let source = r#"
[tasks.check]
run = ["cargo check"]
run_windows = "cargo check --features windows"
shell = "bash -c"
file = "quality/check.sh"
[hooks]
enter = "./script/enter.sh"
postinstall = { run = "./script/install.sh", shell = "zsh -c" }
[[watch_files]]
patterns = ["src/**"]
run = "cargo fmt"
[tools]
node = { version = "22", postinstall = "corepack enable" }
[shell_alias]
quality = "just check"
[settings.github]
credential_command = "./script/token.sh"
[env]
_.source = ["script/base.sh", { path = "script/local.sh", redact = true }]
"#;
        let analysis = analyze(source).expect("mise config");
        assert!(!analysis.unresolved);
        assert!(!analysis.environment_weakening);
        for command in [
            "cargo check",
            "cargo check --features windows",
            "bash -c true",
            "source quality/check.sh",
            "./script/enter.sh",
            "./script/install.sh",
            "zsh -c true",
            "cargo fmt",
            "corepack enable",
            "just check",
            "./script/token.sh",
            "source script/base.sh",
            "source script/local.sh",
        ] {
            assert!(analysis.commands.iter().any(|candidate| candidate == command), "missing {command:?}");
        }
    }

    #[test]
    fn file_task_execution_metadata_fails_closed_after_toml_decoding() {
        for source in [
            "#MISE env={RUSTFLAGS='-A warnings'}\ncargo clippy\n",
            "#MISE \"e\\u006ev\"={RUSTFLAGS='-A warnings'}\ncargo clippy\n",
            "# [MISE] env.RUSTFLAGS='-A warnings'\ncargo clippy\n",
            "//MISE env={PATH='quality/bin'}\nconsole.log('check')\n",
            "#MISE tools={\n#MISE rust='nightly'\n#MISE }\ncargo clippy\n",
            "# [MISE] cache.command_inputs=['quality/key.sh']\ncargo clippy\n",
            "//MISE \"d\\u0069r\"='../outside'\nconsole.log('check')\n",
            "#MISE depends=[\n#MISE 'setup'\n#MISE ]\ncargo clippy\n",
            "#MISE description=\"{{ \\u0065xec(command='quality/helper') }}\"\ncargo clippy\n",
            "#MISE alias=\"\"\"\n#MISE {{ read_file (\n#MISE path='quality/helper'\n#MISE ) }}\n#MISE \"\"\"\ncargo clippy\n",
            "#MISE description=\"{% if \\u0065xec(command='quality/helper') %}check{% endif %}\"\ncargo clippy\n",
            "#MISE alias=\"\"\"\n#MISE {% if read_file (\n#MISE path='quality/helper'\n#MISE ) %}check{% endif %}\n#MISE \"\"\"\ncargo clippy\n",
        ] {
            assert!(file_task_metadata_is_unresolved(source), "{source}");
        }
        for prefix in FILE_TASK_USAGE_PREFIXES {
            let source = format!("{prefix} mount \"quality/generate-spec\"\ncargo clippy\n");
            assert!(file_task_metadata_is_unresolved(&source), "{source}");
        }
        assert!(!file_task_metadata_is_unresolved(
            "#MISE description='Check quality'\n# [MISE] alias='check'\n//MISE quiet=true\ncargo clippy\n"
        ));
        assert!(!file_task_metadata_is_unresolved("# MISE env={RUSTFLAGS='-A warnings'}\ncargo clippy\n"));
    }

    #[test]
    fn malformed_or_unbounded_mise_execution_fails_closed() {
        assert!(analyze("[tasks.check]\nrun = 42\n").expect("mise config").unresolved);
        assert!(analyze("[tasks.check]\nfile = ['quality/check.sh']\n").expect("mise config").unresolved);
        assert!(analyze("[tasks.check]\nfile = '{{ env.TASK_FILE }}'\n").expect("mise config").unresolved);
        assert!(analyze("[tasks.check]\nfile = 42\n").expect("mise config").unresolved);
        assert!(analyze("[tasks.check]\nfile = '../quality/check.sh'\n").expect("mise config").unresolved);
        assert!(analyze("[task_config]\nincludes = ['tasks.toml']\n").expect("mise config").unresolved);
        assert!(analyze("[task_config]\ndir = '../outside'\n").expect("mise config").unresolved);
        for field in ["depends", "depends_post", "wait_for"] {
            let source = format!("[tasks.check]\nrun = 'cargo clippy'\n{field} = ['RUSTFLAGS=-A setup']\n");
            assert!(analyze(&source).expect("mise config").unresolved, "{field}");
        }
        for run in [
            "[{ task = 'lint', env = { RUSTFLAGS = '-A warnings' } }]",
            "[{ task = 'lint', args = ['--', '-A', 'warnings'] }]",
        ] {
            assert!(analyze(&format!("[tasks.check]\nrun = {run}\n")).expect("mise config").unresolved, "{run}");
        }
        assert!(analyze("[tasks.check]\nrun = './quality/check.sh'\ndir = '../outside'\n").expect("mise config").unresolved);
        assert!(
            analyze("[tasks.check]\nrun = 'cargo clippy'\nusage = 'complete \"plugin\" run=\"quality/generate-spec\"'\n")
                .expect("mise config")
                .unresolved
        );
        for source in [
            "env_file = '.env'\n",
            "dotenv = '.env'\n",
            "env_path = 'quality/bin'\n",
            "[vars._]\nsource = 'quality/vars.sh'\n",
            "[tasks.check]\nrun = 'cargo clippy'\ntools = { rust = 'nightly' }\n",
            "[tasks.check]\nrun = 'cargo clippy'\ncache = { command_inputs = ['quality/key.sh'] }\n",
            "[tasks.check]\nrun = 'cargo clippy'\ncache = true\n",
            "[tasks.check]\nrun = 'cargo clippy'\nsources = ['src/**']\noutputs = ['target/check']\n",
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
            "[tools]\ncargo = 'path:target/fake'\n",
            "[tools.cargo]\nversion = 'path:target/fake'\n",
            "[tools.cargo]\nversion = ['1.97.0', 'PATH:target/fake']\n",
            "[settings]\ntrusted_config_paths = ['../outside']\n",
            "[settings]\nshorthands_file = '../outside.toml'\n",
        ] {
            assert!(analyze(source).expect("mise config").unresolved, "{source}");
        }
        assert!(
            analyze("[task_templates.quality]\nrun = 'cargo clippy -- -A warnings'\n[tasks.check]\nextends = 'quality'\n")
                .expect("mise config")
                .unresolved
        );
        assert!(analyze("[env]\n_.path = { value = 'quality/bin' }\n").expect("mise config").unresolved);
        for directive in ["file", "dotenv", "python"] {
            let analysis = analyze(&format!("[env]\n_.{directive} = 'quality/environment'\n")).expect("mise config");
            assert!(analysis.environment_weakening, "{directive}");
            assert!(analysis.unresolved, "{directive}");
        }
        assert!(analyze("[env]\nTOKEN = '{{ exec(command=\"helper\") }}'\n").expect("mise config").unresolved);
        assert!(analyze("[env]\nTOKEN = \"{{ \\u0065xec(command='helper') }}\"\n").expect("mise config").unresolved);
        assert!(
            analyze("[env]\nTOKEN = \"{% if \\u0065xec(command='helper') %}set{% endif %}\"\n")
                .expect("mise config")
                .unresolved
        );
        assert!(
            analyze("[env]\nTOKEN = \"\"\"{{ read_file (\npath='quality/helper'\n) }}\"\"\"\n")
                .expect("mise config")
                .unresolved
        );
        assert!(
            analyze("[env]\nTOKEN = \"\"\"{% if read_file (\npath='quality/helper'\n) %}set{% endif %}\"\"\"\n")
                .expect("mise config")
                .unresolved
        );
        assert!(analyze("not = [valid").is_err());
    }
}
