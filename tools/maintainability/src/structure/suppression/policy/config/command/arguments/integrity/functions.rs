use std::collections::BTreeSet;

use super::{is_rust_tool_token, tokens, tool_basename};

pub(super) fn declares_required_command_override(source: &str, case_insensitive_tools: bool) -> bool {
    tokens::shell_function_definitions(source)
        .iter()
        .any(|definition| is_required_command(&definition.name, case_insensitive_tools))
}

pub(super) fn quality_function_names(source: &str, case_insensitive_tools: bool) -> BTreeSet<String> {
    let definitions = tokens::shell_function_definitions_in_active_source(source);
    let mut names = definitions
        .iter()
        .filter(|definition| super::contains_quality_command(&definition.body, case_insensitive_tools))
        .map(|definition| definition.name.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let discovered = definitions
            .iter()
            .filter(|definition| !names.contains(&definition.name))
            .filter(|definition| body_calls_function(&definition.body, &names))
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>();
        if discovered.is_empty() {
            return names;
        }
        names.extend(discovered);
    }
}

pub(super) fn has_unparsed_definition(source: &str) -> bool {
    tokens::active_source_has_unsupported_shell_function(source)
}

pub(super) fn command_calls(command: &[String], names: &BTreeSet<String>) -> bool {
    let Some(index) = super::executable_index(command) else {
        return false;
    };
    let executable = &command[index];
    let arguments = &command[index + 1..];
    let executable_name = tool_basename(executable).to_ascii_lowercase();
    match super::super::references::wrapper::select(executable, &executable_name, arguments) {
        super::super::references::wrapper::Selection::NotWrapper => {}
        super::super::references::wrapper::Selection::NoCommand => return false,
        super::super::references::wrapper::Selection::Nested(nested) => return command_calls(nested, names),
        super::super::references::wrapper::Selection::Opaque => return command_calls(arguments, names),
    }
    let name = executable.trim_matches(['(', ')', '{', '}']);
    !name.contains(['/', '\\']) && names.contains(name)
}

pub(super) fn source_calls(source: &str, names: &BTreeSet<String>) -> bool {
    tokens::source_command_tokens(source).iter().any(|command| command_calls(command, names))
}

fn body_calls_function(body: &str, names: &BTreeSet<String>) -> bool {
    source_calls(body, names)
}

fn is_required_command(name: &str, case_insensitive_tools: bool) -> bool {
    is_rust_tool_token(name, case_insensitive_tools) || tool_basename(name).eq_ignore_ascii_case("just")
}

#[cfg(test)]
mod tests {
    use super::{declares_required_command_override, has_unparsed_definition, quality_function_names};

    #[test]
    fn required_command_functions_cannot_replace_executables() {
        assert!(declares_required_command_override("cargo() { :; }", true));
        assert!(declares_required_command_override("just() { :; }", true));
        assert!(declares_required_command_override("just() ( : )", true));
        let multiline = format!("cargo()\n{}\n    :\n{}", '{', '}');
        assert!(declares_required_command_override(&multiline, true));
        assert!(declares_required_command_override("function cargo { return }", true));
        assert!(!declares_required_command_override("printf '%s\n' 'cargo() {'", true));
        assert!(!declares_required_command_override("function report { return }", true));
    }

    #[test]
    fn non_brace_function_bodies_fail_closed() {
        assert!(has_unparsed_definition("gate()\n(\n just check-quality\n)\n"));
        assert!(!has_unparsed_definition("gate()\n{\n just check-quality\n}\n"));
        assert!(!has_unparsed_definition("gate() {\n count=${#values[@]}\n value=${name#prefix}\n just check-quality\n}\n"));
    }

    #[test]
    fn quality_function_names_include_transitive_wrappers() {
        let source = "inner() {\n value=${name#prefix}\n just check-quality\n}\nouter()\n{\n inner\n true\n}\n";
        let names = quality_function_names(source, true);
        assert!(names.contains("inner"));
        assert!(names.contains("outer"));
        assert!(!quality_function_names("report() { printf '%s' cargo; }\n", true).contains("report"));
    }
}
