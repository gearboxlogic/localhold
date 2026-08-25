use super::{ValueSemantics, path};

mod operands;
mod output;
mod path_policy;
mod profiles;
mod removal;
mod strip;
mod tar;
mod unzip;

pub(super) use path_policy::Policy as PathPolicy;

#[derive(Clone, Copy)]
struct DispatchContext<'a> {
    surface: &'a str,
    source_is_reviewed: bool,
    semantics: ValueSemantics,
    path_policy: Option<&'a PathPolicy>,
}

pub(super) fn dispatch_is_opaque(path: &str, source_is_reviewed: bool, command: &str, arguments: &[String]) -> bool {
    dispatch_with_context(
        DispatchContext {
            surface: path,
            source_is_reviewed,
            semantics: ValueSemantics::Shell,
            path_policy: None,
        },
        command,
        arguments,
    )
}

pub(super) fn argv_dispatch_is_opaque(path: &str, command: &str, arguments: &[String]) -> bool {
    dispatch_with_context(
        DispatchContext {
            surface: path,
            source_is_reviewed: false,
            semantics: ValueSemantics::Literal,
            path_policy: None,
        },
        command,
        arguments,
    )
}

pub(super) fn dispatch_with_path_policy(path: &str, source_is_reviewed: bool, command: &str, arguments: &[String], path_policy: &PathPolicy) -> bool {
    dispatch_with_context(
        DispatchContext {
            surface: path,
            source_is_reviewed,
            semantics: ValueSemantics::Shell,
            path_policy: Some(path_policy),
        },
        command,
        arguments,
    )
}

pub(super) fn argv_dispatch_with_path_policy(path: &str, command: &str, arguments: &[String], path_policy: &PathPolicy) -> bool {
    dispatch_with_context(
        DispatchContext {
            surface: path,
            source_is_reviewed: false,
            semantics: ValueSemantics::Literal,
            path_policy: Some(path_policy),
        },
        command,
        arguments,
    )
}

fn dispatch_with_context(context: DispatchContext<'_>, command: &str, arguments: &[String]) -> bool {
    if profiles::accepts_dynamic_arguments(context.surface, context.source_is_reviewed, command, arguments) {
        return false;
    }
    if is_compression_command(command) {
        return true;
    }
    if inspection_only(command, arguments) {
        return false;
    }
    dynamic_mutation_arguments_are_opaque(command, arguments, context.semantics)
        || super::editor::is_command_capable(command)
        || install_dispatch_is_opaque(command, arguments)
        || context.semantics == ValueSemantics::Shell && output_redirection_is_opaque(context, arguments)
        || output::dispatch_is_opaque(context, command, arguments)
        || strip::dispatch_is_opaque(context, command, arguments)
        || is_objcopy_command(command) && (arguments_reference_protected_inputs(context, arguments) || final_destination_is_opaque(context, arguments))
        || match command {
            "chmod" | "chmod.exe" => operands::chmod_is_opaque(context, arguments),
            "cp" | "cp.exe" => copy_may_materialize_links(arguments) || arguments_reference_protected_inputs(context, arguments) || final_destination_is_opaque(context, arguments),
            "install" | "install.exe" | "truncate" | "truncate.exe" | "unlink" => {
                arguments_reference_protected_inputs(context, arguments) || final_destination_is_opaque(context, arguments)
            }
            "ln" | "ln.exe" | "link" | "mv" | "mv.exe" | "move" | "move-item" => !inspection_only(command, arguments),
            "del" | "del.exe" | "erase" | "erase.exe" | "remove-item" | "rm" | "rm.exe" => removal::dispatch_is_opaque(context, arguments),
            "tee" | "tee.exe" => tee_output_is_opaque(context, arguments),
            "copy" | "copy-item" | "set-content" | "add-content" | "out-file" => arguments_reference_protected_inputs(context, arguments),
            "curl" | "curl.exe" => curl_output_is_opaque(context, arguments) || curl_remote_name_is_opaque(arguments),
            "dd" | "dd.exe" => arguments
                .iter()
                .filter_map(|argument| argument.strip_prefix("of="))
                .any(|destination| destination_is_opaque(context, destination)),
            "iconv" | "iconv.exe" => iconv_output_is_opaque(context, arguments),
            "jar" | "jar.exe" => jar_dispatch_is_opaque(arguments, context.semantics),
            "openssl" | "openssl.exe" => openssl_output_is_opaque(context, arguments),
            "patch" | "patch.exe" => true,
            "shuf" | "shuf.exe" => shuf_output_is_opaque(context, arguments),
            "tar" | "tar.exe" => tar::dispatch_is_opaque(context, arguments),
            "unzip" | "unzip.exe" => unzip::dispatch_is_opaque(arguments),
            "perl" | "perl.exe" if operands::perl_in_place_selected(arguments) => operands::perl_targets_are_opaque(context, arguments),
            "sed" | "sed.exe" if operands::sed_in_place_selected(arguments) => operands::sed_targets_are_opaque(context, arguments),
            _ => false,
        }
}

fn dynamic_mutation_arguments_are_opaque(command: &str, arguments: &[String], semantics: ValueSemantics) -> bool {
    is_mutation_capable(command)
        && !inspection_only(command, arguments)
        && arguments.iter().any(|argument| semantics.contains_dynamic(argument))
        && !dynamic_inputs_are_proven_read_only(command, arguments)
}

pub(super) fn reviewed_arguments(path: &str, source_is_reviewed: bool, command: &str, arguments: &[String]) -> bool {
    profiles::accepts_dynamic_arguments(path, source_is_reviewed, command, arguments)
}

pub(super) fn reviewed_sources() -> std::collections::BTreeSet<(&'static str, &'static str)> {
    profiles::reviewed_sources()
}

fn dynamic_inputs_are_proven_read_only(command: &str, arguments: &[String]) -> bool {
    matches!(command, "jar" | "jar.exe") && jar_list_is_read_only(arguments)
}

fn jar_list_is_read_only(arguments: &[String]) -> bool {
    if let Some(options) = arguments
        .first()
        .filter(|options| !options.starts_with('-') && options.chars().all(|option| matches!(option, 'f' | 't' | 'v')) && options.contains('t') && !options.is_empty())
    {
        let required_operands = usize::from(options.contains('f'));
        return arguments.len() == required_operands + 1;
    }
    let mut lists = false;
    let mut consumes_file = false;
    for argument in arguments {
        if consumes_file {
            consumes_file = false;
            continue;
        }
        match argument.as_str() {
            "--list" => lists = true,
            "--file" => consumes_file = true,
            "--verbose" => {}
            _ if !argument.starts_with('-') && !path::contains_dynamic_value(argument) => {}
            _ => return false,
        }
    }
    lists && !consumes_file
}

fn is_mutation_capable(command: &str) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    matches!(
        command,
        "add-content"
            | "brotli"
            | "bunzip2"
            | "bzip2"
            | "chmod"
            | "copy"
            | "copy-item"
            | "cp"
            | "curl"
            | "dd"
            | "del"
            | "erase"
            | "gzip"
            | "gunzip"
            | "iconv"
            | "install"
            | "jar"
            | "link"
            | "ln"
            | "lz4"
            | "move"
            | "move-item"
            | "mv"
            | "objcopy"
            | "openssl"
            | "out-file"
            | "patch"
            | "perl"
            | "pigz"
            | "remove-item"
            | "rm"
            | "set-content"
            | "shuf"
            | "sponge"
            | "strip"
            | "tee"
            | "tar"
            | "truncate"
            | "unlink"
            | "unlz4"
            | "unpigz"
            | "unxz"
            | "unzip"
            | "unzstd"
            | "xz"
            | "zstd"
    )
}

fn is_compression_command(command: &str) -> bool {
    matches!(
        command.strip_suffix(".exe").unwrap_or(command),
        "brotli" | "bunzip2" | "bzip2" | "gzip" | "gunzip" | "lz4" | "pigz" | "unlz4" | "unpigz" | "unxz" | "unzstd" | "xz" | "zstd"
    )
}

fn copy_may_materialize_links(arguments: &[String]) -> bool {
    let mut preserve_operand = false;
    for argument in arguments.iter().take_while(|argument| argument.as_str() != "--") {
        if preserve_operand {
            preserve_operand = false;
            if argument.split(',').any(|attribute| matches!(attribute, "all" | "links")) {
                return true;
            }
            continue;
        }
        if let Some(options) = argument.strip_prefix('-').filter(|options| !options.starts_with('-')) {
            if cp_short_options_materialize_links(options) {
                return true;
            }
            continue;
        }
        let Some(option) = argument.strip_prefix("--") else {
            continue;
        };
        let (option, operand) = option.split_once('=').map_or((option, None), |(option, operand)| (option, Some(operand)));
        if [("archive", "a"), ("link", "l"), ("no-dereference", "no-d"), ("recursive", "r"), ("symbolic-link", "sy")]
            .iter()
            .any(|(full, minimum)| option.len() >= minimum.len() && full.starts_with(option))
        {
            return true;
        }
        if option.len() >= "pre".len() && "preserve".starts_with(option) {
            if operand.is_some_and(preserves_links) {
                return true;
            }
            if operand.is_none() {
                preserve_operand = true;
            }
        }
    }
    preserve_operand
}

fn preserves_links(operand: &str) -> bool {
    operand.split(',').any(|attribute| matches!(attribute, "all" | "links"))
}

fn cp_short_options_materialize_links(options: &str) -> bool {
    for option in options.chars() {
        if matches!(option, 'P' | 'R' | 'a' | 'd' | 'l' | 'r' | 's') {
            return true;
        }
        if matches!(option, 'S' | 't') {
            return false;
        }
    }
    false
}

fn inspection_only(command: &str, arguments: &[String]) -> bool {
    arguments
        .first()
        .is_some_and(|argument| long_help_is_supported(command) && matches!(argument.as_str(), "--help" | "--version") || command == "move" && argument == "/?")
}

fn long_help_is_supported(command: &str) -> bool {
    matches!(
        command,
        "chmod"
            | "chmod.exe"
            | "cp"
            | "cp.exe"
            | "install"
            | "install.exe"
            | "link"
            | "ln"
            | "ln.exe"
            | "mv"
            | "mv.exe"
            | "tar"
            | "tar.exe"
            | "truncate"
            | "truncate.exe"
            | "unlink"
    )
}

fn install_dispatch_is_opaque(command: &str, arguments: &[String]) -> bool {
    if !matches!(command, "install" | "install.exe") {
        return false;
    }
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        option
            .strip_prefix("--")
            .is_some_and(|option| option == "strip" || option.len() >= "strip-p".len() && "strip-program".starts_with(option))
            || option
                .strip_prefix('-')
                .filter(|options| !options.starts_with('-'))
                .is_some_and(|options| options.contains('s'))
    })
}

fn jar_dispatch_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
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
        if semantics.contains_dynamic(argument) {
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

fn final_destination_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    explicit_target_directories(arguments)
        .chain(arguments.last().map(String::as_str))
        .any(|destination| destination_is_opaque(context, destination))
}

fn destination_is_opaque(context: DispatchContext<'_>, destination: &str) -> bool {
    path_policy::target_is_opaque(context.path_policy, destination, context.semantics)
        && !(context.source_is_reviewed && context.semantics == ValueSemantics::Shell && is_reviewed_dynamic_destination(context.surface, destination))
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
    ("script/check-maintainability-bootstrap.sh", "$snapshot_root/.git/info/attributes"),
    (".github/workflows/gpu-release-gate.yml", "$RUNNER_TEMP/hold-cuda"),
    (".github/workflows/gpu-release-gate.yml", "$GITHUB_ENV"),
    (".github/workflows/gpu-release-gate.yml", "$GITHUB_OUTPUT"),
    (".github/workflows/gpu-release-gate.yml", "$moved"),
    (".github/workflows/gpu-release-gate.yml", "$dependency"),
];

fn iconv_output_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
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
        if target.is_some_and(|target| destination_is_opaque(context, target)) {
            return true;
        }
        index += 1;
    }
    false
}

fn openssl_output_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        if matches!(argument.as_str(), "-out" | "-keyout" | "-writerand" | "-CAserial") {
            index += 1;
            if destination_is_opaque(context, arguments.get(index).map_or("", String::as_str)) {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn shuf_output_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
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
        if target.is_some_and(|target| destination_is_opaque(context, target)) {
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

fn curl_output_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
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
        if target.is_some_and(|target| destination_is_opaque(context, target)) {
            return true;
        }
        index += 1;
    }
    false
}

fn tee_output_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut options_ended = false;
    for argument in arguments {
        if !options_ended && argument == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && matches!(argument.as_str(), "--help" | "--version") {
            return false;
        }
        if !options_ended && argument.starts_with('-') && argument != "-" {
            continue;
        }
        if destination_is_opaque(context, argument) {
            return true;
        }
    }
    false
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

fn arguments_reference_protected_inputs(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut options_ended = false;
    arguments.iter().any(|argument| {
        if argument == "--" && !options_ended {
            options_ended = true;
            return false;
        }
        (options_ended || !argument.starts_with('-')) && path_policy::target_is_opaque(context.path_policy, argument, context.semantics)
    })
}

fn output_redirection_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
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
        if destination_is_opaque(context, target) {
            return true;
        }
    }
    false
}

fn output_redirection_target(argument: &str) -> Option<&str> {
    let operator = argument.trim_start_matches(|character: char| character.is_ascii_digit());
    operator.strip_prefix(">>").or_else(|| operator.strip_prefix(">|")).or_else(|| operator.strip_prefix('>'))
}

#[cfg(test)]
mod tests;
