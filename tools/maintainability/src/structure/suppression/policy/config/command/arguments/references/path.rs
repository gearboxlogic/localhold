use std::path::{Component, Path};

pub(super) enum ProgramPath<'a> {
    NotPath,
    Literal(&'a str),
    Opaque,
}

pub(super) fn select_program(command: &str, direct_program_paths: bool) -> ProgramPath<'_> {
    if windows_drive_prefix(command) {
        return ProgramPath::Opaque;
    }
    if !direct_program_paths {
        return ProgramPath::NotPath;
    }
    if trusted_system_program(command) {
        return ProgramPath::NotPath;
    }
    if is_absolute(command) || command.starts_with('\\') {
        return ProgramPath::Opaque;
    }
    let explicit_relative = ["./", "../", r".\", r"..\"].iter().any(|prefix| command.starts_with(prefix));
    if explicit_relative {
        return if contains_dynamic_value(command) {
            ProgramPath::Opaque
        } else {
            ProgramPath::Literal(command)
        };
    }
    if contains_dynamic_value(command) || !command.contains(['/', '\\']) {
        return ProgramPath::NotPath;
    }
    ProgramPath::Literal(command)
}

pub(super) fn normalize_literal(candidate: &str) -> Option<String> {
    if contains_dynamic_value(candidate) || candidate.contains(['\\', ':']) || is_absolute(candidate) {
        return None;
    }
    let path = Path::new(candidate);
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value.to_str()?.to_owned()),
            _ => return None,
        }
    }
    (!normalized.is_empty()).then(|| normalized.join("/"))
}

pub(super) fn contains_dynamic_value(value: &str) -> bool {
    value.contains(['$', '`', '%', '!', '*', '?', '[', '{', '~'])
}

pub(super) fn is_absolute(candidate: &str) -> bool {
    candidate.starts_with('/') || Path::new(candidate).is_absolute() || windows_absolute(candidate)
}

pub(super) fn trusted_system_program(command: &str) -> bool {
    ["/bin/", "/usr/bin/", "/mingw64/bin/"].iter().any(|prefix| {
        command
            .strip_prefix(prefix)
            .is_some_and(|name| !name.is_empty() && !name.contains(['/', '\\']) && !contains_dynamic_value(name))
    })
}

fn windows_absolute(command: &str) -> bool {
    let bytes = command.as_bytes();
    command.starts_with(r"\\") || windows_drive_prefix(command) && bytes.get(2).is_some_and(|separator| matches!(separator, b'/' | b'\\'))
}

fn windows_drive_prefix(command: &str) -> bool {
    let bytes = command.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::{ProgramPath, select_program};

    #[test]
    fn runtime_absolute_programs_fail_closed() {
        assert!(matches!(select_program("/tmp/lint", true), ProgramPath::Opaque));
        assert!(matches!(select_program("/opt/local/bin/lint", true), ProgramPath::Opaque));
        assert!(matches!(select_program(r"C:\Temp\lint.exe", true), ProgramPath::Opaque));
        assert!(matches!(select_program(r"C:hidden.exe", true), ProgramPath::Opaque));
        assert!(matches!(select_program(r"c:hidden.exe", false), ProgramPath::Opaque));
        assert!(matches!(select_program(r"\Temp\lint.exe", true), ProgramPath::Opaque));
        assert!(matches!(select_program(r"\\server\share\lint.exe", true), ProgramPath::Opaque));
        assert!(matches!(select_program(r"\\?\UNC\server\share\lint.exe", true), ProgramPath::Opaque));
        assert!(matches!(select_program("./$PROGRAM", true), ProgramPath::Opaque));
    }

    #[test]
    fn static_system_and_repository_programs_are_distinguished() {
        assert!(matches!(select_program("/usr/bin/uname", true), ProgramPath::NotPath));
        assert!(matches!(select_program("/usr/bin/../tmp/lint", true), ProgramPath::Opaque));
        assert!(matches!(select_program(r"/usr/bin/..\tmp\lint", true), ProgramPath::Opaque));
        assert!(matches!(select_program("quality/lint", true), ProgramPath::Literal("quality/lint")));
        assert!(matches!(select_program("/tmp/lint", false), ProgramPath::NotPath));
    }
}
