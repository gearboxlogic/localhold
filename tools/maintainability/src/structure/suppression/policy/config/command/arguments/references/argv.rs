use std::collections::BTreeSet;

use super::{ReviewState, ShellMode, ShellSurface, ValueSemantics, execution_input_candidates, model::ExecutionInputs, path, record_execution_inputs};

pub(super) enum Argument<'a> {
    Literal(&'a str),
    Unknown,
}

pub(super) fn execution_inputs_with_path_policy<'a>(
    path: &str,
    program: &str,
    arguments: impl IntoIterator<Item = Argument<'a>>,
    path_policy: Option<&super::mutation::PathPolicy>,
) -> ExecutionInputs {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    let uncertain = arguments.iter().any(|argument| matches!(argument, Argument::Unknown));
    let mut tokens = vec![program.to_owned()];
    tokens.extend(arguments.iter().filter_map(|argument| match argument {
        Argument::Literal(value) => Some((*value).to_owned()),
        Argument::Unknown => None,
    }));
    let functions = BTreeSet::new();
    let surface = ShellSurface {
        path,
        mode: ShellMode {
            direct_program_paths: true,
            make_surface: false,
            argv: true,
        },
        functions: &functions,
        path_policy,
        review: ReviewState {
            git_wrappers: false,
            source: false,
        },
    };
    let (candidates, mut unresolved) = if uncertain { (Vec::new(), true) } else { execution_input_candidates(surface, &tokens) };
    let mut inputs = BTreeSet::new();
    record_execution_inputs(candidates, ValueSemantics::Literal, &mut inputs, &mut unresolved);
    let mut windows_bare_programs = BTreeSet::new();
    if matches!(path::select_program_with_semantics(program, true, ValueSemantics::Literal), path::ProgramPath::NotPath) && !program.contains(['/', '\\']) {
        windows_bare_programs.insert(program.to_ascii_lowercase());
    }
    ExecutionInputs {
        paths: inputs,
        windows_bare_programs,
        unresolved,
    }
}
