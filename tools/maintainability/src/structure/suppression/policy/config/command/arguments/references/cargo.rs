#[derive(Clone, Copy, Eq, PartialEq)]
enum ManifestSelection<'a> {
    None,
    Selected(&'a str),
    Opaque,
}

pub(super) fn dispatch_is_opaque(arguments: &[String]) -> bool {
    let Some(subcommand) = subcommand(arguments) else {
        return false;
    };
    if !is_execution_capable(subcommand) {
        return false;
    }
    if matches!(subcommand, "rustc" | "rustdoc") && rust_compiler_arguments_are_opaque(arguments) {
        return true;
    }

    let manifest = selected_manifest(arguments);
    if matches!(subcommand, "run" | "r") {
        return !matches!(manifest, ManifestSelection::Selected(path) if is_reviewed_tool_manifest(path));
    }
    matches!(manifest, ManifestSelection::Opaque) || matches!(manifest, ManifestSelection::Selected(path) if !is_reviewed_execution_manifest(path))
}

fn rust_compiler_arguments_are_opaque(arguments: &[String]) -> bool {
    arguments
        .iter()
        .position(|argument| argument == "--")
        .is_some_and(|separator| super::compiler::rust_compiler_dispatch_is_opaque(&arguments[separator + 1..]))
}

fn subcommand(arguments: &[String]) -> Option<&str> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if matches!(argument.as_str(), "--explain" | "--version" | "-V" | "--list" | "--help" | "-h") {
            return None;
        }
        let consumes_operand = matches!(argument.as_str(), "--color" | "--config" | "--change-directory" | "-C" | "-Z");
        if consumes_operand {
            index += 2;
            continue;
        }
        if argument.starts_with("--change-directory=") || argument.starts_with("-C") && argument.len() > 2 {
            index += 1;
            continue;
        }
        if argument.starts_with('+') || argument.starts_with('-') || argument.starts_with("--color=") || argument.starts_with("--config=") {
            index += 1;
            continue;
        }
        return Some(argument);
    }
    None
}

fn is_execution_capable(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "bench" | "build" | "b" | "check" | "c" | "clippy" | "doc" | "d" | "fix" | "install" | "nextest" | "package" | "publish" | "run" | "r" | "rustc" | "rustdoc" | "test" | "t"
    )
}

fn selected_manifest(arguments: &[String]) -> ManifestSelection<'_> {
    let mut manifest = ManifestSelection::None;
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let candidate = if argument == "--manifest-path" {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else {
            argument.strip_prefix("--manifest-path=")
        };
        if let Some(candidate) = candidate {
            if !matches!(manifest, ManifestSelection::None) || candidate.is_empty() {
                return ManifestSelection::Opaque;
            }
            manifest = ManifestSelection::Selected(candidate);
        } else if argument == "--manifest-path" {
            return ManifestSelection::Opaque;
        }
        index += 1;
    }
    manifest
}

fn is_reviewed_execution_manifest(path: &str) -> bool {
    path == "Cargo.toml" || is_reviewed_tool_manifest(path)
}

fn is_reviewed_tool_manifest(path: &str) -> bool {
    matches!(path, "tools/maintainability/Cargo.toml" | "tools/dependency-unsafe/Cargo.toml")
}

#[cfg(test)]
mod tests {
    use super::dispatch_is_opaque;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn standalone_manifest_execution_fails_closed() {
        for values in [
            &["test", "--manifest-path", "quality/helper/Cargo.toml"][..],
            &["build", "--manifest-path=quality/helper/Cargo.toml"],
            &["bench", "--manifest-path", "$MANIFEST"],
            &["nextest", "run", "--manifest-path", "quality/helper/Cargo.toml"],
        ] {
            assert!(dispatch_is_opaque(&arguments(values)), "{values:?}");
        }
    }

    #[test]
    fn repository_and_reviewed_tool_manifests_remain_auditable() {
        for values in [
            &["test", "--workspace", "--locked"][..],
            &["test", "--manifest-path", "Cargo.toml"],
            &["clippy", "--manifest-path", "tools/maintainability/Cargo.toml"],
            &["build", "--manifest-path=tools/dependency-unsafe/Cargo.toml"],
            &["metadata", "--manifest-path", "quality/helper/Cargo.toml"],
        ] {
            assert!(!dispatch_is_opaque(&arguments(values)), "{values:?}");
        }
        assert!(dispatch_is_opaque(&arguments(&["run"])));
        assert!(dispatch_is_opaque(&arguments(&["run", "--manifest-path", "Cargo.toml"])));
        assert!(!dispatch_is_opaque(&arguments(&["run", "--manifest-path", "tools/maintainability/Cargo.toml"])));
    }

    #[test]
    fn custom_rust_linkers_fail_closed() {
        assert!(dispatch_is_opaque(&arguments(&["rustc", "--", "-C", "linker=quality/lint"])));
        assert!(dispatch_is_opaque(&arguments(&["rustdoc", "--", "-Clinker=quality/lint"])));
        assert!(!dispatch_is_opaque(&arguments(&["rustc", "--", "-C", "opt-level=2"])));
    }
}
