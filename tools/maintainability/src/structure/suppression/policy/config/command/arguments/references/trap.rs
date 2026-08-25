use super::super::tokens;

pub(super) fn action_is_opaque(surface: super::ShellSurface<'_>, arguments: &[String]) -> bool {
    if super::super::is_directory_change_command(arguments) {
        return true;
    }
    let Some(first) = arguments.first().map(String::as_str) else {
        return false;
    };
    if first == "{" {
        return true;
    }
    let action = match first {
        "" | "-" | "-l" | "-p" => return false,
        "--" => match arguments.get(1).map(String::as_str) {
            Some("-" | "") | None => return false,
            Some(action) => action,
        },
        option if option.starts_with('-') => return true,
        action => action,
    };
    if ["$(", "`", "<(", ">("].iter().any(|syntax| action.contains(syntax)) {
        return true;
    }
    let commands = tokens::source_command_tokens(action);
    if commands.is_empty() {
        return !action.trim().is_empty();
    }
    commands.into_iter().any(|command| command_tree_is_opaque(surface, &command))
}

fn command_tree_is_opaque(surface: super::ShellSurface<'_>, command: &[String]) -> bool {
    let word = command.iter().find_map(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        (!word.is_empty() && !super::is_execution_input_prefix(word) && (!super::super::is_environment_assignment(word) || word.contains("$(") || word.contains('`')))
            .then_some(word)
    });
    if super::super::is_directory_change_command(command)
        || word.is_some_and(super::path::contains_dynamic_value)
        || command
            .iter()
            .any(|token| matches!(token.trim_matches(['(', ')', '{', '}']).to_ascii_lowercase().as_str(), "." | "source" | "source.exe"))
    {
        return true;
    }
    let (inputs, opaque) = super::execution_input_candidates(surface, command);
    opaque || !inputs.is_empty()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::action_is_opaque;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    fn opaque(values: &[&str]) -> bool {
        let functions = BTreeSet::from(["cleanup".to_owned()]);
        let surface = super::super::ShellSurface {
            path: "script/check.sh",
            mode: super::super::ShellMode {
                direct_program_paths: true,
                make_surface: false,
                argv: false,
            },
            functions: &functions,
            path_policy: None,
            review: super::super::ReviewState {
                git_wrappers: false,
                source: false,
            },
        };
        action_is_opaque(surface, &arguments(values))
    }

    #[test]
    fn static_cleanup_actions_are_distinct_from_command_dispatch() {
        assert!(!opaque(&["cleanup", "EXIT"]));
        assert!(!opaque(&["rm -f target/temporary", "EXIT"]));
        assert!(!opaque(&["-", "EXIT"]));
        assert!(!opaque(&["-p", "EXIT"]));
        assert!(opaque(&["rm -f \"$temporary\"", "EXIT"]));
        assert!(opaque(&["sh quality/lint.txt", "EXIT"]));
        assert!(opaque(&["source quality/lint.txt", "EXIT"]));
        assert!(opaque(&["cd quality", "DEBUG"]));
        assert!(opaque(&["{", "Set-Location", "quality", ";", "continue", "}"]));
        assert!(opaque(&["{", "if", "($true)", "{", "Set-Location", "quality", "}", ";", "continue", "}"]));
        assert!(opaque(&["cleanup \"$(sh quality/lint.txt)\"", "EXIT"]));
        assert!(opaque(&["$cleanup_command", "EXIT"]));
        assert!(opaque(&["--unknown", "EXIT"]));
    }
}
