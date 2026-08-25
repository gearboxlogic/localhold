use std::collections::BTreeSet;

mod sources;
mod workflows;
use sources::SOURCE_PROFILES;
use workflows::WORKFLOW_ARGUMENT_PROFILES;

struct ArgumentProfile {
    path: &'static str,
    command: &'static str,
    arguments: &'static [&'static str],
}

const FIXTURE: &str = "script/tests/test_maintainability_bootstrap.sh";

const ARGUMENT_PROFILES: &[ArgumentProfile] = &[
    profile("Justfile", "cargo", &["nextest", "run", "{{", "ARGS", "}}"]),
    profile("Justfile", "cargo-fmt", &["--all"]),
    profile("Justfile", "cargo-fmt", &["--all", "--", "--check"]),
    profile(
        "Justfile",
        "cargo",
        &[
            "run",
            "--manifest-path",
            "tools/dependency-unsafe/Cargo.toml",
            "--locked",
            "--",
            "generate",
            "--platform",
            "{{",
            "PLATFORM",
            "}}",
        ],
    ),
    profile("Justfile", "mise", &["install"]),
    profile("Justfile", "mise", &["lock"]),
    profile("script/bootstrap.sh", "mise", &["install", "--locked"]),
    profile("script/bootstrap.sh", "mise", &["install"]),
    profile("script/bootstrap.sh", "mise", &["lock"]),
    profile(".github/workflows/trusted-maintainability.yml", "git", &["rev-parse", "--verify", "${remote_ref}^{commit}"]),
    profile(FIXTURE, "check", &["--root", "$test_repository", "$@"]),
    profile(FIXTURE, "check-maintainability-bootstrap.sh", &["$@"]),
    profile(FIXTURE, "trusted_check", &["--root", "$test_repository", "--test-environment", ">/dev/null"]),
    profile(FIXTURE, "trusted_check", &["--root", "$gate_candidate", "--test-environment", ">/dev/null"]),
    profile(FIXTURE, "trusted_rustup_command", &["which", "--toolchain", "1.97.0", "cargo"]),
    profile(FIXTURE, "check-maintainability-bootstrap.sh", &[]),
    profile(
        FIXTURE,
        "git",
        &["-C", "$test_repository", "show", "$test_base:tools/maintainability/Cargo.toml", ">$test_tool/Cargo.toml"],
    ),
    profile(FIXTURE, "sed", &["-n", "s/^[[:space:]]*LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256: //p", "$ci_workflow"]),
    profile(FIXTURE, "find", &["$1", "-prune", "-perm", "/222", "-print"]),
    profile(FIXTURE, "cp", &["$source_tool/Cargo.toml", "$test_tool/Cargo.toml"]),
    profile(FIXTURE, "cp", &["$source_tool/Cargo.lock", "$test_tool/Cargo.lock"]),
    profile(FIXTURE, "cp", &["$repository_root/Justfile", "$test_repository/Justfile"]),
    profile(FIXTURE, "cp", &["$repository_root/mise.toml", "$test_repository/mise.toml"]),
    profile(FIXTURE, "cp", &["$repository_root/mise.lock", "$test_repository/mise.lock"]),
    profile(FIXTURE, "cp", &["$check", "$test_repository/script/check-maintainability-bootstrap.sh"]),
    profile(FIXTURE, "cp", &["$source_runner", "$test_repository/script/run-source-safety.sh"]),
    profile(FIXTURE, "cp", &["$source_gate_runner", "$test_repository/script/run-maintainability-gate.sh"]),
    profile(
        FIXTURE,
        "cp",
        &["$source_bootstrap_tests", "$test_repository/script/tests/test_maintainability_bootstrap.sh"],
    ),
    profile(FIXTURE, "cp", &["$trusted_toolchain_bin/cargo$tool_extension", "$fake_toolchain_bin/cargo$tool_extension"]),
    profile(FIXTURE, "cp", &["$trusted_toolchain_bin/cargo$tool_extension", "$fake_toolchain_bin/rustc$tool_extension"]),
    profile(FIXTURE, "cp", &["$trusted_toolchain_bin/$tool$tool_extension", "$fake_toolchain_bin/$tool$tool_extension"]),
    profile(FIXTURE, "cp", &["$trusted_toolchain_root/$runtime_library", "$fake_toolchain_root/$runtime_library"]),
    profile(FIXTURE, "cp", &["-R", "$source_tool/src", "$test_tool/src"]),
    profile(FIXTURE, "chmod", &["-R", "u+w", "--", "$test_repository"]),
    profile(FIXTURE, "chmod", &["+x", "$inherited_config_helper"]),
    profile(FIXTURE, "chmod", &["+x", "$fake_bin/just", "$fake_bin/$cargo_name", "$fake_bin/rustup"]),
    profile(FIXTURE, "chmod", &["+x", "$fake_system_bin/git"]),
    profile(FIXTURE, "chmod", &["+x", "$tar_options_helper"]),
    profile(
        FIXTURE,
        "chmod",
        &["+x", "$fake_toolchain_bin/cargo$tool_extension", "$fake_toolchain_bin/rustc$tool_extension"],
    ),
    profile(FIXTURE, "rm", &["$test_tool/build.rs"]),
    profile(FIXTURE, "rm", &["$test_tool/Cargo.lock"]),
    profile(FIXTURE, "rm", &["-rf", "$test_tool/src"]),
    profile(FIXTURE, "rm", &["-r", "$test_repository/.cargo"]),
    profile(FIXTURE, "rm", &["-r", "$fixture/.cargo"]),
    profile(FIXTURE, "rm", &["-r", "$cargo_home"]),
    profile(FIXTURE, "rm", &["-rf", "--", "$fixture"]),
    profile(
        FIXTURE,
        "sed",
        &[
            "-i",
            "/^name = \"localhold-maintainability\"$/,/^]$/ { /^dependencies = \\[$/a\\ \"untrusted\",\n}",
            "$test_tool/Cargo.lock",
        ],
    ),
    profile(
        FIXTURE,
        "sed",
        &[
            "-i",
            "s/^readonly reviewed_manifest_sha256=.*/readonly reviewed_manifest_sha256=$untrusted_manifest_sha256/",
            "$test_repository/script/check-maintainability-bootstrap.sh",
        ],
    ),
    profile(
        FIXTURE,
        "sed",
        &[
            "-i",
            "s/^readonly reviewed_lockfile_sha256=.*/readonly reviewed_lockfile_sha256=$untrusted_lockfile_sha256/",
            "$test_repository/script/check-maintainability-bootstrap.sh",
        ],
    ),
    profile(
        FIXTURE,
        "sed",
        &[
            "-i",
            "s/^readonly reviewed_manifest_sha256=.*/readonly reviewed_manifest_sha256=$trusted_manifest_sha256/",
            "$test_repository/script/check-maintainability-bootstrap.sh",
        ],
    ),
    profile("script/install.sh", "install", &["-m", "0755", "$build_dir/release/hold", "$bin_dir/hold"]),
    profile(
        "script/install.sh",
        "cargo_command",
        &["build", "--release", "--locked", "--features", "reranker", "--target-dir", "$build_dir"],
    ),
    profile(
        "script/install.sh",
        "cargo_command",
        &["build", "--release", "--locked", "--features", "reranker-cuda", "--target-dir", "$build_dir"],
    ),
    profile(
        "script/install.sh",
        "install",
        &["-m", "0644", "localhold.example.toml", "$share_dir/localhold.example.toml"],
    ),
    profile("script/install.sh", "install", &["-m", "0644", "LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.md", "$doc_dir/"]),
    profile(
        "script/test-postgres-smoke.sh",
        "sed",
        &[
            "-E",
            "-e",
            "s#(postgres(ql)?://)[^/@[:space:]]+:[^/@[:space:]]+@#\\1[redacted]@#g",
            "-e",
            "s/(POSTGRES_PASSWORD=)[^[:space:]]+/\\1[redacted]/g",
            "-e",
            "s/(password[= ]+)[^[:space:]]+/\\1[redacted]/Ig",
            ">&2",
        ],
    ),
    profile("script/check-maintainability-bootstrap.sh", "bash_command", &["$gate_runner", "$mode"]),
    profile(
        "script/check-maintainability-bootstrap.sh",
        "find_command",
        &["$source_root", "(", "-type", "f", "-o", "-type", "l", ")", "-print0"],
    ),
    profile("script/check-maintainability-bootstrap.sh", "cygpath_command", &["-m", "$directory", "2>/dev/null"]),
    profile("script/check-maintainability-bootstrap.sh", "find_command", &["$path", "-prune", "-perm", "/222", "-print"]),
    profile("script/check-maintainability-bootstrap.sh", "cygpath_command", &["-u", "$cargo_home"]),
    profile(
        "script/check-maintainability-bootstrap.sh",
        "awk_command",
        &[
            "\n        /^\\[package\\][[:space:]]*(#.*)?$/ { in_package = 1; next }\n        /^\\[/ { in_package = 0 }\n        in_package && /^[[:space:]]*build[[:space:]]*=/ {\n            value = $0\n            sub(/^[^=]*=[[:space:]]*/, \"\", value)\n            sub(/[[:space:]]*#.*/, \"\", value)\n            gsub(/[[:space:]]/, \"\", value)\n            print value\n        }\n    ",
            "$manifest",
        ],
    ),
    profile("script/check-maintainability-bootstrap.sh", "sha256_command", &["--", "$path"]),
    profile("script/check-maintainability-bootstrap.sh", "cygpath_command", &["-w", "$git_executable"]),
    profile("script/check-maintainability-bootstrap.sh", "mktemp_command", &["-d", "$target_parent/s.XXXXXXXX"]),
    profile("script/check-maintainability-bootstrap.sh", "mkdir_command", &["--", "$target_parent"]),
    profile("script/check-maintainability-bootstrap.sh", "rmdir_command", &["--", "$snapshot_root"]),
    profile(
        "script/check-maintainability-bootstrap.sh",
        "chmod_command",
        &["-R", "u+w", "--", "$snapshot_root", "2>/dev/null"],
    ),
    profile("script/check-maintainability-bootstrap.sh", "rm_command", &["-rf", "--", "$snapshot_root"]),
    profile("script/check-maintainability-bootstrap.sh", "mkdir_command", &["--", "$evidence_parent"]),
    profile("script/check-maintainability-bootstrap.sh", "rm_command", &["-rf", "--", "$destination"]),
    profile("script/check-maintainability-bootstrap.sh", "mv_command", &["--", "$evidence", "$destination"]),
    profile("script/check-maintainability-bootstrap.sh", "tar_command", &["-xf", "-", "-C", "$snapshot_root"]),
    profile(
        "script/check-maintainability-bootstrap.sh",
        "rm_command",
        &[
            "-rf",
            "--",
            "$snapshot_root/tools/maintainability/Cargo.toml",
            "$snapshot_root/tools/maintainability/Cargo.lock",
            "$snapshot_root/tools/maintainability/src",
        ],
    ),
    profile(
        "script/check-maintainability-bootstrap.sh",
        "mkdir_command",
        &["--", "$snapshot_root/target", "$audit_scratch_root"],
    ),
    profile("script/check-maintainability-bootstrap.sh", "chmod_command", &["-R", "a-w", "--", "$snapshot_root"]),
    profile(
        "script/check-maintainability-bootstrap.sh",
        "chmod_command",
        &["u+rwx", "--", "$snapshot_root/target", "$audit_scratch_root"],
    ),
    profile(
        "script/check-maintainability-bootstrap.sh",
        "bash_command",
        &["$snapshot_bootstrap", "--root", "$snapshot_root"],
    ),
    profile("script/check-maintainability-bootstrap.sh", "bash_command", &["$snapshot_gate_runner", "$mode"]),
    profile("script/run-maintainability-gate.sh", "mkdir_command", &["--", "$target_parent"]),
    profile("script/run-maintainability-gate.sh", "sha256_command", &["--", "$1"]),
    profile("script/run-maintainability-gate.sh", "mktemp_command", &["-d", "$target_parent/g.XXXXXXXX"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-u", "$rustup_home"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-w", "$rustup_home"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-u", "$rustup_executable"]),
    profile("script/run-maintainability-gate.sh", "rustup_executable", &["which", "--toolchain", "1.97.0", "cargo"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-u", "$resolved_cargo"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-w", "$native_cargo"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-w", "$native_cargo_clippy"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-w", "$native_cargo_fmt"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-w", "$native_rustc"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-w", "$native_rustdoc"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-w", "$native_rustfmt"]),
    profile(
        "script/run-maintainability-gate.sh",
        "vswhere_command",
        &["-nologo", "-latest", "-products", "*", "-find", "VC\\Tools\\MSVC\\**\\bin\\Hostx64\\x64\\link.exe"],
    ),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-u", "$linker_candidate"]),
    profile("script/run-maintainability-gate.sh", "cygpath_command", &["-w", "$fresh_cargo_home"]),
    profile("script/run-maintainability-gate.sh", "rm_command", &["-rf", "--", "$target_directory"]),
    profile(
        "script/run-maintainability-gate.sh",
        "curl_command",
        &[
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--output",
            "$downloaded_rustup",
            "$rustup_archive_url",
        ],
    ),
    profile("script/run-maintainability-gate.sh", "chmod_command", &["0700", "--", "$downloaded_rustup"]),
    profile("script/run-maintainability-gate.sh", "mkdir_command", &["--", "$fresh_cargo_home"]),
    profile("script/run-maintainability-gate.sh", "mkdir_command", &["--", "$compatibility_bin"]),
    profile("script/run-maintainability-gate.sh", "cp_command", &["--", "$rustup_executable", "$compatibility_rustc"]),
    profile(
        "script/run-maintainability-gate.sh",
        "ln_command",
        &["--", "$compatibility_rustc", "$compatibility_cargo_clippy"],
    ),
    profile(
        "script/run-maintainability-gate.sh",
        "ln_command",
        &["--", "$compatibility_rustc", "$compatibility_clippy_driver"],
    ),
    profile(
        "script/run-maintainability-gate.sh",
        "bash_command",
        &["$implementation_root/script/tests/test_maintainability_bootstrap.sh"],
    ),
    profile("script/run-maintainability-gate.sh", "bash_command", &["$implementation_root/script/run-source-safety.sh"]),
    profile(
        "script/run-maintainability-gate.sh",
        "cargo_executable",
        &["fetch", "--manifest-path", "$audit_manifest", "--locked"],
    ),
    profile(
        "script/run-maintainability-gate.sh",
        "cargo_fmt_executable",
        &["--manifest-path", "$audit_manifest", "--", "--check"],
    ),
    profile(
        "script/run-maintainability-gate.sh",
        "cargo_executable",
        &["test", "--manifest-path", "$audit_manifest", "--locked"],
    ),
    profile(
        "script/run-maintainability-gate.sh",
        "cargo_clippy_executable",
        &["clippy", "--manifest-path", "$audit_manifest", "--all-targets", "--locked", "--", "-D", "warnings"],
    ),
    profile(
        "script/run-maintainability-gate.sh",
        "cargo_executable",
        &["run", "--manifest-path", "$audit_manifest", "--locked", "--", "check"],
    ),
    profile(
        "script/run-source-safety.sh",
        "cargo_command",
        &["fetch", "--manifest-path", "$maintainability_manifest", "--locked"],
    ),
    profile(
        "script/run-source-safety.sh",
        "cargo_fmt_command",
        &["--manifest-path", "$maintainability_manifest", "--", "--check"],
    ),
    profile(
        "script/run-source-safety.sh",
        "cargo_command",
        &["test", "--manifest-path", "$maintainability_manifest", "--locked"],
    ),
    profile(
        "script/run-source-safety.sh",
        "cargo_clippy_command",
        &[
            "clippy",
            "--manifest-path",
            "$maintainability_manifest",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
    ),
    profile(
        "script/run-source-safety.sh",
        "cargo_command",
        &["run", "--manifest-path", "$maintainability_manifest", "--locked", "--", "check"],
    ),
    profile("script/claude-review.sh", "rm", &["-rf", "--", "$scratch_directory"]),
    profile("script/claude-review.sh", "ps", &["-A", "-o", "pgid=,stat="]),
    profile("script/claude-review.sh", "trap", &["HUP", "INT", "TERM"]),
    profile(
        "script/claude-review.sh",
        "claude_binary",
        &[
            "--safe-mode",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-chrome",
            "--no-session-persistence",
            "--model",
            "$model",
            "--effort",
            "high",
            "--permission-mode",
            "plan",
            "--tools",
            "Read,Grep,Glob,Bash",
            "--print",
            "--output-format",
            "text",
            "$@",
        ],
    ),
    profile("script/tests/test_claude_review.sh", "cat", &[">", "$capture/stdin"]),
    profile("script/tests/test_claude_review.sh", "printf", &["%s\\n", "$count", ">", "$capture/ps-count"]),
    profile("script/tests/test_claude_review.sh", ":", &[">", "$capture/simulate-zombie-group"]),
    profile("script/tests/test_claude_review.sh", ":", &[">", "$capture/simulate-live-group"]),
    profile("script/tests/test_claude_review.sh", "ps", &["-A", "-o", "pgid=,stat="]),
    profile("script/tests/test_claude_review.sh", "ps", &["-o", "stat=", "-p", "$pid", "2>/dev/null"]),
    profile("script/tests/test_claude_review.sh", "rm", &["-f", "--", "$capture/grandchild-ready"]),
    profile(
        "script/tests/test_claude_review.sh",
        "bash",
        &[
            "-c",
            "trap \"\" TERM; : > \"$1\"; while :; do sleep 1; done",
            "reviewer-grandchild",
            "$capture/grandchild-ready",
        ],
    ),
    profile("script/tests/test_claude_review.sh", "printf", &["%s\\n", "$!", ">", "$capture/grandchild-pid"]),
    profile("script/tests/test_claude_review.sh", "sort", &[">", "$capture/environment"]),
    profile(
        "script/tests/test_claude_review.sh",
        "claude-review.sh",
        &["opus", "Review the LocalHold diff.", ">", "$test_root/output"],
    ),
    profile("script/tests/test_claude_review.sh", "claude-review.sh", &["opus", ">", "$test_root/stdin-output"]),
    profile(
        "script/tests/test_claude_review.sh",
        "claude-review.sh",
        &["fable", "Fail this fake review.", ">", "$test_root/failure-output"],
    ),
    profile(
        "script/tests/test_claude_review.sh",
        "claude-review.sh",
        &["opus", "Wait for a termination signal.", ">", "$test_root/signal-output"],
    ),
    profile(
        "script/tests/test_claude_review.sh",
        "claude-review.sh",
        &["opus", "$prompt", ">", "$test_root/descendant-output", "2>", "$test_root/descendant-error"],
    ),
    profile(
        "script/tests/test_claude_review.sh",
        "ln",
        &["-s", "--", "$script_dir/test_claude_review.sh", "$test_root/bin/claude"],
    ),
    profile(
        "script/tests/test_claude_review.sh",
        "ln",
        &["-s", "--", "$script_dir/test_claude_review.sh", "$test_root/bin/ps"],
    ),
    profile("script/tests/test_claude_review.sh", "rm", &["-rf", "--", "$test_root"]),
    profile("script/tests/test_claude_review.sh", "rm", &["-rf", "--", "$test_root/capture"]),
    profile("script/tests/test_claude_review.sh", "rm", &["-rf", "--", "$signal_scratch"]),
];

const fn profile(path: &'static str, command: &'static str, arguments: &'static [&'static str]) -> ArgumentProfile {
    ArgumentProfile { path, command, arguments }
}

pub(super) fn accepts_dynamic_arguments(path: &str, source_is_reviewed: bool, command: &str, arguments: &[String]) -> bool {
    source_is_reviewed && matching_argument_profiles(path, command, arguments) == 1
}

pub(super) fn reviewed_sources() -> BTreeSet<(&'static str, &'static str)> {
    SOURCE_PROFILES.iter().map(|profile| (profile.id, profile.path)).collect()
}

fn matching_argument_profiles(path: &str, command: &str, arguments: &[String]) -> usize {
    argument_profiles()
        .filter(|profile| {
            profile.path == path
                && profile.command == command
                && profile.arguments.len() == arguments.len()
                && profile.arguments.iter().zip(arguments).all(|(expected, actual)| *expected == actual)
        })
        .count()
}

fn argument_profiles() -> impl Iterator<Item = &'static ArgumentProfile> {
    ARGUMENT_PROFILES.iter().chain(WORKFLOW_ARGUMENT_PROFILES)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use sha2::{Digest, Sha256};

    use super::super::super::super::super::profile_policy::{POLICY_PATH, ProfileManifest};
    use super::*;

    #[test]
    fn every_reviewed_argument_tuple_matches_once_and_is_exact() {
        for reviewed in argument_profiles() {
            let arguments = reviewed.arguments.iter().map(|argument| (*argument).to_owned()).collect::<Vec<_>>();
            assert_eq!(matching_argument_profiles(reviewed.path, reviewed.command, &arguments), 1, "{}", reviewed.path);
            assert!(accepts_dynamic_arguments(reviewed.path, true, reviewed.command, &arguments));
            assert!(!accepts_dynamic_arguments("script/other.sh", true, reviewed.command, &arguments));
            assert!(!accepts_dynamic_arguments(reviewed.path, true, "changed-command", &arguments));
            assert!(!accepts_dynamic_arguments(reviewed.path, false, reviewed.command, &arguments));

            let mut appended = arguments.clone();
            appended.push("changed-argument".to_owned());
            assert_mutation_requires_a_separate_profile(reviewed, &appended, "appended argument");
            for index in 0..arguments.len() {
                let mut changed = arguments.clone();
                changed[index].push_str("-changed");
                assert_mutation_requires_a_separate_profile(reviewed, &changed, "changed argument");

                let mut removed = arguments.clone();
                removed.remove(index);
                assert_mutation_requires_a_separate_profile(reviewed, &removed, "removed argument");
            }
        }
    }

    fn assert_mutation_requires_a_separate_profile(reviewed: &ArgumentProfile, arguments: &[String], mutation: &str) {
        let matches = matching_argument_profiles(reviewed.path, reviewed.command, arguments);
        assert!(
            matches <= 1,
            "{mutation} produced duplicate reviewed tuples for {} {:?}: {arguments:?}",
            reviewed.path,
            reviewed.command
        );
        assert_eq!(
            accepts_dynamic_arguments(reviewed.path, true, reviewed.command, arguments),
            matches == 1,
            "{mutation} was accepted without its own exact reviewed tuple for {} {:?}: {arguments:?}",
            reviewed.path,
            reviewed.command,
        );
    }

    #[test]
    fn embedded_source_profiles_cover_arguments_and_match_checked_in_bytes() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let policy = ProfileManifest::parse(&fs::read(workspace.join(POLICY_PATH)).expect("reviewed command profile policy")).expect("reviewed command profile policy");
        let profiles = policy
            .profiles()
            .iter()
            .map(|profile| (profile.id.as_str(), profile.path.as_str()))
            .collect::<BTreeSet<_>>();
        assert_eq!(reviewed_sources(), profiles);
        assert_eq!(SOURCE_PROFILES.len(), policy.profiles().len());
        for embedded in SOURCE_PROFILES {
            let profile = policy.profiles().iter().find(|profile| profile.id == embedded.id).expect("governed source profile");
            assert_eq!(profile.path, embedded.path);
            let source = fs::read(workspace.join(embedded.path)).expect("reviewed source");
            assert_eq!(format!("{:x}", Sha256::digest(source)), profile.current_sha256, "{}", embedded.path);
        }
    }
}
