use std::collections::BTreeSet;
use std::path::Path;

use super::{
    dynamic_program, is_cargo_tool_token, is_environment_assignment, is_mise, is_normalized_manifest_path, is_python, is_yaml, matches_tool_name, normalized_source_for_surface,
    package_json, tokens, tool_basename,
};

mod argv;
mod cargo;
mod compiler;
mod editor;
mod git;
mod interpreter;
mod model;
mod mutation;
mod native;
mod package_scripts;
mod path;
mod program;
mod python;
mod sed;
pub(in crate::structure::suppression::policy::config::command::arguments) mod shell;
mod trap;
mod unknown;
mod workspace;
pub(super) mod wrapper;

pub(in crate::structure::suppression::policy::config::command) use workspace::{
    Analyzer as WorkspaceAnalyzer, Context as WorkspaceContext, execution_inputs_for_surface_in_workspace,
};
pub(in crate::structure::suppression::policy::config::command) fn cargo_manifest_paths_for_surface(path: &str, source: &str) -> (BTreeSet<String>, bool) {
    let case_insensitive_tools = true;
    if let Some(scripts) = package_json::script_commands(path, source) {
        let Ok(scripts) = scripts else {
            return (BTreeSet::new(), true);
        };
        return collect_cargo_manifest_paths(scripts.iter().map(String::as_str), case_insensitive_tools);
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        return collect_cargo_manifest_paths(embedded_commands.iter().map(String::as_str), case_insensitive_tools);
    }
    collect_cargo_manifest_paths(std::iter::once(source.as_str()).chain(embedded_commands.iter().map(String::as_str)), case_insensitive_tools)
}

pub(in crate::structure::suppression::policy::config::command) fn reviewed_command_profiles() -> BTreeSet<(&'static str, &'static str)> {
    mutation::reviewed_sources()
}

#[cfg(test)]
pub(in crate::structure::suppression::policy::config::command) fn execution_inputs_for_surface(path: &str, source: &str, source_is_reviewed: bool) -> model::ExecutionInputs {
    execution_inputs_for_surface_with_policy(path, source, source_is_reviewed, None)
}

fn execution_inputs_for_surface_with_policy(path: &str, source: &str, source_is_reviewed: bool, path_policy: Option<&mutation::PathPolicy>) -> model::ExecutionInputs {
    if let Some(inputs) = package_scripts::execution_inputs(path, source, source_is_reviewed, path_policy) {
        return inputs;
    }
    if is_python(path) {
        return python::execution_inputs_with_path_policy(path, source, path_policy);
    }
    if is_mise(path) {
        let Ok(analysis) = super::mise::analyze(source) else {
            return model::ExecutionInputs::from_paths((BTreeSet::new(), true));
        };
        let (paths, unresolved) = collect_execution_inputs_with_policy(analysis.commands.iter().map(String::as_str), true, path, source_is_reviewed, path_policy);
        return model::ExecutionInputs::from_paths((paths, unresolved || analysis.unresolved));
    }
    if Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ps1"))
    {
        let analysis = super::powershell::analyze_execution_commands(source);
        let mut inputs = model::ExecutionInputs::from_paths(collect_execution_inputs_with_policy(
            std::iter::once(analysis.commands.as_str()),
            true,
            path,
            source_is_reviewed,
            path_policy,
        ));
        inputs.unresolved |= !super::powershell::unresolved_is_reviewed(path, source_is_reviewed, &analysis);
        return inputs;
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        let powershell_commands = super::super::yaml::powershell_run_commands(path, &source);
        let mut powershell_unresolved = false;
        let commands = embedded_commands
            .iter()
            .map(|command| {
                if powershell_commands.contains(command) {
                    let analysis = super::powershell::analyze_execution_commands(command);
                    powershell_unresolved |= !super::powershell::unresolved_is_reviewed(path, source_is_reviewed, &analysis);
                    analysis.commands
                } else {
                    command.to_owned()
                }
            })
            .collect::<Vec<_>>();
        let mut inputs = model::ExecutionInputs::from_paths(collect_execution_inputs_with_policy(
            commands.iter().map(String::as_str),
            true,
            path,
            source_is_reviewed,
            path_policy,
        ));
        inputs.unresolved |= powershell_unresolved;
        return inputs;
    }
    if !supports_direct_program_paths(path) {
        return model::ExecutionInputs::from_paths((BTreeSet::new(), true));
    }
    let command_source = direct_command_source(path, &source);
    model::ExecutionInputs::from_paths(collect_execution_inputs_with_policy(
        std::iter::once(command_source.as_str()),
        true,
        path,
        source_is_reviewed,
        path_policy,
    ))
}

fn direct_command_source(path: &str, source: &str) -> String {
    let basename = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or_default().to_ascii_lowercase();
    if matches!(basename.as_str(), "makefile" | "gnumakefile")
        || Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "make" | "mk"))
    {
        return super::super::make::recipe_commands(source).join("\n");
    }
    if matches!(basename.as_str(), "justfile" | ".justfile")
        || Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("just"))
    {
        return source
            .lines()
            .filter(|line| line.chars().next().is_some_and(char::is_whitespace))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
    }
    source.to_owned()
}

fn collect_cargo_manifest_paths<'a>(sources: impl IntoIterator<Item = &'a str>, case_insensitive_tools: bool) -> (BTreeSet<String>, bool) {
    let mut paths = BTreeSet::new();
    let mut unresolved = false;
    for source in sources {
        collect_cargo_manifest_paths_from_source(source, case_insensitive_tools, &mut paths, &mut unresolved);
    }
    (paths, unresolved)
}

fn collect_cargo_manifest_paths_from_source(source: &str, case_insensitive_tools: bool, paths: &mut BTreeSet<String>, unresolved: &mut bool) {
    collect_cargo_manifest_paths_from_source_at_depth(source, case_insensitive_tools, paths, unresolved, 0);
}

fn collect_cargo_manifest_paths_from_source_at_depth(source: &str, case_insensitive_tools: bool, paths: &mut BTreeSet<String>, unresolved: &mut bool, depth: usize) {
    const MAX_NESTED_COMMAND_DEPTH: usize = 32;
    if depth == MAX_NESTED_COMMAND_DEPTH {
        *unresolved = true;
        return;
    }
    for tokens in tokens::normalized_shell_commands(source) {
        for nested in tokens.iter().filter(|token| token.chars().any(char::is_whitespace) && token.contains("--manifest-path")) {
            if nested == source {
                *unresolved = true;
            } else {
                collect_cargo_manifest_paths_from_source_at_depth(nested, case_insensitive_tools, paths, unresolved, depth + 1);
            }
        }
        let Some(tool_index) = tokens.iter().position(|token| is_manifest_capable_tool_token(token, case_insensitive_tools)) else {
            continue;
        };
        let arguments = &tokens[tool_index.saturating_add(1)..];
        let mut index = 0;
        while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
            let manifest = if argument == "--manifest-path" {
                index += 1;
                arguments.get(index).map(String::as_str)
            } else {
                argument.strip_prefix("--manifest-path=")
            };
            if let Some(manifest) = manifest {
                record_manifest_path(manifest, paths, unresolved);
            } else if argument == "--manifest-path" {
                *unresolved = true;
            }
            index += 1;
        }
    }
}

fn record_manifest_path(manifest: &str, paths: &mut BTreeSet<String>, unresolved: &mut bool) {
    if is_normalized_manifest_path(manifest) && !path::contains_dynamic_value(manifest) {
        paths.insert(manifest.to_owned());
    } else {
        *unresolved = true;
    }
}

fn is_manifest_capable_tool_token(token: &str, case_insensitive: bool) -> bool {
    is_cargo_tool_token(token, case_insensitive) || matches_tool_name(tool_basename(token), case_insensitive, &["cargo-clippy", "cargo-clippy.exe"])
}

#[cfg(test)]
fn collect_execution_inputs<'a>(sources: impl IntoIterator<Item = &'a str>, direct_program_paths: bool, path: &str, source_is_reviewed: bool) -> (BTreeSet<String>, bool) {
    collect_execution_inputs_with_policy(sources, direct_program_paths, path, source_is_reviewed, None)
}

fn collect_execution_inputs_with_policy<'a>(
    sources: impl IntoIterator<Item = &'a str>,
    direct_program_paths: bool,
    path: &str,
    source_is_reviewed: bool,
    path_policy: Option<&mutation::PathPolicy>,
) -> (BTreeSet<String>, bool) {
    let mut inputs = BTreeSet::new();
    let mut unresolved = false;
    let make_surface = matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or_default().to_ascii_lowercase().as_str(),
        "makefile" | "gnumakefile"
    );
    for source in sources {
        let normalized = direct_program_paths.then(|| tokens::without_noncommand_shell_data(source));
        let analyzed_source = normalized.as_deref().unwrap_or(source);
        let functions = tokens::declared_shell_functions_in_active_source(analyzed_source);
        let reviewed_git_wrappers = git::reviewed_shell_wrappers(path, source, source_is_reviewed);
        let surface = ShellSurface {
            path,
            mode: ShellMode {
                direct_program_paths,
                make_surface,
                argv: false,
            },
            functions: &functions,
            path_policy,
            review: ReviewState {
                git_wrappers: reviewed_git_wrappers,
                source: source_is_reviewed,
            },
        };
        let nested_surface = ShellSurface {
            mode: ShellMode {
                make_surface: false,
                ..surface.mode
            },
            ..surface
        };
        unresolved |= super::dynamic::has_opaque_command_assignment_flow(path, source, source_is_reviewed);
        unresolved |= super::untrusted_directory_change_with_quality_dispatcher(source, true);
        let scrubbed_source;
        let source = if direct_program_paths {
            unresolved |= tokens::has_executable_unquoted_heredoc(source);
            let normalized = normalized.as_deref().expect("direct command surface normalization");
            let (process_commands, opaque_processes) = tokens::process_substitution_commands(normalized);
            let (command_substitutions, opaque_commands) = tokens::command_substitution_commands(normalized, true);
            unresolved |= opaque_processes || opaque_commands;
            let substitutions = process_commands.iter().chain(&command_substitutions).map(String::as_str).collect::<BTreeSet<_>>();
            for command in process_commands.iter().chain(&command_substitutions) {
                let scrubbed = super::dynamic::scrub_substitutions(command, &substitutions);
                record_shell_source_inputs(nested_surface, &scrubbed, &mut inputs, &mut unresolved);
            }
            scrubbed_source = super::dynamic::scrub_substitutions(normalized, &substitutions);
            &scrubbed_source
        } else {
            source
        };
        record_shell_source_inputs(surface, source, &mut inputs, &mut unresolved);
    }
    (inputs, unresolved)
}

#[derive(Clone, Copy)]
struct ShellSurface<'a> {
    path: &'a str,
    mode: ShellMode,
    functions: &'a BTreeSet<String>,
    path_policy: Option<&'a mutation::PathPolicy>,
    review: ReviewState,
}

#[derive(Clone, Copy)]
struct ShellMode {
    direct_program_paths: bool,
    make_surface: bool,
    argv: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum ValueSemantics {
    Shell,
    Literal,
}

impl ValueSemantics {
    fn contains_dynamic(self, value: &str) -> bool {
        self == Self::Shell && path::contains_dynamic_value(value)
    }
}

impl ShellMode {
    const fn value_semantics(self) -> ValueSemantics {
        if self.argv { ValueSemantics::Literal } else { ValueSemantics::Shell }
    }
}

#[derive(Clone, Copy)]
struct ReviewState {
    git_wrappers: bool,
    source: bool,
}

struct DispatchInput<'a, 'surface> {
    surface: ShellSurface<'surface>,
    tokens: &'a [String],
    command_index: usize,
    raw_command_word: &'a str,
    command_word: &'a str,
    command_token: &'a str,
    command: String,
    arguments: &'a [String],
}

fn record_shell_source_inputs(surface: ShellSurface<'_>, source: &str, inputs: &mut BTreeSet<String>, unresolved: &mut bool) {
    for tokens in tokens::source_command_tokens(source) {
        if surface.mode.direct_program_paths && !surface.mode.make_surface && substitution_is_command_program(&tokens) {
            *unresolved = true;
        }
        record_execution_inputs_from_tokens(surface, &tokens, inputs, unresolved);
    }
}

fn substitution_is_command_program(tokens: &[String]) -> bool {
    let Some(position) = command_position(tokens) else {
        return false;
    };
    if !is_environment_assignment(position.word) && (position.word.contains("$(") || position.word.contains('`')) {
        return true;
    }
    let command = tool_basename(nested_command_token(position.word)).to_ascii_lowercase();
    matches!(
        wrapper::select(position.raw, &command, &tokens[position.index.saturating_add(1)..]),
        wrapper::Selection::Nested(nested) if substitution_is_command_program(nested)
    )
}

struct CommandPosition<'a> {
    index: usize,
    raw: &'a str,
    word: &'a str,
}

fn command_position(tokens: &[String]) -> Option<CommandPosition<'_>> {
    if tokens.iter().any(|token| token.starts_with("((")) && tokens.iter().any(|token| token.ends_with("))")) {
        return None;
    }
    let mut substitution_depth = 0;
    let mut in_backticks = false;
    let index = tokens.iter().position(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        if substitution_depth > 0 || in_backticks {
            tokens::update_substitution_state(token, &mut substitution_depth, &mut in_backticks);
            return false;
        }
        if is_environment_assignment(word) || is_compound_assignment(word) {
            tokens::update_substitution_state(token, &mut substitution_depth, &mut in_backticks);
            return false;
        }
        !word.is_empty() && !is_execution_input_prefix(word)
    })?;
    let raw = tokens[index].as_str();
    Some(CommandPosition {
        index,
        raw,
        word: raw.trim_matches(['(', ')', '{', '}']),
    })
}

fn is_compound_assignment(word: &str) -> bool {
    let Some((target, value)) = word.split_once('=') else {
        return false;
    };
    let target = target.strip_suffix('+').unwrap_or(target);
    let (name, indexed) = target
        .split_once('[')
        .map_or((target, false), |(name, index)| (name, !index.is_empty() && index.ends_with(']')));
    (indexed || value.starts_with('(')) && !name.is_empty() && !name.as_bytes()[0].is_ascii_digit() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn record_execution_inputs_from_tokens(surface: ShellSurface<'_>, tokens: &[String], inputs: &mut BTreeSet<String>, unresolved: &mut bool) {
    let (candidates, opaque) = execution_input_candidates(surface, tokens);
    *unresolved |= opaque;
    record_execution_inputs(candidates, surface.mode.value_semantics(), inputs, unresolved);
}

fn record_execution_inputs(candidates: Vec<&str>, semantics: ValueSemantics, inputs: &mut BTreeSet<String>, unresolved: &mut bool) {
    for candidate in candidates {
        if let Some(input) = path::normalize_literal_with_semantics(candidate, semantics) {
            inputs.insert(input);
        } else {
            *unresolved = true;
        }
    }
}

fn execution_input_candidates<'a>(surface: ShellSurface<'_>, tokens: &'a [String]) -> (Vec<&'a str>, bool) {
    let position = if surface.mode.argv {
        tokens.first().map(|program| CommandPosition {
            index: 0,
            raw: program,
            word: program,
        })
    } else {
        if matches!(tokens, [redirection] if redirection.starts_with('>')) {
            return (Vec::new(), true);
        }
        if matches!(tokens, [redirection] if redirection.starts_with('<') && !redirection.starts_with("<<")) || matches!(tokens, [operator, _] if operator == "<") {
            return (Vec::new(), false);
        }
        if let Some(body) = shell_function_declaration_body(tokens, surface.functions) {
            return (Vec::new(), !body.is_empty());
        }
        command_position(tokens)
    };
    let Some(position) = position else {
        return (Vec::new(), false);
    };
    let command_index = position.index;
    let raw_command_word = position.raw;
    let command_word = position.word;
    if !surface.mode.argv && raw_command_word.contains(['*', '?', '[', '{', '~']) && !matches!(raw_command_word, "[" | "[[" | "{") {
        return (Vec::new(), true);
    }
    if !surface.mode.argv && command_word.chars().any(char::is_whitespace) && (command_word.contains("$(") || command_word.contains('`')) {
        return (Vec::new(), false);
    }
    if !surface.mode.argv && command_word.contains("$'") {
        return (Vec::new(), true);
    }
    let command_token = if surface.mode.argv { command_word } else { nested_command_token(command_word) };
    let command_basename = tool_basename(command_token);
    let command = unknown::reviewed_dynamic_program(surface.path, command_basename, surface.review.git_wrappers)
        .unwrap_or(command_basename)
        .to_ascii_lowercase();
    let arguments = &tokens[command_index.saturating_add(1)..];
    if surface.mode.argv && !command_token.contains(['/', '\\']) && argv_shell_only_program(&command) {
        return (Vec::new(), true);
    }
    if !surface.mode.argv && command_token.starts_with('<') {
        return (Vec::new(), true);
    }
    let program_candidate = match path::select_program_with_semantics(command_token, surface.mode.direct_program_paths, surface.mode.value_semantics()) {
        path::ProgramPath::Literal(candidate) => Some(candidate),
        path::ProgramPath::Opaque => {
            return (Vec::new(), !mutation::reviewed_arguments(surface.path, surface.review.source, &command, arguments));
        }
        path::ProgramPath::NotPath => None,
    };
    let (mut candidates, opaque) = classify_execution_input(DispatchInput {
        surface,
        tokens,
        command_index,
        raw_command_word,
        command_word,
        command_token,
        command,
        arguments,
    });
    if let Some(candidate) = program_candidate
        && !candidates.contains(&candidate)
    {
        candidates.push(candidate);
    }
    (candidates, opaque)
}

fn classify_execution_input<'a>(input: DispatchInput<'a, '_>) -> (Vec<&'a str>, bool) {
    if input.command_word == ":" {
        return (Vec::new(), mutation_dispatch_is_opaque(input.surface, ":", input.arguments));
    }
    match wrapper::select(input.raw_command_word, &input.command, input.arguments) {
        wrapper::Selection::NotWrapper => {}
        wrapper::Selection::NoCommand => return (Vec::new(), false),
        wrapper::Selection::Nested(command) => return execution_input_candidates(input.surface, command),
        wrapper::Selection::Opaque => return (Vec::new(), true),
    }
    if input.surface.review.git_wrappers && git::wrapper_body_is_exact(input.command_word, input.arguments) {
        return (Vec::new(), false);
    }
    if mutation::reviewed_arguments(input.surface.path, input.surface.review.source, &input.command, input.arguments) {
        return (Vec::new(), false);
    }
    if mutation_dispatch_is_opaque(input.surface, &input.command, input.arguments)
        || compiler::dispatch_with_semantics(&input.command, input.arguments, input.surface.mode.value_semantics())
        || native::dispatch_with_semantics(&input.command, input.arguments, input.surface.mode.value_semantics())
    {
        return (Vec::new(), true);
    }
    dispatch_execution_input(input)
}

fn mutation_dispatch_is_opaque(surface: ShellSurface<'_>, command: &str, arguments: &[String]) -> bool {
    match (surface.mode.argv, surface.path_policy) {
        (true, Some(policy)) => mutation::argv_dispatch_with_path_policy(surface.path, command, arguments, policy),
        (true, None) => mutation::argv_dispatch_is_opaque(surface.path, command, arguments),
        (false, Some(policy)) => mutation::dispatch_with_path_policy(surface.path, surface.review.source, command, arguments, policy),
        (false, None) => mutation::dispatch_is_opaque(surface.path, surface.review.source, command, arguments),
    }
}

fn shell_function_declaration_body<'a>(tokens: &'a [String], functions: &BTreeSet<String>) -> Option<&'a [String]> {
    let brace = tokens.iter().position(|token| token == "{")?;
    let function_name = match &tokens[..brace] {
        [name] => name.strip_suffix("()")?,
        [name, parentheses] if parentheses == "()" => name,
        [name, open, close] if open == "(" && close == ")" => name,
        [keyword, name] if keyword == "function" => name.trim_end_matches("()"),
        [keyword, name, parentheses] if keyword == "function" && parentheses == "()" => name,
        _ => return None,
    };
    if !functions.contains(function_name) {
        return None;
    }
    Some(&tokens[brace + 1..])
}

fn dispatch_execution_input<'a>(input: DispatchInput<'a, '_>) -> (Vec<&'a str>, bool) {
    if input.surface.mode.argv && input.command_token.contains(['/', '\\']) && argv_shell_only_program(&input.command) {
        return dispatch_external_input(input);
    }
    let DispatchInput {
        surface,
        tokens,
        command_index,
        raw_command_word,
        command_word,
        command_token,
        command,
        arguments,
    } = input;
    let selected = match command.as_str() {
        "git_at" if surface.review.git_wrappers && raw_command_word == "git_at" => {
            return (Vec::new(), git::wrapper_call_is_opaque(arguments, true));
        }
        "git_checked" if surface.review.git_wrappers && raw_command_word == "git_checked" => {
            return (Vec::new(), git::wrapper_call_is_opaque(arguments, false));
        }
        _ if raw_command_word == command && surface.functions.contains(&command) => SelectedInput::None,
        "trap" if !command_word.contains(['/', '\\']) => {
            return (Vec::new(), trap::action_is_opaque(surface, arguments));
        }
        "alias" | "coproc" | "eval" | "fc" | "fc.exe" | "iex" | "invoke-expression" | "parallel" | "parallel.exe" | "trap" | "xargs" | "xargs.exe" => SelectedInput::Opaque,
        "enable" if program::bash_enable_loads_builtin(arguments) => SelectedInput::Opaque,
        "mapfile" | "readarray" if mapfile_callback_is_opaque(arguments, surface.mode.value_semantics()) => SelectedInput::Opaque,
        "source" | "source.exe" => shell::source_input(arguments),
        "bash" | "bash.exe" | "dash" | "dash.exe" | "fish" | "fish.exe" | "sh" | "sh.exe" | "zsh" | "zsh.exe" => shell::input(&command, arguments),
        _ if shell::is_additional(&command) => shell::input(&command, arguments),
        _ if dynamic_program::is_python_interpreter(&command) => interpreter::python_input(arguments),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => interpreter::powershell_input(arguments),
        "cmd" | "cmd.exe" | "command.com" => SelectedInput::Opaque,
        _ if dynamic_program::is_unanalyzed_interpreter(&command) => SelectedInput::Opaque,
        _ if program::dynamic_loader_is_opaque(&command) => SelectedInput::Opaque,
        _ => {
            return dispatch_external_input(DispatchInput {
                surface,
                tokens,
                command_index,
                raw_command_word,
                command_word,
                command_token,
                command,
                arguments,
            });
        }
    };
    selected_input_result(selected)
}

fn dispatch_external_input<'a>(input: DispatchInput<'a, '_>) -> (Vec<&'a str>, bool) {
    let DispatchInput {
        surface,
        tokens,
        command_index,
        command_token,
        command,
        arguments,
        ..
    } = input;
    let selected = match command.as_str() {
        "autoconf" | "autoconf.exe" | "awk" | "awk.exe" | "dbus-run-session" | "dpkg" | "dpkg.exe" | "gawk" | "gawk.exe" | "gio" | "gio.exe" | "m4" | "m4.exe" | "mawk"
        | "mawk.exe" | "nawk" | "nawk.exe" | "protoc" | "protoc.exe" | "rake" | "rake.exe" | "rsync" | "rsync.exe" | "run-parts" | "run-parts.exe" | "sqlite3" | "sqlite3.exe"
        | "wget" | "wget.exe" | "go" | "go.exe" => SelectedInput::Opaque,
        "find" | "find.exe" => {
            return (
                Vec::new(),
                find_command_action_is_opaque(surface.path, surface.review.source, &command, arguments, surface.mode.value_semantics()),
            );
        }
        "sed" | "sed.exe" => {
            return (
                Vec::new(),
                sed::program_with_semantics_is_opaque(surface.path, surface.review.source, &command, arguments, surface.mode.value_semantics()),
            );
        }
        "split" | "split.exe" if program::split_filter_with_semantics_is_opaque(arguments, surface.mode.value_semantics()) => SelectedInput::Opaque,
        "sort" | "sort.exe" if program::sort_compression_program_with_semantics_is_opaque(arguments, surface.mode.value_semantics()) => SelectedInput::Opaque,
        "tar" | "tar.exe" if program::tar_dispatch_with_semantics_is_opaque(arguments, surface.mode.value_semantics()) => SelectedInput::Opaque,
        "zip" | "zip.exe" if program::zip_test_command_with_semantics_is_opaque(arguments, surface.mode.value_semantics()) => SelectedInput::Opaque,
        "openssl" | "openssl.exe" if program::openssl_module_selection_with_semantics_is_opaque(arguments, surface.mode.value_semantics()) => SelectedInput::Opaque,
        "rg" | "rg.exe" | "ripgrep" | "ripgrep.exe" if program::ripgrep_preprocessor_with_semantics_is_opaque(arguments, surface.mode.value_semantics()) => SelectedInput::Opaque,
        "cargo" | "cargo.exe" if cargo::dispatch_with_semantics(arguments, surface.mode.value_semantics()) => SelectedInput::Opaque,
        "just" | "just.exe" if program::just_source_selection_with_semantics_is_opaque(arguments, surface.mode.value_semantics()) => SelectedInput::Opaque,
        "make" | "make.exe" | "gmake" | "gmake.exe" => {
            let (inputs, opaque) = makefile_inputs(arguments, surface.mode.value_semantics());
            return (inputs, opaque || make_environment_selection_is_opaque(&tokens[..command_index]));
        }
        "git" | "git.exe" => {
            let opaque = if surface.mode.argv {
                git::argv_dispatch_is_opaque(surface.path, arguments)
            } else {
                git::dispatch_is_opaque(surface.path, arguments)
            };
            return (Vec::new(), opaque);
        }
        _ if compiler::is_recognized(&command) => SelectedInput::None,
        _ if unknown::is_preclassified_command(surface.path, &command) => SelectedInput::None,
        _ => match path::select_program_with_semantics(command_token, surface.mode.direct_program_paths, surface.mode.value_semantics()) {
            path::ProgramPath::NotPath => return unknown::execution_inputs_with_semantics(command_token, arguments, surface.mode.value_semantics()),
            path::ProgramPath::Literal(candidate) => SelectedInput::Literal(candidate),
            path::ProgramPath::Opaque => SelectedInput::Opaque,
        },
    };
    selected_input_result(selected)
}

fn selected_input_result(selected: SelectedInput<'_>) -> (Vec<&str>, bool) {
    match selected {
        SelectedInput::None => (Vec::new(), false),
        SelectedInput::Literal(candidate) => (vec![candidate], false),
        SelectedInput::Opaque => (Vec::new(), true),
    }
}

fn is_execution_input_prefix(word: &str) -> bool {
    matches!(word, "!" | "if" | "then" | "elif" | "while" | "until" | "do") || word.starts_with('-')
}

fn argv_shell_only_program(command: &str) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    command.starts_with('-')
        || matches!(
            command,
            "!" | "[["
                | ":"
                | "break"
                | "case"
                | "cd"
                | "compgen"
                | "continue"
                | "do"
                | "done"
                | "elif"
                | "else"
                | "esac"
                | "exit"
                | "export"
                | "fi"
                | "for"
                | "if"
                | "in"
                | "local"
                | "mapfile"
                | "read"
                | "readarray"
                | "return"
                | "readonly"
                | "set"
                | "shift"
                | "source"
                | "then"
                | "trap"
                | "type"
                | "umask"
                | "unset"
                | "until"
                | "wait"
                | "while"
        )
}

fn nested_command_token(token: &str) -> &str {
    let token = token.rsplit_once("$(").map_or(token, |(_, command)| command);
    token.rsplit_once('`').map_or(token, |(_, command)| command)
}

fn supports_direct_program_paths(path: &str) -> bool {
    let path = Path::new(path);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    matches!(basename.to_ascii_lowercase().as_str(), "justfile" | ".justfile" | "makefile" | "gnumakefile")
        || path.extension().and_then(|extension| extension.to_str()).is_none_or(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "sh" | "bash" | "zsh" | "fish" | "ps1" | "cmd" | "bat" | "just" | "toml" | "txt"
            )
        })
}

#[derive(Clone, Copy)]
pub(super) enum SelectedInput<'a> {
    None,
    Literal(&'a str),
    Opaque,
}

fn find_command_action_is_opaque(path: &str, source_is_reviewed: bool, command: &str, arguments: &[String], semantics: ValueSemantics) -> bool {
    arguments
        .iter()
        .any(|argument| semantics.contains_dynamic(argument) || matches!(argument.as_str(), "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"))
        && !mutation::reviewed_arguments(path, source_is_reviewed, command, arguments)
}

fn mapfile_callback_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        semantics.contains_dynamic(argument)
            || argument
                .strip_prefix('-')
                .filter(|options| !options.starts_with('-'))
                .is_some_and(|options| options.contains('C'))
    })
}

fn makefile_inputs(arguments: &[String], semantics: ValueSemantics) -> (Vec<&str>, bool) {
    let mut inputs = Vec::new();
    let mut opaque = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if make_argument_is_opaque(argument, semantics) {
            opaque = true;
            index += 1;
            continue;
        }
        let expects_operand = matches!(argument.as_str(), "-f" | "--file" | "--makefile");
        let candidate = if expects_operand {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else {
            argument
                .strip_prefix("--file=")
                .or_else(|| argument.strip_prefix("--makefile="))
                .or_else(|| argument.strip_prefix("-f").filter(|candidate| !candidate.is_empty()))
        };
        match candidate {
            Some("-") => opaque = true,
            Some(candidate) => inputs.push(candidate),
            None if expects_operand => opaque = true,
            None => {}
        }
        index += 1;
    }
    (inputs, opaque)
}

fn make_argument_is_opaque(argument: &str, semantics: ValueSemantics) -> bool {
    semantics.contains_dynamic(argument)
        || matches!(argument, "-C" | "-E" | "--directory" | "--eval")
        || argument.starts_with("--directory=")
        || argument.starts_with("-C") && argument.len() > 2
        || argument.starts_with("-E") && argument.len() > 2
        || argument.starts_with("--eval=")
        || is_make_environment_selection(argument)
}

fn make_environment_selection_is_opaque(tokens: &[String]) -> bool {
    tokens.iter().any(|token| is_make_environment_selection(token))
}

fn is_make_environment_selection(token: &str) -> bool {
    token
        .split_once('=')
        .map(|(name, _)| name)
        .is_some_and(|name| matches!(name, ".SHELLFLAGS" | "GNUMAKEFLAGS" | "MAKEFILES" | "MAKEFLAGS" | "MFLAGS" | "SHELL"))
}

#[cfg(test)]
mod tests;
