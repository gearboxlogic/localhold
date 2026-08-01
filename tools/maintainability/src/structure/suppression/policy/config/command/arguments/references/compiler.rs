pub(super) fn dispatch_is_opaque(command: &str, arguments: &[String]) -> bool {
    is_compiler_driver(command) && arguments.iter().any(|argument| is_dispatch_override(argument))
        || is_rust_compiler(command) && rust_compiler_dispatch_is_opaque(arguments)
        || is_linker_tool(command) && arguments.iter().any(|argument| linker_dispatch_override(argument))
        || is_archive_tool(command)
            && arguments
                .iter()
                .take_while(|argument| argument.as_str() != "--")
                .any(|argument| archive_plugin_option(argument))
}

pub(super) fn rust_compiler_dispatch_is_opaque(arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
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

fn is_rust_compiler(command: &str) -> bool {
    matches!(command.strip_suffix(".exe").unwrap_or(command), "rustc" | "rustdoc")
}

fn is_archive_tool(command: &str) -> bool {
    let unversioned = unversioned_tool_name(command);
    unversioned == "ar" || unversioned.ends_with("-ar")
}

fn archive_plugin_option(argument: &str) -> bool {
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
    fn archive_plugin_loading_is_opaque() {
        for command in ["ar", "ar.exe", "gcc-ar", "llvm-ar-19", "x86_64-linux-gnu-ar"] {
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
        assert!(!dispatch_is_opaque("jar", &arguments(&["--plugin=quality/lint.so"])));
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
}
