use super::path;

pub(super) fn dispatch_is_opaque(command: &str, arguments: &[String]) -> bool {
    output_redirection_targets_surface(arguments)
        || match command {
            "cp" | "cp.exe" | "install" | "install.exe" | "ln" | "ln.exe" | "mv" | "mv.exe" | "tee" | "tee.exe" | "truncate" | "truncate.exe" => {
                arguments_reference_surface(arguments)
            }
            "copy" | "copy-item" | "move" | "move-item" | "set-content" | "add-content" | "out-file" => arguments_reference_surface(arguments),
            "curl" | "curl.exe" => curl_output_targets_surface(arguments),
            "dd" | "dd.exe" => arguments.iter().filter_map(|argument| argument.strip_prefix("of=")).any(is_literal_execution_surface),
            "patch" | "patch.exe" => true,
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

fn curl_output_targets_surface(arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let target = if matches!(argument.as_str(), "-o" | "--output") {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else if let Some(target) = argument.strip_prefix("--output=") {
            Some(target)
        } else {
            curl_short_output_target(argument, arguments, &mut index)
        };
        if target.is_some_and(is_literal_execution_surface) {
            return true;
        }
        index += 1;
    }
    false
}

fn curl_short_output_target<'a>(argument: &'a str, arguments: &'a [String], index: &mut usize) -> Option<&'a str> {
    let options = argument.strip_prefix('-').filter(|options| !options.starts_with('-'))?;
    let output = options.find('o')?;
    let attached = &options[output + 1..];
    if !attached.is_empty() {
        return Some(attached);
    }
    *index += 1;
    arguments.get(*index).map(String::as_str)
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
        assert!(opaque("patch", &["Justfile", "quality/lint.patch"]));
        assert!(opaque("patch", &["<", "quality/lint.patch"]));
        assert!(!opaque("cp", &["input.txt", "output.txt"]));
    }

    #[test]
    fn curl_output_to_literal_execution_surfaces_fails_closed() {
        for arguments in [
            &["--silent", "--output", "Justfile", "file:///tmp/payload"][..],
            &["--output=script/check.sh", "file:///tmp/payload"],
            &["-o", ".github/workflows/ci.yml", "file:///tmp/payload"],
            &["-sSomise.toml", "file:///tmp/payload"],
        ] {
            assert!(opaque("curl", arguments), "{arguments:?}");
            assert!(opaque("curl.exe", arguments), "{arguments:?}");
        }
        assert!(!opaque("curl", &["--output", "target/report.txt", "https://example.invalid/report"]));
        assert!(!opaque("curl", &["--output-dir", "Justfile", "https://example.invalid/report"]));
    }

    #[test]
    fn output_redirection_to_literal_execution_surfaces_fails_closed() {
        assert!(opaque("cat", &["quality/lint.data", ">", "Justfile"]));
        assert!(opaque("printf", &["replacement", ">script/check.sh"]));
        assert!(opaque("printf", &["replacement", "2>>", "quality/check.py"]));
        assert!(!opaque("printf", &["report", ">", "target/report.txt"]));
    }
}
