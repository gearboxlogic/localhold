use std::path::Path;

use super::path;

mod output;

pub(super) fn dispatch_is_opaque(path: &str, command: &str, arguments: &[String]) -> bool {
    super::editor::is_command_capable(command)
        || output_redirection_is_opaque(path, arguments)
        || output::dispatch_is_opaque(path, command, arguments)
        || is_objcopy_command(command) && (arguments_reference_protected_inputs(arguments) || final_destination_is_opaque(path, arguments))
        || match command {
            "cp" | "cp.exe" | "install" | "install.exe" | "ln" | "ln.exe" | "mv" | "mv.exe" | "truncate" | "truncate.exe" => {
                arguments_reference_protected_inputs(arguments) || final_destination_is_opaque(path, arguments)
            }
            "link" | "unlink" => arguments_reference_protected_inputs(arguments) || final_destination_is_opaque(path, arguments),
            "tee" | "tee.exe" => tee_output_is_opaque(path, arguments),
            "copy" | "copy-item" | "move" | "move-item" | "set-content" | "add-content" | "out-file" => arguments_reference_protected_inputs(arguments),
            "brotli" | "brotli.exe" | "bunzip2" | "bunzip2.exe" | "bzip2" | "bzip2.exe" | "gzip" | "gzip.exe" | "gunzip" | "gunzip.exe" | "lz4" | "lz4.exe" | "pigz"
            | "pigz.exe" | "unlz4" | "unlz4.exe" | "unpigz" | "unpigz.exe" | "unxz" | "unxz.exe" | "unzstd" | "unzstd.exe" | "xz" | "xz.exe" | "zstd" | "zstd.exe" => {
                compression_dispatch_is_opaque(command, arguments)
            }
            "curl" | "curl.exe" => curl_output_is_opaque(path, arguments) || curl_remote_name_is_opaque(arguments),
            "dd" | "dd.exe" => arguments
                .iter()
                .filter_map(|argument| argument.strip_prefix("of="))
                .any(|destination| destination_is_opaque(path, destination)),
            "iconv" | "iconv.exe" => iconv_output_is_opaque(path, arguments),
            "jar" | "jar.exe" => jar_dispatch_is_opaque(arguments),
            "openssl" | "openssl.exe" => openssl_output_is_opaque(path, arguments),
            "patch" | "patch.exe" => true,
            "shuf" | "shuf.exe" => shuf_output_is_opaque(path, arguments),
            "unzip" | "unzip.exe" => unzip_dispatch_is_opaque(arguments),
            "perl" | "perl.exe" if arguments.iter().any(|argument| argument.starts_with("-i")) => arguments_reference_protected_inputs(arguments),
            "sed" | "sed.exe"
                if arguments
                    .iter()
                    .any(|argument| argument == "-i" || argument.starts_with("--in-place") || argument.starts_with("-i")) =>
            {
                arguments_reference_protected_inputs(arguments)
            }
            _ => false,
        }
}

fn compression_dispatch_is_opaque(command: &str, arguments: &[String]) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    let decompresses_by_default = matches!(command, "bunzip2" | "gunzip" | "unlz4" | "unpigz" | "unxz" | "unzstd");
    let mut decompresses = decompresses_by_default;
    let mut writes_to_stdout = false;
    let mut inspects_only = false;
    for argument in arguments.iter().take_while(|argument| argument.as_str() != "--") {
        if let Some(options) = argument.strip_prefix('-').filter(|options| !options.starts_with('-')) {
            decompresses |= options.contains('d');
            writes_to_stdout |= options.contains('c');
            inspects_only |= options.chars().any(|option| matches!(option, 'h' | 'l' | 'L' | 't' | 'V'));
            continue;
        }
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        decompresses |= long_option_matches(option, "--decompress", "--d") || long_option_matches(option, "--uncompress", "--u");
        writes_to_stdout |= long_option_matches(option, "--stdout", "--st") || long_option_matches(option, "--to-stdout", "--to-s");
        inspects_only |= matches!(option, "--help" | "--license" | "--list" | "--test" | "--version");
    }
    decompresses && !writes_to_stdout && !inspects_only
}

fn long_option_matches(option: &str, full: &str, minimum: &str) -> bool {
    option.len() >= minimum.len() && full.starts_with(option)
}

fn jar_dispatch_is_opaque(arguments: &[String]) -> bool {
    let mut consumes_operand = false;
    for argument in arguments {
        if consumes_operand {
            consumes_operand = false;
            continue;
        }
        if matches!(argument.as_str(), "--file" | "--main-class" | "--manifest" | "--module-version" | "--release") {
            consumes_operand = true;
            continue;
        }
        if argument == "--extract" {
            return true;
        }
        if argument == "--" || argument.starts_with("--") {
            continue;
        }
        if argument == "-f" {
            consumes_operand = true;
            continue;
        }
        if path::contains_dynamic_value(argument) {
            return true;
        }
        let options = argument.strip_prefix('-').unwrap_or(argument);
        if options.chars().all(is_jar_option) && options.chars().any(is_jar_operation) {
            return options.contains('x');
        }
    }
    false
}

const fn is_jar_operation(option: char) -> bool {
    matches!(option, 'c' | 'i' | 't' | 'u' | 'x')
}

const fn is_jar_option(option: char) -> bool {
    is_jar_operation(option) || matches!(option, '0' | 'C' | 'e' | 'f' | 'm' | 'M' | 'P' | 'v')
}

fn final_destination_is_opaque(path: &str, arguments: &[String]) -> bool {
    if arguments.iter().any(|argument| matches!(argument.as_str(), "--help" | "--version")) {
        return false;
    }
    explicit_target_directories(arguments)
        .chain(arguments.last().map(String::as_str))
        .any(|destination| destination_is_opaque(path, destination))
}

fn destination_is_opaque(path: &str, destination: &str) -> bool {
    !is_safe_literal_destination(destination) && !is_reviewed_dynamic_destination(path, destination)
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
    (path == "script/tests/test_maintainability_bootstrap.sh" && path::contains_dynamic_value(destination))
        || workflow_runner_temp_destination(path, destination)
        || REVIEWED_DYNAMIC_DESTINATIONS.contains(&(path, destination))
}

fn workflow_runner_temp_destination(path: &str, destination: &str) -> bool {
    path.starts_with(".github/workflows/")
        && ["$RUNNER_TEMP/", "${RUNNER_TEMP}/", "$env:RUNNER_TEMP/"]
            .iter()
            .any(|prefix| destination.strip_prefix(prefix).is_some_and(|suffix| path::normalize_literal(suffix).is_some()))
}

// These surfaces intentionally mutate installation or isolated test/runtime
// trees. Keep each destination token exact so the exception cannot be reused
// by another surface or silently widened to a different target.
const REVIEWED_DYNAMIC_DESTINATIONS: &[(&str, &str)] = &[
    ("script/install.sh", "$bin_dir/hold"),
    ("script/install.sh", "$share_dir/localhold.example.toml"),
    ("script/install.sh", "$doc_dir/"),
    ("script/tests/test_claude_review.sh", "$test_root/bin/claude"),
    ("script/tests/test_claude_review.sh", "$capture/args"),
    ("script/tests/test_claude_review.sh", "$capture/child-pid"),
    ("script/tests/test_claude_review.sh", "$capture/cwd"),
    ("script/tests/test_claude_review.sh", "$capture/environment"),
    ("script/tests/test_claude_review.sh", "$capture/ready"),
    ("script/tests/test_claude_review.sh", "$capture/signal"),
    ("script/tests/test_claude_review.sh", "$capture/temp-environment"),
    ("script/tests/test_claude_review.sh", "$test_root/failure-output"),
    ("script/tests/test_claude_review.sh", "$test_root/output"),
    ("script/tests/test_claude_review.sh", "$test_root/signal-output"),
    ("script/tests/test_claude_review.sh", "$timeout_marker"),
    ("script/tests/test_claude_review.sh", "$TMPDIR/nested/payload"),
    (".github/workflows/gpu-release-gate.yml", "$RUNNER_TEMP/hold-cuda"),
    (".github/workflows/gpu-release-gate.yml", "$GITHUB_ENV"),
    (".github/workflows/gpu-release-gate.yml", "$GITHUB_OUTPUT"),
    (".github/workflows/gpu-release-gate.yml", "$moved"),
    (".github/workflows/gpu-release-gate.yml", "$dependency"),
];

fn is_safe_literal_destination(candidate: &str) -> bool {
    if let Some(path) = path::normalize_literal(candidate) {
        return !super::super::super::is_protected_check_input(&path);
    }
    !path::contains_dynamic_value(candidate) && (Path::new(candidate).is_absolute() || is_windows_absolute(candidate))
}

fn is_windows_absolute(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\')
}

fn iconv_output_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let target = if argument == "-o" {
            index += 1;
            Some(arguments.get(index).map_or("", String::as_str))
        } else if let Some(target) = argument.strip_prefix("-o").filter(|target| !target.is_empty()) {
            Some(target)
        } else if let Some(option) = argument.strip_prefix("--") {
            let (option, attached) = option.split_once('=').map_or((option, None), |(option, target)| (option, Some(target)));
            (!option.is_empty() && "output".starts_with(option)).then(|| attached.unwrap_or_else(|| arguments.get(index + 1).map_or("", String::as_str)))
        } else {
            None
        };
        if target.is_some_and(|target| destination_is_opaque(path, target)) {
            return true;
        }
        index += 1;
    }
    false
}

fn openssl_output_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        if matches!(argument.as_str(), "-out" | "-keyout" | "-writerand" | "-CAserial") {
            index += 1;
            if destination_is_opaque(path, arguments.get(index).map_or("", String::as_str)) {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn shuf_output_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let target = if matches!(argument.as_str(), "-o" | "--output") {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else if let Some(target) = argument.strip_prefix("--output=") {
            Some(target)
        } else {
            short_output_target(argument, arguments, &mut index)
        };
        if target.is_some_and(|target| destination_is_opaque(path, target)) {
            return true;
        }
        index += 1;
    }
    false
}

fn is_objcopy_command(command: &str) -> bool {
    let stem = command.strip_suffix(".exe").unwrap_or(command);
    let unversioned = stem
        .rsplit_once('-')
        .filter(|(_, suffix)| suffix.chars().all(|character| character.is_ascii_digit() || character == '.'))
        .map_or(stem, |(prefix, _)| prefix);
    unversioned == "objcopy" || unversioned.ends_with("-objcopy")
}

fn unzip_dispatch_is_opaque(arguments: &[String]) -> bool {
    unzip_sets_archive_timestamp(arguments) || unzip_extracts_files(arguments) && !unzip_extracts_into_isolated_directory(arguments)
}

fn unzip_sets_archive_timestamp(arguments: &[String]) -> bool {
    unzip_short_options(arguments).any(|options| options.contains('T'))
}

fn unzip_extracts_files(arguments: &[String]) -> bool {
    !arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        matches!(argument.as_str(), "--help" | "--version")
            || unzip_short_option_flags(argument).is_some_and(|options| options.chars().any(|option| matches!(option, 'c' | 'h' | 'l' | 'p' | 't' | 'v' | 'z' | 'Z')))
    })
}

fn unzip_extracts_into_isolated_directory(arguments: &[String]) -> bool {
    if unzip_short_options(arguments).any(|options| options.contains(':')) {
        return false;
    }
    let mut isolated = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let destination = unzip_directory_option(argument).and_then(|attached| {
            if attached.is_empty() {
                index += 1;
                arguments.get(index).map(String::as_str)
            } else {
                Some(attached)
            }
        });
        if let Some(destination) = destination {
            if !isolated_unzip_directory(destination) {
                return false;
            }
            isolated = true;
        }
        index += 1;
    }
    isolated
}

fn unzip_short_options(arguments: &[String]) -> impl Iterator<Item = &str> {
    arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .filter_map(|argument| unzip_short_option_flags(argument))
}

fn unzip_short_option_flags(argument: &str) -> Option<&str> {
    let options = argument.strip_prefix('-').filter(|options| !options.starts_with('-'))?;
    let end = options.find(['P', 'd']).unwrap_or(options.len());
    Some(&options[..end])
}

fn unzip_directory_option(argument: &str) -> Option<&str> {
    let options = argument.strip_prefix('-').filter(|options| !options.starts_with('-'))?;
    let directory = options.find('d')?;
    if options.find('P').is_some_and(|password| password < directory) {
        return None;
    }
    options.get(directory + 1..)
}

fn isolated_unzip_directory(destination: &str) -> bool {
    if path::normalize_literal(destination).as_deref() == Some("extracted") {
        return true;
    }
    ["$RUNNER_TEMP/", "${RUNNER_TEMP}/"]
        .iter()
        .any(|prefix| destination.strip_prefix(prefix).is_some_and(|suffix| path::normalize_literal(suffix).is_some()))
}

fn curl_remote_name_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        if let Some(options) = argument.strip_prefix('-').filter(|options| !options.starts_with('-')) {
            return options.contains('O');
        }
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        ["--remote-name", "--remote-name-all"]
            .iter()
            .any(|full| option.len() >= "--remote-n".len() && full.starts_with(option))
    })
}

fn curl_output_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let target = if matches!(argument.as_str(), "-o" | "--output") {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else if let Some(target) = argument.strip_prefix("--output=") {
            Some(target)
        } else {
            short_output_target(argument, arguments, &mut index)
        };
        if target.is_some_and(|target| destination_is_opaque(path, target)) {
            return true;
        }
        index += 1;
    }
    false
}

fn tee_output_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut options_ended = false;
    arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--help" && argument.as_str() != "--version")
        .any(|argument| {
            if argument == "--" {
                options_ended = true;
                return false;
            }
            if !options_ended && argument.starts_with('-') && argument != "-" {
                return false;
            }
            destination_is_opaque(path, argument)
        })
}

fn short_output_target<'a>(argument: &'a str, arguments: &'a [String], index: &mut usize) -> Option<&'a str> {
    let options = argument.strip_prefix('-').filter(|options| !options.starts_with('-'))?;
    let output = options.find('o')?;
    let attached = &options[output + 1..];
    if !attached.is_empty() {
        return Some(attached);
    }
    *index += 1;
    arguments.get(*index).map(String::as_str)
}

fn arguments_reference_protected_inputs(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| is_literal_protected_input(argument))
}

fn output_redirection_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let Some(target) = output_redirection_target(argument) else {
            continue;
        };
        let target = if target.is_empty() {
            let Some(target) = arguments.next() else {
                continue;
            };
            target
        } else {
            target
        };
        if destination_is_opaque(path, target) {
            return true;
        }
    }
    false
}

fn output_redirection_target(argument: &str) -> Option<&str> {
    let operator = argument.trim_start_matches(|character: char| character.is_ascii_digit());
    operator.strip_prefix(">>").or_else(|| operator.strip_prefix(">|")).or_else(|| operator.strip_prefix('>'))
}

fn is_literal_protected_input(candidate: &str) -> bool {
    path::normalize_literal(candidate).is_some_and(|path| super::super::super::is_protected_check_input(&path))
}

#[cfg(test)]
mod tests;
