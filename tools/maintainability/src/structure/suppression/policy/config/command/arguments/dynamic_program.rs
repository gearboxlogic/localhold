use std::path::Path;

pub(super) fn is_unanalyzed_path(path: &str) -> bool {
    Path::new(path).extension().and_then(|extension| extension.to_str()).is_some_and(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "cjs" | "js" | "jsx" | "lua" | "mjs" | "pl" | "pm" | "php" | "rb" | "tcl" | "ts" | "tsx"
        )
    })
}

pub(super) fn is_unanalyzed_interpreter(command: &str) -> bool {
    matches!(
        command,
        "bun" | "bun.exe" | "cmake" | "cmake.exe" | "deno" | "deno.exe" | "lua" | "lua.exe" | "node" | "node.exe" | "perl" | "perl.exe" | "php" | "php.exe" | "ruby" | "ruby.exe"
    ) || is_tcl_interpreter(command)
}

fn is_tcl_interpreter(command: &str) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    ["tclsh", "wish"].iter().any(|name| {
        command == *name
            || command
                .strip_prefix(name)
                .is_some_and(|version| !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.'))
    })
}

#[cfg(test)]
mod tests {
    use super::{is_unanalyzed_interpreter, is_unanalyzed_path};

    #[test]
    fn javascript_programs_fail_closed() {
        for path in ["quality/lint.js", "quality/lint.CJS", "quality/lint.mjs"] {
            assert!(is_unanalyzed_path(path), "{path}");
        }
        assert!(!is_unanalyzed_path("quality/lint.json"));
    }

    #[test]
    fn tcl_interpreters_and_versioned_names_fail_closed() {
        for command in ["tclsh", "tclsh.exe", "tclsh8.6", "tclsh86.exe", "wish", "wish8.7.exe"] {
            assert!(is_unanalyzed_interpreter(command), "{command}");
        }
        assert!(!is_unanalyzed_interpreter("wishful"));
        assert!(!is_unanalyzed_interpreter("tclsh-preview"));
    }

    #[test]
    fn cmake_language_execution_fails_closed() {
        assert!(is_unanalyzed_interpreter("cmake"));
        assert!(is_unanalyzed_interpreter("cmake.exe"));
    }
}
