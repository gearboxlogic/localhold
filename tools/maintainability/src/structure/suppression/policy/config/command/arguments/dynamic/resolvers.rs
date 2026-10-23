use super::super::tokens;

const TRUSTED_SYSTEM_COMMAND: &str = r#"trusted_system_command() {
    local name=$1
    local candidate
    candidate=$(type -P -- "$name") || {
        printf 'maintainability bootstrap requires command: %s\n' "$name" >&2
        exit 1
    }
    local directory=${candidate%/*}
    [[ $directory != "$candidate" ]] || directory=.
    directory=$(cd -- "$directory" && pwd -P)
    candidate="$directory/${candidate##*/}"
    case "$directory" in
        /bin | /usr/bin | /mingw64/bin) ;;
        *)
            printf 'maintainability bootstrap requires an OS-owned command: %s\n' "$candidate" >&2
            exit 1
            ;;
    esac
    if [[ ! -f "$candidate" || ! -x "$candidate" ]]; then
        printf 'maintainability bootstrap requires an executable system file: %s\n' "$candidate" >&2
        exit 1
    fi
    printf '%s\n' "$candidate"
}"#;

const GATE_SHA256_FILE: &str = r#"sha256_file() {
    local output
    output=$("$sha256_command" -- "$1")
    printf '%s\n' "${output%%[[:space:]]*}"
}"#;

const AUTHENTICATED_TOOL: &str = r#"authenticated_tool() {
    local path=$1
    local expected=$2
    if [[ ! -f $path || -L $path || ! -x $path ]]; then
        printf 'pinned Rust tool must be a regular non-symlink executable: %s\n' "$path" >&2
        exit 1
    fi
    local actual
    actual=$(sha256_file "$path")
    if [[ $actual != "$expected" ]]; then
        printf 'reviewed Rust tool digest differs: %s\n' "$path" >&2
        exit 1
    fi
    printf '%s\n' "$path"
}"#;

pub(super) struct Candidate<'a> {
    pub(super) path: &'a str,
    pub(super) name: &'a str,
    pub(super) resolver: &'a str,
    pub(super) resolver_command: &'a [String],
    pub(super) assignment_command: &'a [String],
    pub(super) source: &'a str,
    pub(super) source_is_reviewed: bool,
}

pub(super) fn is_reviewed(candidate: &Candidate<'_>) -> bool {
    if !candidate.source_is_reviewed {
        return false;
    }
    if candidate.resolver == "trusted_system_command" && candidate.path == "script/check-maintainability-bootstrap.sh" {
        let expected_tool = candidate
            .name
            .strip_suffix("_command")
            .map_or(candidate.name, |tool| if tool == "sha256" { "sha256sum" } else { tool });
        return matches!(
            candidate.assignment_command,
            [assignment, tool]
                if assignment == &format!("{}=$(trusted_system_command", candidate.name)
                    && tool == &format!("{expected_tool})")
                    && has_trusted_system_command(candidate.source)
        );
    }
    candidate.path == "script/run-maintainability-gate.sh"
        && match candidate.resolver {
            "authenticated_tool" => {
                reviewed_authenticated_tool(candidate.name, candidate.resolver_command)
                    && exact_function(candidate.source, "authenticated_tool", AUTHENTICATED_TOOL)
                    && exact_function(candidate.source, "sha256_file", GATE_SHA256_FILE)
            }
            "sha256_file" => exact_function(candidate.source, "sha256_file", GATE_SHA256_FILE),
            "$cygpath_command" => candidate.name == "rustup_executable" && candidate.resolver_command == ["$cygpath_command", "-u", "$rustup_executable"],
            _ => false,
        }
}

fn reviewed_authenticated_tool(name: &str, command: &[String]) -> bool {
    matches!(
        (name, command),
        ("rustup_executable", [resolver, path, digest])
            if resolver == "authenticated_tool" && path == "$rustup_executable" && digest == "$expected_rustup_sha256"
    ) || matches!(
        (name, command),
        ("cargo_executable", [resolver, path, digest])
            if resolver == "authenticated_tool" && path == "$toolchain_bin/cargo$tool_extension" && digest == "$expected_cargo_sha256"
    ) || matches!(
        (name, command),
        ("cargo_clippy_executable", [resolver, path, digest])
            if resolver == "authenticated_tool"
                && path == "$toolchain_bin/cargo-clippy$tool_extension"
                && digest == "$expected_cargo_clippy_sha256"
    ) || matches!(
        (name, command),
        ("cargo_fmt_executable", [resolver, path, digest])
            if resolver == "authenticated_tool" && path == "$toolchain_bin/cargo-fmt$tool_extension" && digest == "$expected_cargo_fmt_sha256"
    )
}

pub(super) fn has_trusted_system_command(source: &str) -> bool {
    exact_function(source, "trusted_system_command", TRUSTED_SYSTEM_COMMAND)
        || exact_function(
            source,
            "trusted_system_command",
            &TRUSTED_SYSTEM_COMMAND.replace("/mingw64/bin)", "/mingw64/bin | /ucrt64/bin)"),
        )
}

fn exact_function(source: &str, name: &str, definition: &str) -> bool {
    let command_source = tokens::without_noncommand_shell_data(source);
    let mut definitions = source.match_indices(definition);
    let Some((offset, _)) = definitions.next() else {
        return false;
    };
    if definitions.next().is_some() {
        return false;
    }
    let line_index = source[..offset].bytes().filter(|byte| *byte == b'\n').count();
    let Some(declaration) = definition.lines().next() else {
        return false;
    };
    command_source.lines().nth(line_index).is_some_and(|line| line.trim_start() == declaration) && tokens::declared_shell_function_count(&command_source, name) == 1
}

pub(super) fn is_reviewed_literal_program_assignment(path: &str, name: &str, value: &str) -> bool {
    matches!(
        (path, name, value),
        ("script/run-maintainability-gate.sh", "sha256_command", "/usr/bin/sha256sum")
            | ("script/run-maintainability-gate.sh", "uname_command", "/usr/bin/uname")
            | ("script/run-maintainability-gate.sh", "bash_command", "/usr/bin/bash")
            | ("script/run-maintainability-gate.sh", "chmod_command", "/usr/bin/chmod")
            | ("script/run-maintainability-gate.sh", "cp_command", "/usr/bin/cp")
            | ("script/run-maintainability-gate.sh", "env_command", "/usr/bin/env")
            | ("script/run-maintainability-gate.sh", "ln_command", "/usr/bin/ln")
            | ("script/run-maintainability-gate.sh", "mkdir_command", "/usr/bin/mkdir")
            | ("script/run-maintainability-gate.sh", "mktemp_command", "/usr/bin/mktemp")
            | ("script/run-maintainability-gate.sh", "rm_command", "/usr/bin/rm")
            | (
                "script/run-maintainability-gate.sh",
                "curl_command",
                "/usr/bin/curl" | "/mingw64/bin/curl.exe" | "/ucrt64/bin/curl.exe"
            )
            | ("script/run-maintainability-gate.sh", "cygpath_command", "/usr/bin/cygpath")
            | (
                "script/run-maintainability-gate.sh",
                "vswhere_command",
                "/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
            )
            | ("script/test-postgres-smoke.sh", "container_cli", "docker" | "podman")
            | (".github/workflows/ci.yml", "binary", "target/x86_64-pc-windows-msvc/release/hold.exe")
    )
}

#[cfg(test)]
mod tests {
    use super::{AUTHENTICATED_TOOL, GATE_SHA256_FILE, TRUSTED_SYSTEM_COMMAND, exact_function};

    #[test]
    fn ucrt_system_resolver_retains_exact_directory_boundary() {
        let updated = TRUSTED_SYSTEM_COMMAND.replace("/mingw64/bin)", "/mingw64/bin | /ucrt64/bin)");
        assert!(super::has_trusted_system_command(TRUSTED_SYSTEM_COMMAND));
        assert!(super::has_trusted_system_command(&updated));
        assert!(!super::has_trusted_system_command(&updated.replace("/ucrt64/bin", "/tmp/bin")));
        assert!(!super::has_trusted_system_command(&updated.replace("/ucrt64/bin", "/ucrt64/*")));
        assert!(!super::has_trusted_system_command(&updated.replace("type -P", "printf /tmp/helper")));
    }

    #[test]
    fn resolver_functions_are_whole_definition_profiles() {
        assert!(exact_function(TRUSTED_SYSTEM_COMMAND, "trusted_system_command", TRUSTED_SYSTEM_COMMAND));
        assert!(!exact_function(
            &TRUSTED_SYSTEM_COMMAND.replace("type -P", "printf /tmp/helper"),
            "trusted_system_command",
            TRUSTED_SYSTEM_COMMAND
        ));
        let gate = format!("{GATE_SHA256_FILE}\n\n{AUTHENTICATED_TOOL}\n");
        assert!(exact_function(&gate, "sha256_file", GATE_SHA256_FILE));
        assert!(exact_function(&gate, "authenticated_tool", AUTHENTICATED_TOOL));
        assert!(!exact_function(
            &format!("{gate}\nfunction authenticated_tool\n{{\n  printf /tmp/helper\n}}\n"),
            "authenticated_tool",
            AUTHENTICATED_TOOL
        ));
        assert!(!exact_function(
            &format!("cat <<'PROFILE'\n{AUTHENTICATED_TOOL}\nPROFILE\nfunction authenticated_tool\n{{\n  printf /tmp/helper\n}}\n"),
            "authenticated_tool",
            AUTHENTICATED_TOOL
        ));
    }
}
