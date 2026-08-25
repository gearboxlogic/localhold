pub(super) fn is_reviewed_call(arguments: &[String]) -> bool {
    reviewed_repository_call(arguments)
        || reviewed_candidate_call(arguments)
        || arguments == ["-c", "core.autocrlf=false", "clone", "-q", "--no-hardlinks", "$test_repository", "$gate_candidate"]
}

fn reviewed_repository_call(arguments: &[String]) -> bool {
    let Some(arguments) = strip_prefix(arguments, &["-C", "$test_repository"]) else {
        return false;
    };
    arguments == ["init", "-q"]
        || arguments == ["rev-parse", "HEAD"]
        || arguments == ["show", "$test_base:tools/maintainability/Cargo.toml"]
        || arguments == ["-c", "core.autocrlf=false", "worktree", "add", "-q", "--detach", "$trusted_gate", "$test_base"]
        || arguments == ["worktree", "add", "-q", "--detach", "$historical_repository", "$test_base"]
        || reviewed_add(arguments, "$test_repository")
        || reviewed_commit(arguments)
}

fn reviewed_candidate_call(arguments: &[String]) -> bool {
    let Some(arguments) = strip_prefix(arguments, &["-C", "$gate_candidate"]) else {
        return false;
    };
    arguments == ["rev-parse", "HEAD"] || reviewed_add(arguments, "$gate_candidate") || reviewed_commit(arguments)
}

fn reviewed_add(arguments: &[String], root: &str) -> bool {
    let prefix = [
        "-c",
        "core.autocrlf=false",
        "-c",
        "user.name=LocalHold",
        "-c",
        "user.email=localhold@example.invalid",
        "add",
    ];
    let Some(files) = strip_prefix(arguments, &prefix) else {
        return false;
    };
    match root {
        "$test_repository" => {
            matches!(
                files,
                [file] if matches!(file.as_str(), "." | "src/lib.rs")
            ) || files == ["tools/maintainability/src/main.rs", "tools/maintainability/src/untrusted.rs"]
                || files
                    == [
                        "script/check-maintainability-bootstrap.sh",
                        "tools/maintainability/Cargo.lock",
                        "tools/maintainability/Cargo.toml",
                        "tools/untrusted",
                    ]
                || files == ["script/check-maintainability-bootstrap.sh", "tools/maintainability/Cargo.toml"]
        }
        "$gate_candidate" => files == ["script/run-maintainability-gate.sh"],
        _ => false,
    }
}

fn reviewed_commit(arguments: &[String]) -> bool {
    let prefix = ["-c", "user.name=LocalHold", "-c", "user.email=localhold@example.invalid", "commit", "-qm"];
    let Some([message]) = strip_prefix(arguments, &prefix) else {
        return false;
    };
    matches!(
        message.as_str(),
        "reviewed fixture" | "untrusted checker head" | "untrusted candidate gate" | "second pushed commit" | "untrusted checker dependency graph" | "untrusted checker lock graph"
    )
}

fn strip_prefix<'a>(arguments: &'a [String], prefix: &[&str]) -> Option<&'a [String]> {
    arguments
        .get(..prefix.len())
        .filter(|candidate| candidate.iter().map(String::as_str).eq(prefix.iter().copied()))
        .map(|_| &arguments[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::is_reviewed_call;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn fixture_git_profile_is_surface_specific_and_exact() {
        assert!(is_reviewed_call(&arguments(&["-C", "$test_repository", "rev-parse", "HEAD"])));
        assert!(is_reviewed_call(&arguments(&[
            "-C",
            "$test_repository",
            "-c",
            "user.name=LocalHold",
            "-c",
            "user.email=localhold@example.invalid",
            "commit",
            "-qm",
            "reviewed fixture",
        ])));
        assert!(!is_reviewed_call(&arguments(&["-C", "$test_repository", "commit", "-m", "arbitrary"])));
        assert!(!is_reviewed_call(&arguments(&["-C", "$test_repository", "-c", "alias.lint=!sh quality/lint.txt", "lint"])));
    }
}
