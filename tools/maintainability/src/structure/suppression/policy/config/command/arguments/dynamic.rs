use std::collections::{BTreeMap, BTreeSet};

use super::{is_environment_assignment, is_rust_subcommand, is_rust_tool_token, is_shell_command_prefix, tokens, tool_basename};

mod builtins;
mod resolvers;

pub(super) fn has_rust_tool_assignment_flow(source: &str, case_insensitive_tools: bool) -> bool {
    let assigned_rust_tools = assigned_rust_tool_variables(source, case_insensitive_tools);
    if assigned_rust_tools.iter().any(|name| references_variable(source, name)) {
        return true;
    }
    let (computed, opaque_target) = builtins::assigned_variables(source);
    opaque_target || computed.iter().any(|name| references_variable(source, name)) || computed_variables_reach_rust_command(source, &computed, case_insensitive_tools)
}

pub(super) fn has_opaque_command_assignment_flow(path: &str, source: &str, source_is_reviewed: bool) -> bool {
    let (invoked, opaque_invocation) = invoked_dynamic_command_variables(path, source);
    if opaque_invocation {
        return true;
    }
    let assigned = assigned_command_variables(path, source, source_is_reviewed);
    if invoked.iter().any(|name| assigned.get(name).is_none_or(|opaque_assignment| *opaque_assignment)) {
        return true;
    }
    let (computed, opaque_target) = builtins::assigned_variables(source);
    opaque_target || computed.iter().any(|name| references_variable(source, name))
}

#[cfg(test)]
pub(super) fn opaque_command_assignment_names(path: &str, source: &str) -> BTreeSet<String> {
    let (mut invoked, opaque_invocation) = invoked_dynamic_command_variables(path, source);
    let assigned = assigned_command_variables(path, source, true);
    invoked.retain(|name| assigned.get(name).is_none_or(|opaque_assignment| *opaque_assignment));
    if opaque_invocation {
        invoked.insert("<opaque-invocation>".to_owned());
    }
    invoked
}

pub(super) fn has_reviewed_trusted_system_command(source: &str) -> bool {
    resolvers::has_trusted_system_command(source)
}

fn assigned_command_variables(path: &str, source: &str, source_is_reviewed: bool) -> BTreeMap<String, bool> {
    let mut assigned = BTreeMap::new();
    for command in tokens::source_command_tokens(source) {
        for word in &command {
            if let Some(name) = compound_assignment_name(word) {
                assigned.entry(name.to_owned()).and_modify(|existing| *existing = true).or_insert(true);
                continue;
            }
            let Some((name, value)) = assignment(word) else {
                continue;
            };
            let opaque = opaque_command_assignment(path, (name, value), &command, source, source_is_reviewed);
            assigned.entry(name.to_owned()).and_modify(|existing| *existing |= opaque).or_insert(opaque);
        }
    }
    assigned
}

fn compound_assignment_name(word: &str) -> Option<&str> {
    let word = word.trim_matches(['(', ')', '{', '}', ';']);
    let (target, _) = word.split_once('=')?;
    let appended = target.ends_with('+');
    let target = target.strip_suffix('+').unwrap_or(target);
    let name = target.split_once('[').map_or(target, |(name, _)| name);
    (appended || target != name).then_some(name).filter(|name| valid_variable_name(name))
}

fn opaque_command_assignment(path: &str, assignment: (&str, &str), command: &[String], source: &str, source_is_reviewed: bool) -> bool {
    let (name, value) = assignment;
    let value = value.trim_matches(['\'', '"']);
    if is_reviewed_literal_program_assignment(path, name, value) || is_reviewed_parameter_program_assignment(path, name, value, command) {
        return false;
    }
    if value.contains("$(") || value.contains('`') {
        return opaque_command_substitution(path, name, command, source, source_is_reviewed);
    }
    true
}

fn is_reviewed_literal_program_assignment(path: &str, name: &str, value: &str) -> bool {
    matches!(
        (path, name, value),
        ("script/run-maintainability-gate.sh", "sha256_command", "/usr/bin/sha256sum")
            | ("script/run-maintainability-gate.sh", "uname_command", "/usr/bin/uname")
            | ("script/run-maintainability-gate.sh", "bash_command", "/usr/bin/bash")
            | ("script/run-maintainability-gate.sh", "chmod_command", "/usr/bin/chmod")
            | ("script/run-maintainability-gate.sh", "cp_command", "/usr/bin/cp")
            | ("script/run-maintainability-gate.sh", "ln_command", "/usr/bin/ln")
            | ("script/run-maintainability-gate.sh", "mkdir_command", "/usr/bin/mkdir")
            | ("script/run-maintainability-gate.sh", "mktemp_command", "/usr/bin/mktemp")
            | ("script/run-maintainability-gate.sh", "rm_command", "/usr/bin/rm")
            | ("script/run-maintainability-gate.sh", "curl_command", "/usr/bin/curl" | "/mingw64/bin/curl.exe")
            | ("script/run-maintainability-gate.sh", "cygpath_command", "/usr/bin/cygpath")
            | (
                "script/run-maintainability-gate.sh",
                "vswhere_command",
                "/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
            )
            | ("script/test-postgres-smoke.sh", "container_cli", "docker" | "podman")
            | (".github/workflows/ci.yml", "binary", "target/x86_64-pc-windows-msvc/release/hold.exe")
    )
}

fn is_reviewed_parameter_program_assignment(path: &str, name: &str, value: &str, command: &[String]) -> bool {
    if matches!(
        (path, name, value),
        (
            "script/tests/test_maintainability_bootstrap.sh",
            "check",
            "$repository_root/script/check-maintainability-bootstrap.sh"
        ) | (
            "script/tests/test_maintainability_bootstrap.sh",
            "trusted_check",
            "$trusted_gate/script/check-maintainability-bootstrap.sh"
        ) | (
            ".github/workflows/release.yml",
            "binary",
            "extracted/localhold-${GITHUB_REF_NAME}-x86_64-unknown-linux-gnu/bin/hold"
        ) | (
            ".github/workflows/release-smoke.yml",
            "binary",
            "$PWD/extracted/localhold-${RELEASE_TAG}-x86_64-unknown-linux-gnu/bin/hold"
        ) | (".github/workflows/ci.yml", "archive_binary", "$RUNNER_TEMP/extracted/$archive_root/bin/hold")
    ) {
        return true;
    }
    let line = command.join(" ");
    matches!(
        (path, name, line.as_str()),
        (
            "script/run-source-safety.sh",
            "cargo_command",
            "readonly cargo_command=${LOCALHOLD_MAINTAINABILITY_CARGO:?maintainability bootstrap did not provide an absolute Cargo command}"
        ) | ("script/install.sh", "cargo_command", "cargo_command=${CARGO:-cargo}")
            | (
                "script/tests/test_maintainability_bootstrap.sh",
                "trusted_rustup_command",
                "trusted_rustup_command=${LOCALHOLD_MAINTAINABILITY_RUSTUP:-rustup}"
            )
            | (
                "script/run-maintainability-gate.sh",
                "rustup_executable",
                "rustup_executable=${LOCALHOLD_MAINTAINABILITY_RUSTUP:-}" | "rustup_executable=$candidate" | "rustup_executable=$downloaded_rustup"
            )
            | (
                "script/run-source-safety.sh",
                "cargo_clippy_command",
                "readonly cargo_clippy_command=${LOCALHOLD_MAINTAINABILITY_CARGO_CLIPPY:?maintainability bootstrap did not provide an absolute Cargo Clippy command}"
            )
            | (
                "script/run-source-safety.sh",
                "cargo_fmt_command",
                "readonly cargo_fmt_command=${LOCALHOLD_MAINTAINABILITY_CARGO_FMT:?maintainability bootstrap did not provide an absolute Cargo fmt command}"
            )
            | (
                "script/run-source-safety.sh" | "script/run-maintainability-gate.sh",
                "git_command",
                "readonly git_command=${LOCALHOLD_MAINTAINABILITY_GIT:?maintainability bootstrap did not provide an absolute Git command}"
            )
    )
}

fn opaque_command_substitution(path: &str, name: &str, command: &[String], source: &str, source_is_reviewed: bool) -> bool {
    if matches!(
        (path, name, command),
        ("script/claude-review.sh", "claude_binary", [assignment, option, tool, fallback, closing])
            if assignment == "claude_binary=$(command" && option == "-v" && tool == "claude" && fallback == "||" && closing == "true)"
    ) {
        return false;
    }
    let assignment_source = command.join(" ");
    let (substitutions, malformed) = tokens::command_substitution_commands(&assignment_source, true);
    let Some(substitution) = (!malformed && substitutions.len() == 1).then(|| &substitutions[0]) else {
        return true;
    };
    let substitution_commands = tokens::source_command_tokens(substitution);
    let Some(resolver_command) = (substitution_commands.len() == 1).then(|| &substitution_commands[0]) else {
        return true;
    };
    let Some(resolver) = resolver_command.first().filter(|resolver| !resolver.is_empty()) else {
        return false;
    };
    !resolvers::is_reviewed(&resolvers::Candidate {
        path,
        name,
        resolver,
        resolver_command,
        assignment_command: command,
        source,
        source_is_reviewed,
    })
}

fn assigned_rust_tool_variables(source: &str, case_insensitive_tools: bool) -> BTreeSet<String> {
    tokens::source_command_tokens(source)
        .into_iter()
        .flatten()
        .filter_map(|word| {
            let (name, value) = assignment(&word)?;
            let value = value.trim_matches(['\'', '"']);
            (is_rust_tool_token(value, case_insensitive_tools) || is_rust_subcommand(value)).then_some(name.to_owned())
        })
        .collect()
}

fn computed_variables_reach_rust_command(source: &str, assigned: &BTreeSet<String>, case_insensitive_tools: bool) -> bool {
    tokens::source_command_tokens(source).iter().any(|command| {
        let invokes_rust = command.iter().any(|word| is_rust_tool_token(tool_basename(word), case_insensitive_tools));
        invokes_rust
            && assigned
                .iter()
                .any(|name| command.iter().any(|word| references_named_variable(word, name, case_insensitive_tools)))
    })
}

fn references_named_variable(word: &str, name: &str, case_insensitive: bool) -> bool {
    let bytes = word.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        if !matches!(byte, b'$' | b'%' | b'!') {
            return false;
        }
        let mut start = index + 1;
        if *byte == b'$' && bytes.get(start) == Some(&b'{') {
            start += 1;
        }
        let mut end = start;
        while bytes.get(end).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') {
            end += 1;
        }
        let candidate = &word[start..end];
        !candidate.is_empty() && if case_insensitive { candidate.eq_ignore_ascii_case(name) } else { candidate == name }
    })
}

fn assignment(word: &str) -> Option<(&str, &str)> {
    let word = word.trim_matches(['(', ')', '{', '}', ';']);
    let (name, value) = word.split_once('=')?;
    let name = name.trim_start_matches('$');
    (!name.is_empty() && !value.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') && !name.as_bytes()[0].is_ascii_digit())
        .then_some((name, value))
}

fn references_variable(source: &str, name: &str) -> bool {
    tokens::source_command_tokens(source)
        .iter()
        .any(|command| dynamic_command_variable(command).is_some_and(|candidate| candidate.eq_ignore_ascii_case(name)))
}

fn invoked_dynamic_command_variables(path: &str, source: &str) -> (BTreeSet<String>, bool) {
    let mut invoked = BTreeSet::new();
    let mut opaque = false;
    collect_invoked_dynamic_commands_from_source(path, source, &mut invoked, &mut opaque);
    (invoked, opaque)
}

fn collect_invoked_dynamic_commands_from_source(path: &str, source: &str, invoked: &mut BTreeSet<String>, opaque: &mut bool) {
    let source = tokens::without_noncommand_shell_data(source);
    let (command_substitutions, malformed_commands) = tokens::command_substitution_commands(&source, true);
    let (process_substitutions, malformed_processes) = tokens::process_substitution_commands(&source);
    *opaque |= malformed_commands || malformed_processes;
    let substitutions = command_substitutions.iter().chain(&process_substitutions).map(String::as_str).collect::<BTreeSet<_>>();
    for command_source in std::iter::once(source.as_str()).chain(substitutions.iter().copied()) {
        let scrubbed = scrub_substitutions(command_source, &substitutions);
        for command in tokens::source_command_tokens(&scrubbed) {
            collect_invoked_dynamic_commands(path, &command, 0, invoked, opaque);
        }
    }
}

pub(super) fn scrub_substitutions(source: &str, substitutions: &BTreeSet<&str>) -> String {
    let mut scrubbed = source.to_owned();
    let mut substitutions = substitutions.iter().copied().collect::<Vec<_>>();
    substitutions.sort_unstable_by_key(|substitution| std::cmp::Reverse(substitution.len()));
    for substitution in substitutions {
        for delimited in [
            format!("$({substitution})"),
            format!("<({substitution})"),
            format!(">({substitution})"),
            format!("`{substitution}`"),
        ] {
            scrubbed = scrubbed.replace(&delimited, "reviewed-substitution");
        }
    }
    scrubbed
}

fn collect_invoked_dynamic_commands(path: &str, command: &[String], depth: usize, invoked: &mut BTreeSet<String>, opaque: &mut bool) {
    const MAX_WRAPPER_DEPTH: usize = 32;
    if depth == MAX_WRAPPER_DEPTH {
        *opaque = true;
        return;
    }
    if matches!(command, [redirection] if redirection.starts_with('<') && !redirection.starts_with("<<")) || matches!(command, [operator, _] if operator == "<") {
        return;
    }
    match dynamic_command(command) {
        DynamicCommand::Named(name) => {
            invoked.insert(name.to_owned());
            return;
        }
        DynamicCommand::Opaque(word) if reviewed_dynamic_command_reference(path, word) => return,
        DynamicCommand::Opaque(_) => {
            *opaque = true;
            return;
        }
        DynamicCommand::Static => {}
    }
    let Some((index, raw)) = static_command_position(command) else {
        return;
    };
    let command_name = tool_basename(raw.trim_matches(['(', ')', '{', '}'])).to_ascii_lowercase();
    match super::references::wrapper::select(raw, &command_name, &command[index + 1..]) {
        super::references::wrapper::Selection::Nested(nested) => collect_invoked_dynamic_commands(path, nested, depth + 1, invoked, opaque),
        super::references::wrapper::Selection::Opaque => *opaque = true,
        super::references::wrapper::Selection::NotWrapper | super::references::wrapper::Selection::NoCommand => {}
    }
}

fn static_command_position(command: &[String]) -> Option<(usize, &str)> {
    command.iter().enumerate().find_map(|(index, token)| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        (!word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word) && assignment(word).is_none()).then_some((index, token.as_str()))
    })
}

fn reviewed_dynamic_command_reference(path: &str, command: &str) -> bool {
    matches!(
        (path, command),
        (
            "script/tests/test_maintainability_bootstrap.sh",
            "$test_repository/script/check-maintainability-bootstrap.sh"
        ) | ("script/tests/test_claude_review.sh", "$repository_root/script/claude-review.sh")
            | (".github/workflows/gpu-release-gate.yml", "$root/bin/hold" | "$CUDA_RELEASE_ROOT/bin/hold")
            | (".github/workflows/ci.yml", "$RUNNER_TEMP/localhold/bin/hold")
    )
}

fn dynamic_command_variable(command: &[String]) -> Option<&str> {
    match dynamic_command(command) {
        DynamicCommand::Named(name) => Some(name),
        DynamicCommand::Static | DynamicCommand::Opaque(_) => None,
    }
}

enum DynamicCommand<'a> {
    Static,
    Named(&'a str),
    Opaque(&'a str),
}

fn dynamic_command(command: &[String]) -> DynamicCommand<'_> {
    if command.get(1).is_some_and(|token| token == "=") {
        return DynamicCommand::Static;
    }
    if command.iter().any(|token| token == "[[" || token.starts_with("((")) {
        return DynamicCommand::Static;
    }
    if command.windows(2).any(|words| words[0] == "command" && matches!(words[1].as_str(), "-v" | "-V")) {
        return DynamicCommand::Static;
    }
    let mut substitution_depth = 0_usize;
    let mut in_backticks = false;
    for token in command {
        let word = token.trim_matches(['(', ')']);
        if substitution_depth > 0 || in_backticks {
            tokens::update_substitution_state(token, &mut substitution_depth, &mut in_backticks);
            continue;
        }
        if word.is_empty() || is_shell_command_prefix(word) || is_environment_assignment(word) || assignment(word).is_some() {
            tokens::update_substitution_state(token, &mut substitution_depth, &mut in_backticks);
            continue;
        }
        if array_element_assignment(word) {
            return DynamicCommand::Static;
        }
        if !word.contains(['$', '%', '!', '`']) {
            return DynamicCommand::Static;
        }
        return variable_name(word).map_or(DynamicCommand::Opaque(word), |name| {
            if valid_variable_name(name) {
                DynamicCommand::Named(name)
            } else {
                DynamicCommand::Opaque(word)
            }
        });
    }
    DynamicCommand::Static
}

fn array_element_assignment(word: &str) -> bool {
    let Some((target, _)) = word.split_once("]=") else {
        return false;
    };
    let Some((name, index)) = target.split_once('[') else {
        return false;
    };
    valid_variable_name(name) && !index.is_empty()
}

fn variable_name(word: &str) -> Option<&str> {
    word.strip_prefix("${")
        .and_then(|word| word.strip_suffix('}'))
        .or_else(|| word.strip_prefix('$'))
        .or_else(|| word.strip_prefix('%').and_then(|word| word.strip_suffix('%')))
        .or_else(|| word.strip_prefix('!').and_then(|word| word.strip_suffix('!')))
}

fn valid_variable_name(name: &str) -> bool {
    !name.is_empty() && !name.as_bytes()[0].is_ascii_digit() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::{has_opaque_command_assignment_flow as production_opaque_command_assignment_flow, has_rust_tool_assignment_flow};

    fn has_opaque_command_assignment_flow(path: &str, source: &str) -> bool {
        production_opaque_command_assignment_flow(path, source, false)
    }

    #[test]
    fn referenced_rust_tool_assignments_fail_closed() {
        assert!(has_rust_tool_assignment_flow("tool=cargo; sub=clippy; flag=-A; $tool $sub -- $flag warnings", false));
        assert!(has_rust_tool_assignment_flow("set TOOL=cargo\n%TOOL% build", true));
        assert!(has_rust_tool_assignment_flow(
            "printf -v tool %b cargo; printf -v sub %b clippy; printf -v option %b A; \"$tool\" \"$sub\" -- \"-$option\" warnings",
            false
        ));
        assert!(has_rust_tool_assignment_flow("printf -v option %b A; cargo clippy -- \"-$option\" warnings", false));
        assert!(has_rust_tool_assignment_flow(
            "printf -v option %b A; cargo clippy -- \"$unrelated-$option\" warnings",
            false
        ));
        assert!(has_rust_tool_assignment_flow("IFS= read -r tool <<<cargo; \"$tool\" check", false));
        assert!(!has_rust_tool_assignment_flow("tool=cargo\nprintf '%s\\n' configured", false));
        assert!(!has_rust_tool_assignment_flow("cargo_name=cargo\nprintf '%s\\n' \"$cargo_name\"", false));
        assert!(!has_rust_tool_assignment_flow("tool=node; $tool application.js", false));
    }

    #[test]
    fn dynamic_command_assignment_profiles_are_exact() {
        assert!(has_opaque_command_assignment_flow("script/check.sh", "runner=$(cat quality/lint.txt); $runner"));
        assert!(has_opaque_command_assignment_flow("script/check.sh", "runner=`sed -n 1p quality/lint.txt`; ${runner}"));
        assert!(has_opaque_command_assignment_flow(
            "script/check-maintainability-bootstrap.sh",
            "resolver=/tmp/unreviewed; runner=$($resolver); $runner"
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/check.sh",
            "runner=$(cat quality/lint.txt); if [[ -n $PATH ]]; then $runner; fi"
        ));
        assert!(has_opaque_command_assignment_flow("script/check.sh", "runner=sh; \"$runner\" quality/lint.txt"));
        assert!(has_opaque_command_assignment_flow("script/check.sh", "tool=node; $tool application.js"));
        assert!(!has_opaque_command_assignment_flow(
            "script/check.sh",
            "kernel=$(/usr/bin/uname -s); if [[ $kernel == Linux || $kernel == MINGW* ]]; then true; fi"
        ));
        assert!(!has_opaque_command_assignment_flow(
            "script/claude-review.sh",
            "claude_binary=$(command -v claude || true); \"$claude_binary\""
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/check-maintainability-bootstrap.sh",
            "bash_command=$(trusted_system_command bash); \"$bash_command\" --version"
        ));
        assert!(!has_opaque_command_assignment_flow(
            "script/run-maintainability-gate.sh",
            "readonly bash_command=/usr/bin/bash; \"$bash_command\" --version"
        ));
        assert!(!has_opaque_command_assignment_flow(
            "script/tests/test_maintainability_bootstrap.sh",
            "repository_root=/reviewed; check=\"$repository_root/script/check-maintainability-bootstrap.sh\"; \"$check\""
        ));
        assert!(!has_opaque_command_assignment_flow(
            "script/tests/test_maintainability_bootstrap.sh",
            "trusted_gate=/reviewed; trusted_check=\"$trusted_gate/script/check-maintainability-bootstrap.sh\"; \"$trusted_check\""
        ));
        assert!(!has_opaque_command_assignment_flow(
            ".github/workflows/ci.yml",
            "binary=target/x86_64-pc-windows-msvc/release/hold.exe; \"$binary\" --version"
        ));
        assert!(!has_opaque_command_assignment_flow(
            ".github/workflows/ci.yml",
            "archive_binary=\"$RUNNER_TEMP/extracted/$archive_root/bin/hold\"; \"$archive_binary\" --version"
        ));
        assert!(!has_opaque_command_assignment_flow(
            ".github/workflows/release.yml",
            "binary=\"extracted/localhold-${GITHUB_REF_NAME}-x86_64-unknown-linux-gnu/bin/hold\"; \"$binary\" --version"
        ));
        assert!(!has_opaque_command_assignment_flow(
            ".github/workflows/release-smoke.yml",
            "binary=\"$PWD/extracted/localhold-${RELEASE_TAG}-x86_64-unknown-linux-gnu/bin/hold\"; \"$binary\" --version"
        ));
    }

    #[test]
    fn resolver_exceptions_require_current_source_profile() {
        let path = "script/run-maintainability-gate.sh";
        let source = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../").join(path)).expect("reviewed gate runner");
        assert!(!production_opaque_command_assignment_flow(path, &source, true));
        assert!(production_opaque_command_assignment_flow(path, &source, false));

        let start = source.find("authenticated_tool() {").expect("authenticated tool definition");
        let end = start + source[start..].find("\n}\n").expect("authenticated tool definition end") + 2;
        let definition = &source[start..end];
        for mutation in [
            source.replacen(definition, &format!("if false; then\n{definition}\nfi"), 1),
            source.replacen(definition, &format!("(\n{definition}\n)"), 1),
            format!("{}\n{definition}\n", source.replacen(definition, "", 1)),
        ] {
            assert!(production_opaque_command_assignment_flow(path, &mutation, false));
        }
    }

    #[test]
    fn opaque_command_assignment_flows_fail_closed() {
        assert!(has_opaque_command_assignment_flow(
            "script/run-maintainability-gate.sh",
            "runner=/usr/bin/bash; \"$runner\" quality/lint.txt"
        ));
        assert!(has_opaque_command_assignment_flow(
            ".github/workflows/ci.yml",
            "binary=target/x86_64-pc-windows-msvc/release/unreviewed.exe; \"$binary\" --version"
        ));
        assert!(has_opaque_command_assignment_flow(
            ".github/workflows/ci.yml",
            "archive_binary=\"$RUNNER_TEMP/extracted/$archive_root/bin/unreviewed\"; \"$archive_binary\" --version"
        ));
        assert!(has_opaque_command_assignment_flow(
            ".github/workflows/release.yml",
            "binary=\"extracted/localhold-${GITHUB_REF_NAME}-x86_64-unknown-linux-gnu/bin/unreviewed\"; \"$binary\" --version"
        ));
        assert!(has_opaque_command_assignment_flow(
            ".github/workflows/release-smoke.yml",
            "binary=\"$PWD/extracted/localhold-${RELEASE_TAG}-x86_64-unknown-linux-gnu/bin/unreviewed\"; \"$binary\" --version"
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/tests/test_maintainability_bootstrap.sh",
            "repository_root=/reviewed; check=/reviewed/script/check.sh; \"$check\""
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/tests/test_maintainability_bootstrap.sh",
            "repository_root=/reviewed; check=\"$repository_root/script/check.sh\"; \"$check\""
        ));
        assert!(has_opaque_command_assignment_flow("script/check.sh", "set -- ash quality/runner.rs; \"$@\""));
        assert!(has_opaque_command_assignment_flow("script/check.sh", "args=(ash quality/runner.rs); \"${args[@]}\""));
        assert!(!has_opaque_command_assignment_flow(
            "script/test-postgres-smoke.sh",
            "container_cli=docker; \"$container_cli\" --version"
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/test-postgres-smoke.sh",
            concat!("container_cli=$", "{SHELL_NAME:-ash}", "; \"$container_cli\" quality/runner.$suffix")
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/test-postgres-smoke.sh",
            "container_cli=docker; container_cli[0]=ash; suffix=rs; \"$container_cli\" quality/runner.$suffix"
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/test-postgres-smoke.sh",
            "container_cli=docker; local -n alias=container_cli; alias=ash; \"$container_cli\" --version"
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/check-maintainability-bootstrap.sh",
            concat!(
                "git_command=$(trusted_system_command git); git_command=$",
                "{SHELL_NAME:-ash}",
                "; \"$git_command\" quality/runner.$suffix"
            )
        ));
    }
}
