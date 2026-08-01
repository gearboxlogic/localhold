use std::path::Path;

use super::path;

pub(super) fn dispatch_is_opaque(path: &str, command: &str, arguments: &[String]) -> bool {
    output_redirection_targets_surface(arguments)
        || match command {
            "cp" | "cp.exe" | "install" | "install.exe" | "ln" | "ln.exe" | "mv" | "mv.exe" | "tee" | "tee.exe" | "truncate" | "truncate.exe" => {
                arguments_reference_surface(arguments) || final_destination_is_opaque(path, arguments)
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

fn final_destination_is_opaque(path: &str, arguments: &[String]) -> bool {
    if arguments.iter().any(|argument| matches!(argument.as_str(), "--help" | "--version")) {
        return false;
    }
    explicit_target_directories(arguments)
        .chain(arguments.last().map(String::as_str))
        .any(|destination| !is_safe_literal_destination(destination) && !is_reviewed_dynamic_destination(path, destination))
}

fn explicit_target_directories(arguments: &[String]) -> impl Iterator<Item = &str> {
    arguments.iter().enumerate().filter_map(|(index, argument)| {
        if argument == "-t" {
            return Some(arguments.get(index + 1).map_or("", String::as_str));
        }
        if let Some(option) = argument.strip_prefix("--") {
            let (option, attached) = option.split_once('=').map_or((option, None), |(option, target)| (option, Some(target)));
            if !option.is_empty() && "target-directory".starts_with(option) {
                return Some(attached.unwrap_or_else(|| arguments.get(index + 1).map_or("", String::as_str)));
            }
        }
        argument.strip_prefix("-t").filter(|target| !target.is_empty())
    })
}

fn is_reviewed_dynamic_destination(path: &str, destination: &str) -> bool {
    // The bootstrap authenticates this complete test driver by SHA-256 before
    // executing it, so its isolated fixture destinations are reviewed as a set.
    (path == "script/tests/test_maintainability_bootstrap.sh" && path::contains_dynamic_value(destination)) || REVIEWED_DYNAMIC_DESTINATIONS.contains(&(path, destination))
}

// These surfaces intentionally mutate installation or isolated test/runtime
// trees. Keep each destination token exact so the exception cannot be reused
// by another surface or silently widened to a different target.
const REVIEWED_DYNAMIC_DESTINATIONS: &[(&str, &str)] = &[
    ("script/install.sh", "$bin_dir/hold"),
    ("script/install.sh", "$share_dir/localhold.example.toml"),
    ("script/install.sh", "$doc_dir/"),
    ("script/tests/test_claude_review.sh", "$test_root/bin/claude"),
    (".github/workflows/gpu-release-gate.yml", "$RUNNER_TEMP/hold-cuda"),
    (".github/workflows/gpu-release-gate.yml", "$moved"),
    (".github/workflows/gpu-release-gate.yml", "$dependency"),
];

fn is_safe_literal_destination(candidate: &str) -> bool {
    if let Some(path) = path::normalize_literal(candidate) {
        return !super::super::super::is_execution_surface(&path);
    }
    !path::contains_dynamic_value(candidate) && (Path::new(candidate).is_absolute() || is_windows_absolute(candidate))
}

fn is_windows_absolute(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\')
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
    use super::{REVIEWED_DYNAMIC_DESTINATIONS, dispatch_is_opaque};

    fn opaque(command: &str, arguments: &[&str]) -> bool {
        dispatch_is_opaque("script/check.sh", command, &arguments.iter().map(|argument| (*argument).to_owned()).collect::<Vec<_>>())
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
        assert!(opaque("cp", &["quality/lint.data", "$destination"]));
        assert!(opaque("cp", &["--target-directory", "$destination", "quality/lint.data"]));
        assert!(opaque("cp", &["--target-directory=$destination", "quality/lint.data"]));
        assert!(opaque("cp", &["--t=$destination", "quality/lint.data"]));
        assert!(opaque("cp", &["-t$destination", "quality/lint.data"]));
        assert!(opaque("cp", &["quality/lint.data", "../Justfile"]));
        assert!(!opaque("cp", &["input.txt", "output.txt"]));
        assert!(!opaque("cp", &["-ttarget/output", "input.txt"]));
        assert!(!opaque("cp", &["input.txt", "/tmp/output.txt"]));
    }

    #[test]
    fn reviewed_dynamic_destinations_are_path_specific() {
        for (path, destination) in REVIEWED_DYNAMIC_DESTINATIONS {
            let arguments = ["quality/lint.data".to_owned(), (*destination).to_owned()];
            assert!(!dispatch_is_opaque(path, "cp", &arguments), "{path}: {destination}");
            assert!(dispatch_is_opaque("script/check.sh", "cp", &arguments), "{path}: {destination}");
        }

        let changed = ["quality/lint.data".to_owned(), "$test_root/bin/Justfile".to_owned()];
        assert!(dispatch_is_opaque("script/tests/test_claude_review.sh", "ln", &changed));
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
