use std::collections::BTreeSet;
use std::path::Path;

use super::analysis::Options as AnalysisOptions;
use super::formats::{is_just, is_mise, is_powershell, is_python, is_standalone_shell_surface, is_windows_command, is_yaml};
use super::{dynamic_program, ignored_just_recipe_failure, join_command_continuations, mise, package_json, powershell, python, tool_basename, weakening_token_with_options};

#[cfg(test)]
pub(in crate::structure::suppression::policy::config) fn weakening_token(source: &str) -> bool {
    weakening_token_with_options(source, AnalysisOptions::strict())
}

#[cfg(test)]
pub(in crate::structure::suppression::policy::config) fn weakening_token_for_surface(path: &str, source: &str) -> bool {
    weakening_token_for_surface_with_context(None, path, source, false)
}

#[derive(Clone, Copy)]
pub(in crate::structure::suppression::policy::config::command) struct FilesystemContext<'a> {
    workspace: &'a Path,
    execution_surfaces: &'a [String],
    tracked_paths: &'a BTreeSet<String>,
}

impl<'a> FilesystemContext<'a> {
    pub(in crate::structure::suppression::policy::config::command) const fn new(
        workspace: &'a Path,
        execution_surfaces: &'a [String],
        tracked_paths: &'a BTreeSet<String>,
    ) -> Self {
        Self {
            workspace,
            execution_surfaces,
            tracked_paths,
        }
    }
}

pub(in crate::structure::suppression::policy::config::command) fn weakening_token_for_surface_with_reviewed_source(
    filesystem_context: FilesystemContext<'_>,
    path: &str,
    source: &str,
    source_is_reviewed: bool,
) -> bool {
    weakening_token_for_surface_with_context(Some(filesystem_context), path, source, source_is_reviewed)
}

fn weakening_token_for_surface_with_context(filesystem_context: Option<FilesystemContext<'_>>, path: &str, source: &str, source_is_reviewed: bool) -> bool {
    if mise::file_task_metadata_is_unresolved(source) || dynamic_program::is_unanalyzed_path(path) {
        return true;
    }
    if is_python(path) && (python::has_opaque_process_arguments(path, source) || python_filesystem_write_is_opaque(filesystem_context, path, source)) {
        return true;
    }
    if is_powershell(path) && (powershell::has_constructed_rust_arguments(source) || powershell::has_unchecked_native_quality_command(source)) {
        return true;
    }
    if is_powershell(path) {
        let analysis = powershell::analyze_execution_commands(source);
        if !powershell::unresolved_is_reviewed(path, source_is_reviewed, &analysis) {
            return true;
        }
    }
    if is_mise(path) {
        let Ok(analysis) = mise::analyze(source) else {
            return true;
        };
        return analysis.unresolved
            || analysis
                .commands
                .iter()
                .any(|command| weakening_token_with_options(command, AnalysisOptions::strict().with_case_insensitive_tools()));
    }
    analyze_commands(path, source, source_is_reviewed, options_for(path))
}

fn options_for(path: &str) -> AnalysisOptions {
    let mut options = AnalysisOptions::strict().with_case_insensitive_tools();
    if is_powershell(path) {
        options = options.allow_backticks().ignore_function_definitions().ignore_shell_assignment_flow();
    }
    if is_python(path) || is_windows_command(path) {
        options = options.ignore_command_substitutions();
    }
    if is_python(path) {
        options = options.ignore_function_definitions();
    }
    options
}

fn analyze_commands(path: &str, source: &str, source_is_reviewed: bool, mut options: AnalysisOptions) -> bool {
    if is_just(path) && ignored_just_recipe_failure(source, options.case_insensitive_tools()) {
        return true;
    }
    if let Some(scripts) = package_json::script_commands(path, source) {
        let options = options.without_initial_errexit();
        return scripts.map_or(true, |scripts| {
            scripts.iter().any(|script| weakening_token_with_options(script, options.allow_just_interpolation()))
        });
    }
    if is_standalone_shell_surface(path) && !shebang_enables_errexit(source) {
        options = options.without_initial_errexit();
    }
    let source = normalized_source_for_surface(path, source);
    if is_yaml(path) {
        return weakening_yaml_commands(path, &source, source_is_reviewed, options);
    }
    let embedded_commands = super::super::yaml::run_commands(path, &source);
    weakening_token_with_options(&source, if is_just(path) { options } else { options.allow_just_interpolation() })
        || embedded_commands
            .iter()
            .any(|command| weakening_token_with_options(command, options.allow_just_interpolation()))
}

fn weakening_yaml_commands(path: &str, source: &str, source_is_reviewed: bool, options: AnalysisOptions) -> bool {
    let powershell_commands = super::super::yaml::powershell_run_commands(path, source);
    if powershell_commands.iter().any(|command| {
        let analysis = powershell::analyze_execution_commands(command);
        powershell::has_unchecked_native_quality_command(command) || !powershell::unresolved_is_reviewed(path, source_is_reviewed, &analysis)
    }) {
        return true;
    }
    let powershell_options = options.allow_backticks().ignore_function_definitions().ignore_shell_assignment_flow();
    super::super::yaml::non_powershell_run_commands(path, source)
        .iter()
        .any(|command| weakening_token_with_options(command, options.allow_just_interpolation()) || powershell::has_constructed_rust_arguments(command))
        || powershell_commands
            .iter()
            .any(|command| weakening_token_with_options(command, powershell_options.allow_just_interpolation()) || powershell::has_constructed_rust_arguments(command))
}

fn python_filesystem_write_is_opaque(filesystem_context: Option<FilesystemContext<'_>>, path: &str, source: &str) -> bool {
    filesystem_context.map_or_else(
        || python::has_opaque_filesystem_write(path, source),
        |context| python::has_opaque_filesystem_write_in_workspace(context.workspace, context.execution_surfaces, context.tracked_paths, path, source),
    )
}

fn shebang_enables_errexit(source: &str) -> bool {
    let Some(interpreter) = source.lines().next().and_then(|line| line.strip_prefix("#!")) else {
        return false;
    };
    let words = interpreter.split_whitespace().collect::<Vec<_>>();
    let Some(shell_index) = words.iter().position(|word| matches!(tool_basename(word), "bash" | "dash" | "sh" | "zsh")) else {
        return false;
    };
    let arguments = &words[shell_index + 1..];
    arguments.iter().take_while(|word| **word != "--").any(|word| {
        word.strip_prefix('-')
            .is_some_and(|options| !options.starts_with('-') && !options.starts_with('o') && options.contains('e'))
    }) || arguments.windows(2).any(|pair| pair[0] == "-o" && pair[1].eq_ignore_ascii_case("errexit"))
}

pub(super) fn normalized_source_for_surface(path: &str, source: &str) -> String {
    match Path::new(path).extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("ps1") => powershell::normalize_escapes(source),
        Some(extension) if matches!(extension.to_ascii_lowercase().as_str(), "cmd" | "bat") => join_command_continuations(source),
        Some(extension) if extension.eq_ignore_ascii_case("py") => python::normalize_continuations(source),
        _ => source.to_owned(),
    }
}
