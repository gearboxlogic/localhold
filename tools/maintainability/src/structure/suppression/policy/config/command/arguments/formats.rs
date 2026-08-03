use std::path::Path;

pub(super) fn supports_shell_source(path: &str) -> bool {
    let path = Path::new(path);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if matches!(
        basename.to_ascii_lowercase().as_str(),
        "justfile" | ".justfile" | "makefile" | "gnumakefile" | "package.json"
    ) {
        return true;
    }
    path.extension().and_then(|extension| extension.to_str()).is_none_or(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "sh" | "bash" | "zsh" | "fish" | "ps1" | "just" | "yml" | "yaml" | "toml"
        )
    })
}

pub(super) fn is_yaml(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yml" | "yaml"))
}

pub(super) fn is_mise(path: &str) -> bool {
    super::super::is_mise_config(Path::new(path))
}

pub(super) fn is_standalone_shell_surface(path: &str) -> bool {
    let path = Path::new(path);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_ascii_lowercase();
    if matches!(basename.as_str(), "justfile" | ".justfile" | "makefile" | "gnumakefile" | "package.json") {
        return false;
    }
    path.extension().and_then(|extension| extension.to_str()).is_none_or(|extension| {
        !matches!(
            extension.to_ascii_lowercase().as_str(),
            "bat" | "cmd" | "json" | "just" | "make" | "mk" | "ps1" | "py" | "rs" | "toml" | "yml" | "yaml"
        )
    })
}

pub(super) fn is_python(path: &str) -> bool {
    has_extension(path, &["py"])
}

pub(super) fn is_windows_command(path: &str) -> bool {
    has_extension(path, &["cmd", "bat"])
}

pub(super) fn is_just(path: &str) -> bool {
    let path = Path::new(path);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    matches!(basename.to_ascii_lowercase().as_str(), "justfile" | ".justfile") || has_extension(path, &["just"])
}

pub(super) fn is_powershell(path: &str) -> bool {
    has_extension(path, &["ps1"])
}

fn has_extension(path: impl AsRef<Path>, extensions: &[&str]) -> bool {
    path.as_ref()
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extensions.iter().any(|candidate| extension.eq_ignore_ascii_case(candidate)))
}
