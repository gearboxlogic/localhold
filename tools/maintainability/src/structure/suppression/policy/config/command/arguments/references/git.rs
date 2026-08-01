pub(super) fn dispatch_is_opaque(arguments: &[String]) -> bool {
    command_producing_subcommand(arguments) || configuration_is_opaque(arguments)
}

fn configuration_is_opaque(arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if matches!(argument.as_str(), "-p" | "--paginate") {
            return true;
        }
        if argument == "--exec-path" || argument.starts_with("--exec-path=") {
            return true;
        }
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
        "config" => config_dispatch_is_opaque(&arguments[index + 1..]),
        "grep" => grep_dispatch_is_opaque(&arguments[index + 1..]),
        "clone" => clone_dispatch_is_opaque(&arguments[index + 1..]),
        "fetch" => upload_pack_selection_is_opaque(&arguments[index + 1..]),
        "difftool" | "filter-branch" | "mergetool" => true,
        "bisect" => arguments[index + 1..]
            .iter()
            .take_while(|argument| argument.as_str() != "--")
            .any(|argument| argument.eq_ignore_ascii_case("run")),
        subcommand => !is_builtin_subcommand(subcommand),
    }
}

fn clone_dispatch_is_opaque(arguments: &[String]) -> bool {
    upload_pack_selection_is_opaque(arguments)
        || arguments
            .iter()
            .take_while(|argument| argument.as_str() != "--")
            .any(|argument| argument == "-u" || argument.starts_with("-u") && argument.len() > 2)
}

fn upload_pack_selection_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        option.len() >= "--upload".len() && "--upload-pack".starts_with(option)
    })
}

fn grep_dispatch_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        argument == "-O" || argument.starts_with("-O") && argument.len() > 2 || option.len() >= "--open".len() && "--open-files-in-pager".starts_with(option)
    })
}

fn config_dispatch_is_opaque(arguments: &[String]) -> bool {
    arguments != ["--global", "core.autocrlf", "false"]
}

fn is_builtin_subcommand(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "add"
            | "bisect"
            | "branch"
            | "cat-file"
            | "check-ignore"
            | "checkout"
            | "clone"
            | "commit"
            | "diff"
            | "difftool"
            | "fetch"
            | "filter-branch"
            | "for-each-ref"
            | "grep"
            | "init"
            | "log"
            | "ls-files"
            | "merge-base"
            | "mergetool"
            | "restore"
            | "rev-list"
            | "rev-parse"
            | "show"
            | "status"
            | "switch"
            | "tag"
            | "worktree"
    )
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
        assert!(dispatch_is_opaque(&arguments(&["config", "--global", "alias.lint", "!sh quality/lint.txt"])));
        assert!(dispatch_is_opaque(&arguments(&["config", "--local", "core.hooksPath", "quality/hooks"])));
        assert!(dispatch_is_opaque(&arguments(&["lint"])));
        assert!(!dispatch_is_opaque(&arguments(&["bisect", "start", "--", "run"])));
        assert!(!dispatch_is_opaque(&arguments(&["diff", "--", "difftool"])));
        assert!(!dispatch_is_opaque(&arguments(&["config", "--global", "core.autocrlf", "false"])));
        assert!(!dispatch_is_opaque(&arguments(&["status", "--short"])));
    }

    #[test]
    fn alternate_git_exec_paths_fail_closed() {
        assert!(dispatch_is_opaque(&arguments(&["--exec-path", "/tmp", "lint"])));
        assert!(dispatch_is_opaque(&arguments(&["--exec-path=/tmp", "lint"])));
    }

    #[test]
    fn pager_dispatch_fails_closed() {
        assert!(dispatch_is_opaque(&arguments(&["grep", "--open-files-in-pager=sh quality/lint.txt", "lint"])));
        assert!(dispatch_is_opaque(&arguments(&["grep", "--open-files", "lint"])));
        assert!(dispatch_is_opaque(&arguments(&["grep", "-Osh quality/lint.txt", "lint"])));
        assert!(dispatch_is_opaque(&arguments(&["--paginate", "status"])));
        assert!(dispatch_is_opaque(&arguments(&["-p", "log"])));
        assert!(!dispatch_is_opaque(&arguments(&["--no-pager", "grep", "lint"])));
    }

    #[test]
    fn transport_program_overrides_fail_closed() {
        assert!(dispatch_is_opaque(&arguments(
            &["clone", "--no-local", "--upload-pack=sh quality/lint.txt", ".", "target",]
        )));
        assert!(dispatch_is_opaque(&arguments(&["clone", "-ush quality/lint.txt", ".", "target"])));
        assert!(dispatch_is_opaque(&arguments(&["fetch", "--upload-pack", "quality/lint", "origin"])));
        assert!(!dispatch_is_opaque(&arguments(&["clone", "--no-local", ".", "target"])));
    }
}
