use super::super::tokens;

pub(super) fn action_is_opaque(path: &str, arguments: &[String], direct_program_paths: bool) -> bool {
    let Some(first) = arguments.first().map(String::as_str) else {
        return false;
    };
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
    commands.into_iter().any(|command| command_tree_is_opaque(path, &command, direct_program_paths))
}

fn command_tree_is_opaque(path: &str, command: &[String], direct_program_paths: bool) -> bool {
    let word = command.iter().find_map(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        (!word.is_empty() && !super::is_execution_input_prefix(word) && (!super::super::is_environment_assignment(word) || word.contains("$(") || word.contains('`')))
            .then_some(word)
    });
    if word.is_some_and(super::path::contains_dynamic_value)
        || command
            .iter()
            .any(|token| matches!(token.trim_matches(['(', ')', '{', '}']).to_ascii_lowercase().as_str(), "." | "source" | "source.exe"))
    {
        return true;
    }
    let (inputs, opaque) = super::execution_input_candidates(path, command, direct_program_paths);
    opaque || !inputs.is_empty()
}

#[cfg(test)]
mod tests {
    use super::action_is_opaque;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn static_cleanup_actions_are_distinct_from_command_dispatch() {
        assert!(!action_is_opaque("script/check.sh", &arguments(&["cleanup", "EXIT"]), true));
        assert!(!action_is_opaque("script/check.sh", &arguments(&["rm -f target/temporary", "EXIT"]), true));
        assert!(!action_is_opaque("script/check.sh", &arguments(&["-", "EXIT"]), true));
        assert!(!action_is_opaque("script/check.sh", &arguments(&["-p", "EXIT"]), true));
        assert!(action_is_opaque("script/check.sh", &arguments(&["rm -f \"$temporary\"", "EXIT"]), true));
        assert!(action_is_opaque("script/check.sh", &arguments(&["sh quality/lint.txt", "EXIT"]), true));
        assert!(action_is_opaque("script/check.sh", &arguments(&["source quality/lint.txt", "EXIT"]), true));
        assert!(action_is_opaque("script/check.sh", &arguments(&["cleanup \"$(sh quality/lint.txt)\"", "EXIT"]), true));
        assert!(action_is_opaque("script/check.sh", &arguments(&["$cleanup_command", "EXIT"]), true));
        assert!(action_is_opaque("script/check.sh", &arguments(&["--unknown", "EXIT"]), true));
    }
}
