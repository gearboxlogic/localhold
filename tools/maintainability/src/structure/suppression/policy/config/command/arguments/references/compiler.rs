pub(super) fn dispatch_is_opaque(command: &str, arguments: &[String]) -> bool {
    is_compiler_driver(command) && arguments.iter().any(|argument| is_dispatch_override(argument))
}

fn is_compiler_driver(command: &str) -> bool {
    let stem = command.strip_suffix(".exe").unwrap_or(command);
    let unversioned = stem
        .rsplit_once('-')
        .filter(|(_, suffix)| suffix.chars().all(|character| character.is_ascii_digit() || character == '.'))
        .map_or(stem, |(prefix, _)| prefix);
    is_driver_name(unversioned) || unversioned.rsplit('-').next().is_some_and(is_driver_name)
}

fn is_driver_name(name: &str) -> bool {
    matches!(name, "c++" | "cc" | "clang" | "clang++" | "clang-cl" | "g++" | "gcc")
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
}
