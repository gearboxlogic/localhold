pub(super) fn dispatch_is_opaque(arguments: &[String]) -> bool {
    command_producing_subcommand(arguments) || configuration_is_opaque(arguments)
}

fn configuration_is_opaque(arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--config-env" || argument.starts_with("--config-env=") {
            return true;
        }
        let configuration = if argument == "-c" {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else {
            argument.strip_prefix("-c").filter(|configuration| !configuration.is_empty())
        };
        if configuration.is_some_and(|configuration| !is_safe_configuration(configuration)) {
            return true;
        }
        if argument == "-c" && configuration.is_none() {
            return true;
        }
        index += 1;
    }
    false
}

fn command_producing_subcommand(arguments: &[String]) -> bool {
    let Some((index, subcommand)) = git_subcommand(arguments) else {
        return false;
    };
    match subcommand.to_ascii_lowercase().as_str() {
        "difftool" | "filter-branch" | "mergetool" => true,
        "bisect" => arguments[index + 1..]
            .iter()
            .take_while(|argument| argument.as_str() != "--")
            .any(|argument| argument.eq_ignore_ascii_case("run")),
        _ => false,
    }
}

fn git_subcommand(arguments: &[String]) -> Option<(usize, &str)> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            return None;
        }
        if matches!(
            argument.as_str(),
            "-C" | "-c" | "--config-env" | "--exec-path" | "--git-dir" | "--namespace" | "--super-prefix" | "--work-tree"
        ) {
            index += 2;
            continue;
        }
        if argument.starts_with('-') {
            index += 1;
            continue;
        }
        return Some((index, argument));
    }
    None
}

fn is_safe_configuration(configuration: &str) -> bool {
    let Some((name, value)) = configuration.split_once('=') else {
        return false;
    };
    let name = name.trim().to_ascii_lowercase();
    let value = value.trim();
    match name.as_str() {
        "core.autocrlf" => is_boolean(value),
        "core.fsmonitor" => matches!(value.to_ascii_lowercase().as_str(), "false" | "no" | "off" | "0"),
        "core.hookspath" => value == "/dev/null" || value.eq_ignore_ascii_case("NUL"),
        "user.email" | "user.name" => true,
        _ => false,
    }
}

fn is_boolean(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1" | "false" | "no" | "off" | "0")
}

#[cfg(test)]
mod tests {
    use super::dispatch_is_opaque;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn only_reviewed_non_executing_configuration_is_allowed() {
        assert!(dispatch_is_opaque(&arguments(&["-c", "alias.lint=!sh quality/lint.txt", "lint"])));
        assert!(dispatch_is_opaque(&arguments(&["-cALIAS.lint=!sh quality/lint.txt", "lint"])));
        assert!(dispatch_is_opaque(&arguments(&["-c", "core.fsmonitor=sh quality/lint.txt", "status"])));
        assert!(dispatch_is_opaque(&arguments(&["-c", "core.hooksPath=quality/hooks", "commit"])));
        assert!(dispatch_is_opaque(&arguments(&["--config-env=core.fsmonitor=FSMONITOR", "status"])));
        assert!(dispatch_is_opaque(&arguments(&["--config-env", "alias.lint=LINT_ALIAS", "lint"])));
        assert!(dispatch_is_opaque(&arguments(&["-c", "safe.directory=.", "status"])));
        assert!(dispatch_is_opaque(&arguments(&["-c"])));
        assert!(!dispatch_is_opaque(&arguments(&["-c", "core.autocrlf=false", "status"])));
        assert!(!dispatch_is_opaque(&arguments(&["-ccore.fsmonitor=false", "status"])));
        assert!(!dispatch_is_opaque(&arguments(&["-c", "core.hooksPath=/dev/null", "status"])));
        assert!(!dispatch_is_opaque(&arguments(&["-c", "user.name=LocalHold", "status"])));
    }

    #[test]
    fn command_producing_git_dispatch_fails_closed() {
        assert!(dispatch_is_opaque(&arguments(&[
            "difftool",
            "--no-prompt",
            "--extcmd=sh quality/lint.txt",
            "--no-index",
            "/etc/hosts",
            "/etc/passwd",
        ])));
        assert!(dispatch_is_opaque(&arguments(&["-C", "repository", "difftool", "--tool", "custom"])));
        assert!(dispatch_is_opaque(&arguments(&["mergetool", "--tool=custom"])));
        assert!(dispatch_is_opaque(&arguments(&["bisect", "run", "sh", "quality/lint.txt"])));
        assert!(dispatch_is_opaque(&arguments(&["bisect", "--no-checkout", "run", "sh", "quality/lint.txt"])));
        assert!(dispatch_is_opaque(&arguments(&["filter-branch", "--tree-filter", "sh quality/lint.txt", "--", "HEAD",])));
        assert!(!dispatch_is_opaque(&arguments(&["bisect", "start", "--", "run"])));
        assert!(!dispatch_is_opaque(&arguments(&["diff", "--", "difftool"])));
    }
}
