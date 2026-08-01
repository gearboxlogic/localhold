use super::path;

pub(super) fn dispatch_is_opaque(command: &str, arguments: &[String]) -> bool {
    output_redirection_targets_surface(arguments)
        || match command {
            "cp" | "cp.exe" | "install" | "install.exe" | "ln" | "ln.exe" | "mv" | "mv.exe" | "tee" | "tee.exe" | "truncate" | "truncate.exe" => {
                arguments_reference_surface(arguments)
            }
            "copy" | "copy-item" | "move" | "move-item" | "set-content" | "add-content" | "out-file" => arguments_reference_surface(arguments),
            "dd" | "dd.exe" => arguments.iter().filter_map(|argument| argument.strip_prefix("of=")).any(is_literal_execution_surface),
            "perl" | "perl.exe" if arguments.iter().any(|argument| argument.starts_with("-i")) => arguments_reference_surface(arguments),
            "sed" | "sed.exe"
                if arguments
                    .iter()
                    .any(|argument| argument == "-i" || argument.starts_with("--in-place") || argument.starts_with("-i")) =>
            {
                arguments_reference_surface(arguments)
            }
            _ => false,
        }
}

fn arguments_reference_surface(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| is_literal_execution_surface(argument))
}

fn output_redirection_targets_surface(arguments: &[String]) -> bool {
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let Some(target) = output_redirection_target(argument) else {
            continue;
        };
        let target = if target.is_empty() { arguments.next().map_or("", String::as_str) } else { target };
        if is_literal_execution_surface(target) {
            return true;
        }
    }
    false
}

fn output_redirection_target(argument: &str) -> Option<&str> {
    let operator = argument.trim_start_matches(|character: char| character.is_ascii_digit());
    operator.strip_prefix(">>").or_else(|| operator.strip_prefix(">|")).or_else(|| operator.strip_prefix('>'))
}

fn is_literal_execution_surface(candidate: &str) -> bool {
    path::normalize_literal(candidate).is_some_and(|path| super::super::super::is_execution_surface(&path))
}

#[cfg(test)]
mod tests {
    use super::dispatch_is_opaque;

    fn opaque(command: &str, arguments: &[&str]) -> bool {
        dispatch_is_opaque(command, &arguments.iter().map(|argument| (*argument).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn mutation_of_literal_execution_surfaces_fails_closed() {
        assert!(opaque("cp", &["quality/lint.data", "Justfile"]));
        assert!(opaque("mv", &["quality/lint.data", ".justfile"]));
        assert!(opaque("install", &["quality/lint.data", "script/check.sh"]));
        assert!(opaque("sed", &["-i", "s/check/skip/", "mise.toml"]));
        assert!(opaque("dd", &["if=quality/lint.data", "of=.github/workflows/ci.yml"]));
        assert!(!opaque("cp", &["input.txt", "output.txt"]));
    }

    #[test]
    fn output_redirection_to_literal_execution_surfaces_fails_closed() {
        assert!(opaque("cat", &["quality/lint.data", ">", "Justfile"]));
        assert!(opaque("printf", &["replacement", ">script/check.sh"]));
        assert!(opaque("printf", &["replacement", "2>>", "quality/check.py"]));
        assert!(!opaque("printf", &["report", ">", "target/report.txt"]));
    }
}
