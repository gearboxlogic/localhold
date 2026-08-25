use super::ValueSemantics;

#[cfg(test)]
pub(super) fn dispatch_is_opaque(command: &str, arguments: &[String]) -> bool {
    dispatch_with_semantics(command, arguments, ValueSemantics::Shell)
}

pub(super) fn dispatch_with_semantics(command: &str, arguments: &[String], semantics: ValueSemantics) -> bool {
    compilation_tool(command) && arguments.iter().any(|argument| semantics.contains_dynamic(argument))
        || is_compiler_driver(command) && arguments.iter().any(|argument| is_dispatch_override(argument))
        || is_rust_compiler(command) && (custom_target_selection_is_opaque(arguments) || rust_compiler_dispatch_is_opaque(arguments))
        || is_rustdoc(command) && rustdoc_dispatch_is_opaque(arguments)
        || is_linker_tool(command) && arguments.iter().any(|argument| linker_dispatch_override(argument))
        || is_archive_tool(command) && archive_dispatch_is_opaque(arguments, semantics)
        || is_binutils_plugin_tool(command)
            && arguments
                .iter()
                .take_while(|argument| argument.as_str() != "--")
                .any(|argument| binutils_plugin_option(argument))
}

pub(super) fn custom_target_selection_is_opaque(arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let target = if argument == "--target" {
            index += 1;
            Some(arguments.get(index).map_or("", String::as_str))
        } else {
            argument.strip_prefix("--target=")
        };
        if target.is_some_and(custom_target_is_opaque) {
            return true;
        }
        index += 1;
    }
    false
}

fn custom_target_is_opaque(target: &str) -> bool {
    target.is_empty()
        || target.starts_with('.')
        || target.contains(['/', '\\', ':'])
        || target.rsplit_once('.').is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("json"))
}

fn compilation_tool(command: &str) -> bool {
    is_compiler_driver(command) || is_rust_compiler(command) || is_linker_tool(command) || is_archive_tool(command) || is_binutils_plugin_tool(command)
}

pub(super) fn is_recognized(command: &str) -> bool {
    compilation_tool(command)
}

pub(super) fn accepts_output_path(command: &str) -> bool {
    is_compiler_driver(command) || is_rust_compiler(command) || is_linker_tool(command)
}

fn is_archive_tool(command: &str) -> bool {
    let unversioned = unversioned_tool_name(command);
    unversioned == "ar" || unversioned.strip_suffix("ar").is_some_and(|prefix| prefix.ends_with('-'))
}

fn archive_dispatch_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    let mut consumes_operand = false;
    for argument in arguments {
        if consumes_operand {
            consumes_operand = false;
            continue;
        }
        if matches!(argument.as_str(), "--output" | "--plugin" | "--target") {
            consumes_operand = true;
            continue;
        }
        if argument == "--" || argument.starts_with("--") {
            continue;
        }
        if semantics.contains_dynamic(argument) {
            return true;
        }
        let options = argument.strip_prefix('-').unwrap_or(argument);
        if options.chars().all(is_archive_option) && options.chars().any(is_archive_operation) {
            return options.contains('x');
        }
    }
    false
}

const fn is_archive_operation(option: char) -> bool {
    matches!(option, 'd' | 'm' | 'p' | 'q' | 'r' | 's' | 't' | 'x')
}

const fn is_archive_option(option: char) -> bool {
    is_archive_operation(option) || matches!(option, 'a' | 'b' | 'c' | 'D' | 'f' | 'i' | 'l' | 'M' | 'N' | 'o' | 'O' | 'P' | 'S' | 'T' | 'u' | 'v' | 'V')
}

pub(super) fn rust_compiler_dispatch_is_opaque(arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--extern" || argument.starts_with("--extern=") {
            return true;
        }
        let option = if matches!(argument.as_str(), "-C" | "--codegen") {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else {
            argument.strip_prefix("-C").or_else(|| argument.strip_prefix("--codegen="))
        };
        if option.is_some_and(|option| option.trim_start_matches('=').starts_with("linker=")) {
            return true;
        }
        index += 1;
    }
    false
}

fn rustdoc_dispatch_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        matches!(option, "--test-builder" | "--test-builder-wrapper" | "--test-runtool" | "--test-runtool-arg")
    })
}

fn is_rust_compiler(command: &str) -> bool {
    matches!(command.strip_suffix(".exe").unwrap_or(command), "rustc" | "rustdoc")
}

fn is_rustdoc(command: &str) -> bool {
    command.strip_suffix(".exe").unwrap_or(command) == "rustdoc"
}

fn is_binutils_plugin_tool(command: &str) -> bool {
    let unversioned = unversioned_tool_name(command);
    ["ar", "nm", "ranlib"]
        .iter()
        .any(|tool| unversioned == *tool || unversioned.strip_suffix(tool).is_some_and(|prefix| prefix.ends_with('-')))
}

fn binutils_plugin_option(argument: &str) -> bool {
    let option = argument.split_once('=').map_or(argument, |(option, _)| option);
    option.len() >= "--pl".len() && "--plugin".starts_with(option)
}

fn is_compiler_driver(command: &str) -> bool {
    let unversioned = unversioned_tool_name(command);
    is_driver_name(unversioned) || unversioned.rsplit('-').next().is_some_and(is_driver_name)
}

fn unversioned_tool_name(command: &str) -> &str {
    let stem = command.strip_suffix(".exe").unwrap_or(command);
    stem.rsplit_once('-')
        .filter(|(_, suffix)| suffix.chars().all(|character| character.is_ascii_digit() || character == '.'))
        .map_or(stem, |(prefix, _)| prefix)
}

fn is_driver_name(name: &str) -> bool {
    matches!(name, "c++" | "cc" | "clang" | "clang++" | "clang-cl" | "g++" | "gcc")
}

fn is_linker_tool(command: &str) -> bool {
    if command == "link.exe" {
        return true;
    }
    let name = unversioned_tool_name(command);
    matches!(name, "ld" | "ld.bfd" | "ld.gold" | "ld.lld" | "ld64.lld" | "lld" | "lld-link" | "mold")
        || name.rsplit('-').next().is_some_and(|suffix| matches!(suffix, "ld" | "ld.bfd" | "ld.gold" | "ld.lld"))
}

fn linker_dispatch_override(argument: &str) -> bool {
    argument.starts_with('@') && argument.len() > 1 || direct_linker_plugin_option(argument)
}

fn direct_linker_plugin_option(argument: &str) -> bool {
    let option = argument.split_once('=').map_or(argument, |(option, _)| option);
    [
        ("-plugin", "-pl"),
        ("--plugin", "--pl"),
        ("-load-pass-plugin", "-load-pass-pl"),
        ("--load-pass-plugin", "--load-pass-pl"),
    ]
    .iter()
    .any(|(full, minimum)| option.len() >= minimum.len() && full.starts_with(option))
}

fn is_dispatch_override(argument: &str) -> bool {
    argument.starts_with('@') && argument.len() > 1
        || argument == "-wrapper"
        || argument.starts_with("-fplugin=")
        || argument.starts_with("-fpass-plugin=")
        || argument.starts_with("-specs=")
        || argument == "--config"
        || argument.starts_with("--config=")
        || argument == "-Xclang"
        || argument.starts_with("-Xclang=")
        || argument == "-Xlinker"
        || argument.starts_with("-Xlinker=")
        || argument.starts_with("--for-linker=")
        || argument == "--for-linker"
        || argument == "-B"
        || argument.starts_with("-B") && argument.len() > 2
        || argument.starts_with("--gcc-install-dir=")
        || argument.starts_with("--gcc-toolchain=")
        || argument.starts_with("--ld-path=")
        || argument.starts_with("-fuse-ld=")
        || linker_plugin_argument(argument)
}

fn linker_plugin_argument(argument: &str) -> bool {
    argument.strip_prefix("-Wl,").is_some_and(|arguments| {
        arguments.split(',').any(|option| {
            matches!(option, "-plugin" | "--plugin" | "-load-pass-plugin" | "--load-pass-plugin")
                || option.starts_with("-plugin=")
                || option.starts_with("--plugin=")
                || option.starts_with("-load-pass-plugin=")
                || option.starts_with("--load-pass-plugin=")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::dispatch_is_opaque;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn compiler_driver_variants_are_recognized_without_matching_related_tools() {
        for command in ["gcc", "gcc.exe", "gcc-15", "x86_64-linux-gnu-gcc", "x86_64-linux-gnu-g++-14", "clang++-19", "clang-cl.exe"] {
            assert!(dispatch_is_opaque(command, &arguments(&["-fplugin=quality/lint.so"])), "{command}");
        }
        for command in ["cargo", "gcc-ar", "llvm-ar"] {
            assert!(!dispatch_is_opaque(command, &arguments(&["-fplugin=quality/lint.so"])), "{command}");
        }
    }

    #[test]
    fn binutils_plugin_loading_is_opaque() {
        for command in [
            "ar",
            "ar.exe",
            "gcc-ar",
            "llvm-ar-19",
            "x86_64-linux-gnu-ar",
            "nm",
            "nm.exe",
            "gcc-nm",
            "llvm-nm-19",
            "x86_64-linux-gnu-nm",
            "ranlib",
            "ranlib.exe",
            "gcc-ranlib",
            "llvm-ranlib-19",
            "x86_64-linux-gnu-ranlib",
        ] {
            assert!(
                dispatch_is_opaque(command, &arguments(&["--plugin=quality/lint.so", "rc", "quality/archive.a"])),
                "{command}"
            );
            assert!(
                dispatch_is_opaque(command, &arguments(&["--plugin", "quality/lint.so", "rc", "quality/archive.a"])),
                "{command}"
            );
            assert!(dispatch_is_opaque(command, &arguments(&["--pl=quality/lint.so", "rc", "quality/archive.a"])), "{command}");
        }
        assert!(!dispatch_is_opaque("ar", &arguments(&["rc", "quality/archive.a", "quality/input.o"])));
        assert!(!dispatch_is_opaque("nm", &arguments(&["--defined-only", "quality/input.o"])));
        assert!(!dispatch_is_opaque("ranlib", &arguments(&["quality/archive.a"])));
        assert!(!dispatch_is_opaque("jar", &arguments(&["--plugin=quality/lint.so"])));
    }

    #[test]
    fn archive_extraction_and_dynamic_operations_fail_closed() {
        for (command, values) in [
            ("ar", &["x", "quality/payload.a"][..]),
            ("ar.exe", &["-xv", "quality/payload.a"]),
            ("llvm-ar-19", &["--output", "extracted", "x", "quality/payload.a"]),
            ("x86_64-linux-gnu-ar", &["-X32_64", "x", "quality/payload.a"]),
            ("gcc-ar", &["$operation", "quality/payload.a"]),
        ] {
            assert!(dispatch_is_opaque(command, &arguments(values)), "{command}: {values:?}");
        }
        assert!(!dispatch_is_opaque("ar", &arguments(&["t", "quality/payload.a"])));
        assert!(!dispatch_is_opaque("ar", &arguments(&["p", "quality/payload.a", "member"])));
    }

    #[test]
    fn direct_linker_plugins_and_response_files_are_opaque() {
        for command in [
            "ld",
            "ld.exe",
            "ld-2.44",
            "x86_64-linux-gnu-ld",
            "ld.bfd",
            "ld.gold",
            "ld.lld-19",
            "lld",
            "lld-link.exe",
            "wasm-ld",
            "mold",
            "link.exe",
        ] {
            assert!(dispatch_is_opaque(command, &arguments(&["@quality/link.args"])), "{command}");
            assert!(dispatch_is_opaque(command, &arguments(&["-plugin", "quality/lint.so"])), "{command}");
            assert!(dispatch_is_opaque(command, &arguments(&["--plugin=quality/lint.so"])), "{command}");
        }
        assert!(dispatch_is_opaque("ld", &arguments(&["--pl=quality/lint.so"])));
        assert!(dispatch_is_opaque("ld.lld", &arguments(&["--load-pass-plugin=quality/lint.so"])));
        assert!(!dispatch_is_opaque("ld", &arguments(&["-o", "quality/output", "quality/input.o"])));
        assert!(!dispatch_is_opaque("fold", &arguments(&["@quality/link.args"])));
        assert!(!dispatch_is_opaque("link", &arguments(&["@quality/link.args"])));
    }

    #[test]
    fn executable_dispatch_options_are_opaque() {
        for values in [
            &["-wrapper", "sh,quality/lint.txt"][..],
            &["-fplugin=quality/lint.so"],
            &["-fpass-plugin=quality/lint.so"],
            &["-specs=quality/lint.specs"],
            &["@quality/lint.args"],
            &["-Xclang", "-load", "-Xclang", "quality/lint.so"],
            &["-Wl,--plugin=quality/lint.so"],
            &["--ld-path=quality/ld"],
            &["-Bquality/toolchain"],
        ] {
            assert!(dispatch_is_opaque("gcc", &arguments(values)), "{values:?}");
        }
        assert!(!dispatch_is_opaque("gcc", &arguments(&["-Wall", "-c", "quality/input.c"])));
    }

    #[test]
    fn rust_linker_selection_is_opaque() {
        for values in [&["-C", "linker=quality/lint"][..], &["-Clinker=quality/lint"], &["--codegen=linker=quality/lint"]] {
            assert!(dispatch_is_opaque("rustc", &arguments(values)), "{values:?}");
        }
        assert!(!dispatch_is_opaque("rustc", &arguments(&["-C", "opt-level=2"])));
    }

    #[test]
    fn custom_rust_target_specifications_are_opaque() {
        for command in ["rustc", "rustc.exe", "rustdoc", "rustdoc.exe"] {
            for values in [
                &["--target", "quality/host.json", "-"][..],
                &["--target=quality\\host.JSON", "-"],
                &["--target", "/tmp/host", "-"],
                &["--target=custom.json", "-"],
                &["--target", "", "-"],
            ] {
                assert!(dispatch_is_opaque(command, &arguments(values)), "{command}: {values:?}");
            }
            assert!(!dispatch_is_opaque(command, &arguments(&["--target", "x86_64-unknown-linux-gnu", "-"])));
            assert!(!dispatch_is_opaque(command, &arguments(&["--target=wasm32-wasip1", "-"])));
        }
    }

    #[test]
    fn rust_external_crates_are_opaque() {
        for values in [
            &["quality/benign.rs", "--extern", "serde=quality/libserde.so"][..],
            &["quality/benign.rs", "--extern=serde=quality/libserde.so"],
        ] {
            assert!(dispatch_is_opaque("rustc", &arguments(values)), "{values:?}");
            assert!(dispatch_is_opaque("rustdoc", &arguments(values)), "{values:?}");
        }
    }

    #[test]
    fn rustdoc_test_program_overrides_fail_closed() {
        for values in [
            &["--test", "quality/doc.rs", "--test-runtool", "sh", "--test-runtool-arg", "quality/lint.txt"][..],
            &["--test", "quality/doc.rs", "--test-runtool=sh"],
            &["--test", "quality/doc.rs", "--test-builder", "quality/builder"],
            &["--test", "quality/doc.rs", "--test-builder-wrapper=quality/wrapper"],
        ] {
            assert!(dispatch_is_opaque("rustdoc", &arguments(values)), "{values:?}");
            assert!(dispatch_is_opaque("rustdoc.exe", &arguments(values)), "{values:?}");
        }
        assert!(!dispatch_is_opaque("rustdoc", &arguments(&["--test", "quality/doc.rs", "--no-run"])));
    }
}
