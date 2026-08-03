const REVIEWED_WRAPPER_DEFINITION: &[&str] = &[
    "git_at() {",
    "    local root=$1",
    "    shift",
    "    \"$git_command\" --no-pager --no-replace-objects -c core.autocrlf=false -c core.fsmonitor=false -c core.hooksPath=/dev/null -c core.attributesFile=/dev/null -c diff.external= -C \"$root\" \"$@\"",
    "}",
    "",
    "git_checked() {",
    "    git_at \"$repository_root\" \"$@\"",
    "}",
];

pub(super) fn dispatch_is_opaque(path: &str, arguments: &[String]) -> bool {
    if path == "script/tests/test_maintainability_bootstrap.sh" && fixture::is_reviewed_call(arguments) {
        return false;
    }
    let Some((_, subcommand)) = git_subcommand(arguments) else {
        return true;
    };
    let dynamic_arguments = arguments.iter().any(|argument| super::path::contains_dynamic_value(argument));
    configuration_is_opaque(arguments)
        || command_producing_subcommand(arguments)
        || dynamic_arguments && subcommand != "grep"
        || !matches!(
            subcommand,
            "cat-file" | "check-ignore" | "grep" | "hash-object" | "ls-files" | "ls-tree" | "merge-base" | "rev-list" | "rev-parse"
        )
}

pub(super) fn reviewed_shell_wrappers(path: &str, source: &str) -> bool {
    if path != "script/check-maintainability-bootstrap.sh" {
        return false;
    }
    let lines = source.lines().collect::<Vec<_>>();
    super::super::dynamic::has_reviewed_trusted_system_command(source)
        && lines
            .windows(REVIEWED_WRAPPER_DEFINITION.len())
            .filter(|window| *window == REVIEWED_WRAPPER_DEFINITION)
            .count()
            == 1
        && super::super::tokens::declared_shell_function_count(source, "git_at") == 1
        && super::super::tokens::declared_shell_function_count(source, "git_checked") == 1
        && lines.iter().filter(|line| **line == REVIEWED_WRAPPER_DEFINITION[3]).count() == 1
        && lines.iter().filter(|line| **line == REVIEWED_WRAPPER_DEFINITION[7]).count() == 1
}

pub(super) fn wrapper_body_is_exact(command: &str, arguments: &[String]) -> bool {
    command == "$git_command"
        && arguments
            == [
                "--no-pager",
                "--no-replace-objects",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.attributesFile=/dev/null",
                "-c",
                "diff.external=",
                "-C",
                "$root",
                "$@",
            ]
        || command == "git_at" && arguments == ["$repository_root", "$@"]
}

pub(super) fn wrapper_call_is_opaque(arguments: &[String], consumes_root: bool) -> bool {
    if consumes_root {
        let Some((root, arguments)) = arguments.split_first() else {
            return true;
        };
        return !reviewed_git_at_call(root, arguments);
    }
    !reviewed_git_checked_call(arguments)
}

fn reviewed_git_checked_call(arguments: &[String]) -> bool {
    matches!(
        arguments,
        [command, verify, revision]
            if command == "rev-parse"
                && verify == "--verify"
                && matches!(revision.as_str(), "${configured_base}^{commit}" | "HEAD^{commit}")
    ) || arguments == ["merge-base", "--is-ancestor", "$trusted_base", "$checked_head"]
        || arguments == ["hash-object", "--no-filters", "--", "$repository_root/$relative_path"]
        || arguments == ["ls-tree", "-r", "-z", "--full-tree", "$checked_head"]
        || arguments == ["ls-files", "-z", "--stage"]
        || arguments == ["clone", "--no-hardlinks", "--no-checkout", "--quiet", "--", "$repository_root", "$snapshot_root"]
}

fn reviewed_git_at_call(root: &str, arguments: &[String]) -> bool {
    match root {
        "$implementation_root" => arguments == ["rev-parse", "--verify", "HEAD^{commit}"],
        "$checker_git_root" => {
            arguments == ["hash-object", "--no-filters", "--", "$checker_root/$relative_path"]
                || arguments == ["ls-tree", "-r", "-z", "--full-tree", "$checker_revision", "--", "tools/maintainability/src"]
        }
        "$repository_root" => arguments == ["show", "$revision:$relative_path"],
        "$snapshot_root" => {
            arguments == ["update-ref", "--no-deref", "HEAD", "$checked_head"]
                || arguments == ["read-tree", "$checked_head"]
                || arguments == ["archive", "--format=tar", "$checked_head"]
                || arguments
                    == [
                        "archive",
                        "--format=tar",
                        "$trusted_checker_revision",
                        "--",
                        "tools/maintainability/Cargo.toml",
                        "tools/maintainability/Cargo.lock",
                        "tools/maintainability/src",
                    ]
        }
        _ => false,
    }
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
        let consumes_configuration = matches!(argument.as_str(), "-c" | "--config");
        let configuration = if consumes_configuration {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else if let Some(configuration) = argument.strip_prefix("--config=") {
            Some(configuration)
        } else {
            argument.strip_prefix("-c").filter(|configuration| !configuration.is_empty())
        };
        if configuration.is_some_and(|configuration| !is_safe_configuration(configuration)) {
            return true;
        }
        if consumes_configuration && configuration.is_none() {
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
        "archive" => archive_dispatch_is_opaque(&arguments[index + 1..]),
        "config" => config_dispatch_is_opaque(&arguments[index + 1..]),
        "grep" => grep_dispatch_is_opaque(&arguments[index + 1..]),
        "clone" => clone_dispatch_is_opaque(&arguments[index + 1..]),
        "fetch" => upload_pack_selection_is_opaque(&arguments[index + 1..]),
        "hash-object" => !arguments[index + 1..].iter().any(|argument| argument == "--no-filters"),
        "init" => init_dispatch_is_opaque(&arguments[index + 1..]),
        "read-tree" => read_tree_dispatch_is_opaque(&arguments[index + 1..]),
        "checkout" | "difftool" | "filter-branch" | "mergetool" | "restore" | "switch" => true,
        "bisect" => arguments[index + 1..]
            .iter()
            .take_while(|argument| argument.as_str() != "--")
            .any(|argument| argument.eq_ignore_ascii_case("run")),
        subcommand => !is_builtin_subcommand(subcommand),
    }
}

fn archive_dispatch_is_opaque(arguments: &[String]) -> bool {
    let mut explicit_tar_format = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        if argument == "--remote" || argument.starts_with("--remote=") || argument == "--exec" || argument.starts_with("--exec=") {
            return true;
        }
        let format = if argument == "--format" {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else {
            argument.strip_prefix("--format=")
        };
        if let Some(format) = format {
            if format != "tar" {
                return true;
            }
            explicit_tar_format = true;
        } else if argument == "--format" {
            return true;
        }
        index += 1;
    }
    !explicit_tar_format
}

fn read_tree_dispatch_is_opaque(arguments: &[String]) -> bool {
    arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| argument == "--reset" || argument.starts_with("--reset=") || argument.strip_prefix('-').is_some_and(|flags| !flags.starts_with('-') && flags.contains('u')))
}

fn init_dispatch_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        argument == "-t" || argument.starts_with("-t") && argument.len() > 2 || option.len() >= "--t".len() && "--template".starts_with(option)
    })
}

fn clone_dispatch_is_opaque(arguments: &[String]) -> bool {
    upload_pack_selection_is_opaque(arguments)
        || abbreviated_clone_configuration_is_opaque(arguments)
        || arguments
            .iter()
            .take_while(|argument| argument.as_str() != "--")
            .any(|argument| argument == "-u" || argument.starts_with("-u") && argument.len() > 2)
}

fn abbreviated_clone_configuration_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        option != "--config" && option.len() >= "--co".len() && "--config".starts_with(option)
    })
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
            | "archive"
            | "bisect"
            | "branch"
            | "cat-file"
            | "check-ignore"
            | "clone"
            | "commit"
            | "diff"
            | "difftool"
            | "fetch"
            | "filter-branch"
            | "for-each-ref"
            | "grep"
            | "hash-object"
            | "init"
            | "log"
            | "ls-files"
            | "ls-tree"
            | "merge-base"
            | "mergetool"
            | "rev-list"
            | "rev-parse"
            | "read-tree"
            | "show"
            | "status"
            | "tag"
            | "update-ref"
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
        "core.hookspath" | "core.attributesfile" => value == "/dev/null",
        "diff.external" => value.is_empty(),
        "user.email" | "user.name" => true,
        _ => false,
    }
}

fn is_boolean(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1" | "false" | "no" | "off" | "0")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{dispatch_is_opaque as dispatch_for_surface, reviewed_shell_wrappers, wrapper_body_is_exact, wrapper_call_is_opaque};

    fn reviewed_bootstrap() -> String {
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../script/check-maintainability-bootstrap.sh")).expect("read reviewed maintainability bootstrap")
    }

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn dispatch_is_opaque(arguments: &[String]) -> bool {
        dispatch_for_surface("script/check.sh", arguments)
    }

    #[test]
    fn reviewed_shell_wrapper_profile_is_exact_and_unique() {
        let reviewed_bootstrap = reviewed_bootstrap();
        assert!(reviewed_shell_wrappers("script/check-maintainability-bootstrap.sh", &reviewed_bootstrap));
        assert!(!reviewed_shell_wrappers("script/unreviewed.sh", &reviewed_bootstrap));
        assert!(!reviewed_shell_wrappers(
            "script/check-maintainability-bootstrap.sh",
            &reviewed_bootstrap.replace("diff.external=", "alias.lint=!sh quality/lint.txt")
        ));
        assert!(!reviewed_shell_wrappers(
            "script/check-maintainability-bootstrap.sh",
            &format!("{reviewed_bootstrap}\ngit_at() {{\n  printf bypass\n}}")
        ));
        assert!(!reviewed_shell_wrappers(
            "script/check-maintainability-bootstrap.sh",
            &format!("{reviewed_bootstrap}\nfunction git_at\n\n# alternate declaration\n{{\n  printf bypass\n}}")
        ));
        assert!(wrapper_body_is_exact(
            "$git_command",
            &arguments(&[
                "--no-pager",
                "--no-replace-objects",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.attributesFile=/dev/null",
                "-c",
                "diff.external=",
                "-C",
                "$root",
                "$@",
            ])
        ));
    }

    #[test]
    fn reviewed_shell_wrapper_calls_constrain_roots_and_git_dispatch() {
        assert!(!wrapper_call_is_opaque(&arguments(&["$snapshot_root", "read-tree", "$checked_head"]), true));
        assert!(!wrapper_call_is_opaque(
            &arguments(&["clone", "--no-hardlinks", "--no-checkout", "--quiet", "--", "$repository_root", "$snapshot_root",]),
            false
        ));
        assert!(wrapper_call_is_opaque(&arguments(&["/tmp/unreviewed", "rev-parse", "HEAD"]), true));
        assert!(wrapper_call_is_opaque(
            &arguments(&["$snapshot_root", "-c", "alias.lint=!sh quality/lint.txt", "lint"]),
            true
        ));
        assert!(wrapper_call_is_opaque(&arguments(&["clone", "$repository_root", "$snapshot_root"]), false));
    }

    #[test]
    fn reviewed_internal_git_operations_reject_execution_hooks() {
        assert!(dispatch_is_opaque(&arguments(&["archive", "--format=tar", "HEAD"])));
        assert!(dispatch_is_opaque(&arguments(&["archive", "--format=custom", "HEAD"])));
        assert!(dispatch_is_opaque(&arguments(&["archive", "--remote=origin", "--format=tar", "HEAD"])));
        assert!(!dispatch_is_opaque(&arguments(&["hash-object", "--no-filters", "--", "src/lib.rs"])));
        assert!(dispatch_is_opaque(&arguments(&["hash-object", "--", "src/lib.rs"])));
        assert!(dispatch_is_opaque(&arguments(&["read-tree", "HEAD"])));
        assert!(dispatch_is_opaque(&arguments(&["read-tree", "-u", "HEAD"])));
        assert!(dispatch_is_opaque(&arguments(&["update-ref", "HEAD", "0123456789abcdef"])));
        assert!(!dispatch_is_opaque(&arguments(&["ls-tree", "-r", "HEAD"])));
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
        assert!(dispatch_is_opaque(&arguments(&["clone", "--config"])));
        assert!(dispatch_is_opaque(&arguments(&["-c", "core.autocrlf=false", "status"])));
        assert!(dispatch_is_opaque(&arguments(&["-ccore.fsmonitor=false", "status"])));
        assert!(dispatch_is_opaque(&arguments(&["-c", "core.hooksPath=/dev/null", "status"])));
        assert!(dispatch_is_opaque(&arguments(&["-c", "user.name=LocalHold", "status"])));
        assert!(dispatch_is_opaque(&arguments(&["clone", "--config=core.autocrlf=false", ".", "target"])));
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
        assert!(dispatch_is_opaque(&arguments(&["checkout", "HEAD^", "--", "Justfile"])));
        assert!(dispatch_is_opaque(&arguments(&["restore", "--source", "HEAD^", "--", "Justfile"])));
        assert!(dispatch_is_opaque(&arguments(&["switch", "--detach", "HEAD^"])));
        assert!(dispatch_is_opaque(&arguments(&["lint"])));
        assert!(dispatch_is_opaque(&arguments(&["bisect", "start", "--", "run"])));
        assert!(dispatch_is_opaque(&arguments(&["diff", "--", "difftool"])));
        assert!(dispatch_is_opaque(&arguments(&["config", "--global", "core.autocrlf", "false"])));
        assert!(dispatch_is_opaque(&arguments(&["status", "--short"])));
    }

    #[test]
    fn git_grep_allows_dynamic_data_but_not_pager_dispatch() {
        assert!(!dispatch_is_opaque(&arguments(&["grep", "-E", "$pattern", "--", "${pathspecs[@]}"])));
        assert!(dispatch_is_opaque(&arguments(&["grep", "-O$pager", "$pattern", "--", "."])));
        assert!(dispatch_is_opaque(&arguments(&["grep", "--open-files-in-pager=$pager", "$pattern", "--", "."])));
        assert!(dispatch_is_opaque(&arguments(&["-c", "$config", "grep", "$pattern", "--", "."])));
    }

    #[test]
    fn alternate_git_exec_paths_fail_closed() {
        assert!(dispatch_is_opaque(&arguments(&["--exec-path", "/tmp", "lint"])));
        assert!(dispatch_is_opaque(&arguments(&["--exec-path=/tmp", "lint"])));
    }

    #[test]
    fn repository_templates_fail_closed() {
        for values in [
            &["init", "--template", "quality/template", "target"][..],
            &["init", "--template=quality/template", "target"],
            &["init", "--t=quality/template", "target"],
            &["init", "--templ=quality/template", "target"],
            &["init", "-t", "quality/template", "target"],
            &["init", "-tquality/template", "target"],
        ] {
            assert!(dispatch_is_opaque(&arguments(values)), "{values:?}");
        }
        assert!(dispatch_is_opaque(&arguments(&["init", "--bare", "target"])));
    }

    #[test]
    fn pager_dispatch_fails_closed_but_explicit_no_pager_is_safe() {
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
        assert!(dispatch_is_opaque(&arguments(&["clone", "--no-local", ".", "target"])));
    }

    #[test]
    fn clone_time_filter_configuration_fails_closed() {
        assert!(dispatch_is_opaque(&arguments(&["clone", "-c", "filter.lint.smudge=sh quality/lint.txt", ".", "target",])));
        assert!(dispatch_is_opaque(&arguments(&[
            "clone",
            "--config",
            "filter.lint.smudge=sh quality/lint.txt",
            ".",
            "target",
        ])));
        assert!(dispatch_is_opaque(&arguments(
            &["clone", "--config=filter.lint.smudge=sh quality/lint.txt", ".", "target",]
        )));
        assert!(dispatch_is_opaque(&arguments(&["clone", "--co", "filter.lint.smudge=sh quality/lint.txt", ".", "target",])));
        assert!(dispatch_is_opaque(&arguments(&["clone", "--conf=filter.lint.smudge=sh quality/lint.txt", ".", "target",])));
    }
}
mod fixture;
