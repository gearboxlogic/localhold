use std::path::Path;

pub(super) fn is_unanalyzed_path(path: &str) -> bool {
    Path::new(path).extension().and_then(|extension| extension.to_str()).is_some_and(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "cjs" | "js" | "jse" | "jsx" | "lua" | "mjs" | "pl" | "pm" | "php" | "rb" | "swift" | "tcl" | "ts" | "tsx" | "vbe" | "vbs" | "wsf" | "wsh"
        )
    })
}

pub(super) fn is_unanalyzed_interpreter(command: &str) -> bool {
    matches!(
        command,
        "ant"
            | "ant.bat"
            | "ant.cmd"
            | "ant.exe"
            | "bun"
            | "bun.exe"
            | "cmake"
            | "cmake.exe"
            | "ctest"
            | "ctest.exe"
            | "cscript"
            | "cscript.exe"
            | "deno"
            | "deno.exe"
            | "docker"
            | "docker-compose"
            | "docker-compose.exe"
            | "docker.exe"
            | "dotnet"
            | "dotnet.exe"
            | "finch"
            | "finch.exe"
            | "gradle"
            | "gradle.bat"
            | "gradle.cmd"
            | "gradle.exe"
            | "gradlew"
            | "gradlew.bat"
            | "gradlew.cmd"
            | "gradlew.exe"
            | "java"
            | "java.exe"
            | "javaw"
            | "javaw.exe"
            | "javac"
            | "javac.exe"
            | "lua"
            | "lua.exe"
            | "ninja"
            | "ninja.exe"
            | "node"
            | "node.exe"
            | "nerdctl"
            | "nerdctl.exe"
            | "npm"
            | "npm.exe"
            | "npx"
            | "npx.exe"
            | "perl"
            | "perl.exe"
            | "php"
            | "php.exe"
            | "podman"
            | "podman-compose"
            | "podman-compose.exe"
            | "podman.exe"
            | "ruby"
            | "ruby.exe"
            | "swift"
            | "swift.exe"
            | "wscript"
            | "wscript.exe"
            | "yarn"
            | "yarn.cmd"
            | "yarn.exe"
            | "yarn.ps1"
    ) || is_python_package_installer(command)
        || is_tcl_interpreter(command)
}

pub(super) fn is_python_interpreter(command: &str) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    matches!(command, "py" | "pyw") || ["python", "pythonw", "pypy"].iter().any(|name| is_versioned_name(command, name))
}

fn is_python_package_installer(command: &str) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    is_versioned_name(command, "pip")
}

fn is_versioned_name(command: &str, name: &str) -> bool {
    command == name
        || command.strip_prefix(name).is_some_and(|version| {
            !version.is_empty()
                && version.bytes().next().is_some_and(|byte| byte.is_ascii_digit())
                && version.bytes().next_back().is_some_and(|byte| byte.is_ascii_digit())
                && version.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.')
        })
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
    use super::{is_python_interpreter, is_unanalyzed_interpreter, is_unanalyzed_path};

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
    fn build_language_execution_fails_closed() {
        for command in [
            "ant",
            "ant.bat",
            "ant.cmd",
            "ant.exe",
            "cmake",
            "cmake.exe",
            "ctest",
            "ctest.exe",
            "gradle",
            "gradle.bat",
            "gradle.cmd",
            "gradle.exe",
            "gradlew",
            "gradlew.bat",
            "gradlew.cmd",
            "gradlew.exe",
            "ninja",
            "ninja.exe",
        ] {
            assert!(is_unanalyzed_interpreter(command), "{command}");
        }
    }

    #[test]
    fn swift_execution_fails_closed() {
        assert!(is_unanalyzed_path("quality/lint.swift"));
        for command in ["swift", "swift.exe"] {
            assert!(is_unanalyzed_interpreter(command), "{command}");
        }
    }

    #[test]
    fn package_and_java_launchers_fail_closed() {
        for command in [
            "npm",
            "npm.exe",
            "npx",
            "npx.exe",
            "pip",
            "pip.exe",
            "pip3",
            "pip3.12",
            "pip3.12.exe",
            "yarn",
            "yarn.cmd",
            "yarn.exe",
            "yarn.ps1",
            "java",
            "java.exe",
            "javaw",
            "javaw.exe",
            "javac",
            "javac.exe",
        ] {
            assert!(is_unanalyzed_interpreter(command), "{command}");
        }
        for command in ["pipeline", "pip-preview", "pip3."] {
            assert!(!is_unanalyzed_interpreter(command), "{command}");
        }
    }

    #[test]
    fn container_launchers_fail_closed() {
        for command in [
            "docker",
            "docker.exe",
            "docker-compose",
            "finch",
            "nerdctl",
            "nerdctl.exe",
            "podman",
            "podman.exe",
            "podman-compose",
        ] {
            assert!(is_unanalyzed_interpreter(command), "{command}");
        }
    }

    #[test]
    fn windows_script_host_and_dotnet_execution_fail_closed() {
        for path in ["quality/lint.vbs", "quality/lint.VBE", "quality/lint.jse", "quality/lint.wsf", "quality/lint.wsh"] {
            assert!(is_unanalyzed_path(path), "{path}");
        }
        for command in ["cscript", "cscript.exe", "wscript", "wscript.exe", "dotnet", "dotnet.exe"] {
            assert!(is_unanalyzed_interpreter(command), "{command}");
        }
    }

    #[test]
    fn versioned_python_interpreters_are_recognized_without_prefix_collisions() {
        for command in [
            "py",
            "py.exe",
            "pyw.exe",
            "python",
            "python.exe",
            "python3",
            "python3.12",
            "python3.12.exe",
            "python312.exe",
            "pythonw.exe",
            "pythonw3.12.exe",
            "pypy",
            "pypy3",
            "pypy3.11.exe",
        ] {
            assert!(is_python_interpreter(command), "{command}");
        }
        for command in ["python-preview", "python3.", "pythonic", "pythonw-preview", "pypical"] {
            assert!(!is_python_interpreter(command), "{command}");
        }
    }
}
