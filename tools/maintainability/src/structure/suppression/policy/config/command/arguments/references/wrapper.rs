pub(in crate::structure::suppression::policy::config::command::arguments) enum Selection<'a> {
    NotWrapper,
    NoCommand,
    Nested(&'a [String]),
    Opaque,
}

pub(in crate::structure::suppression::policy::config::command::arguments) fn select<'a>(raw_command_word: &str, command: &str, arguments: &'a [String]) -> Selection<'a> {
    if !is_exact_command_word(raw_command_word, command) {
        return Selection::NotWrapper;
    }
    match command {
        "command" | "command.exe" => command_builtin(arguments),
        "env" | "env.exe" => env_command(arguments),
        "exec" | "exec.exe" => exec_builtin(arguments),
        "nice" | "nice.exe" => nice_command(arguments),
        "nohup" | "nohup.exe" => nohup_command(arguments),
        "rustup" | "rustup.exe" => rustup_command(arguments),
        "timeout" | "timeout.exe" => timeout_command(arguments),
        "builtin" | "builtin.exe" | "sudo" | "sudo.exe" | "time" | "time.exe" => Selection::Opaque,
        _ if is_unparsed_launcher(command) => Selection::Opaque,
        _ => Selection::NotWrapper,
    }
}

fn is_exact_command_word(word: &str, command: &str) -> bool {
    let word = word.trim_start_matches(['(', '{']);
    word.rsplit(['/', '\\']).next().unwrap_or(word).eq_ignore_ascii_case(command)
}

fn command_builtin(arguments: &[String]) -> Selection<'_> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--" => return nested_after(arguments, index + 1),
            "-p" => index += 1,
            "-V" | "-v" => return Selection::NoCommand,
            _ if argument.starts_with('-') => return Selection::Opaque,
            _ => return Selection::Nested(&arguments[index..]),
        }
    }
    Selection::NoCommand
}

fn env_command(arguments: &[String]) -> Selection<'_> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--" => return nested_after(arguments, index + 1),
            "-" | "-0" | "-i" | "--debug" | "--ignore-environment" | "--null" => index += 1,
            "--help" | "--version" => return Selection::NoCommand,
            "-u" | "--unset" | "-C" | "--chdir" => {
                if arguments.get(index + 1).is_none() {
                    return Selection::Opaque;
                }
                index += 2;
            }
            "-S" | "--split-string" => return Selection::Opaque,
            _ if argument.starts_with("--unset=") || argument.starts_with("--chdir=") => index += 1,
            _ if attached_short_operand(argument, 'u') || attached_short_operand(argument, 'C') => index += 1,
            _ if argument.starts_with('-') => return Selection::Opaque,
            _ if super::super::is_environment_assignment(argument) => index += 1,
            _ => return Selection::Nested(&arguments[index..]),
        }
    }
    Selection::NoCommand
}

fn exec_builtin(arguments: &[String]) -> Selection<'_> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--" => return nested_after(arguments, index + 1),
            "-a" => {
                if arguments.get(index + 1).is_none() {
                    return Selection::Opaque;
                }
                index += 2;
            }
            _ if argument
                .strip_prefix('-')
                .is_some_and(|options| !options.is_empty() && options.bytes().all(|option| matches!(option, b'c' | b'l'))) =>
            {
                index += 1;
            }
            _ if argument.starts_with('-') => return Selection::Opaque,
            _ => return Selection::Nested(&arguments[index..]),
        }
    }
    Selection::NoCommand
}

fn nice_command(arguments: &[String]) -> Selection<'_> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--" => return nested_after(arguments, index + 1),
            "--help" | "--version" => return Selection::NoCommand,
            "-n" | "--adjustment" => {
                if arguments.get(index + 1).is_none() {
                    return Selection::Opaque;
                }
                index += 2;
            }
            _ if argument.starts_with("--adjustment=") || attached_short_operand(argument, 'n') || legacy_nice_adjustment(argument) => {
                index += 1;
            }
            _ if argument.starts_with('-') => return Selection::Opaque,
            _ => return Selection::Nested(&arguments[index..]),
        }
    }
    Selection::NoCommand
}

fn nohup_command(arguments: &[String]) -> Selection<'_> {
    match arguments.first().map(String::as_str) {
        None | Some("--help" | "--version") => Selection::NoCommand,
        Some("--") => nested_after(arguments, 1),
        Some(argument) if argument.starts_with('-') => Selection::Opaque,
        Some(_) => Selection::Nested(arguments),
    }
}

fn timeout_command(arguments: &[String]) -> Selection<'_> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--help" | "--version" => return Selection::NoCommand,
            "--" => {
                index += 1;
                break;
            }
            "-k" | "--kill-after" | "-s" | "--signal" => {
                if arguments.get(index + 1).is_none() {
                    return Selection::Opaque;
                }
                index += 2;
            }
            "--foreground" | "--preserve-status" | "--verbose" | "-v" => index += 1,
            _ if argument.starts_with("--kill-after=") || argument.starts_with("--signal=") => index += 1,
            _ if argument.starts_with('-') => return Selection::Opaque,
            _ => break,
        }
    }
    if arguments.get(index).is_none() {
        return Selection::Opaque;
    }
    nested_after(arguments, index + 1)
}

fn rustup_command(arguments: &[String]) -> Selection<'_> {
    if arguments.first().is_none_or(|argument| !argument.eq_ignore_ascii_case("run")) {
        return Selection::NotWrapper;
    }
    let mut index = 1;
    while matches!(arguments.get(index).map(String::as_str), Some("--install")) {
        index += 1;
    }
    if matches!(arguments.get(index).map(String::as_str), None | Some("-h" | "--help")) {
        return Selection::NoCommand;
    }
    if arguments.get(index).is_some_and(|argument| argument.starts_with('-')) {
        return Selection::Opaque;
    }
    if !matches!(arguments.get(index).map(String::as_str), Some("1.97.0" | "nightly")) {
        return Selection::Opaque;
    }
    index += 1;
    if arguments.get(index).is_some_and(|argument| argument == "--") {
        index += 1;
    }
    nested_after(arguments, index)
}

fn nested_after(arguments: &[String], index: usize) -> Selection<'_> {
    arguments.get(index).map_or(Selection::NoCommand, |_| Selection::Nested(&arguments[index..]))
}

fn attached_short_operand(argument: &str, option: char) -> bool {
    argument.strip_prefix('-').is_some_and(|value| value.starts_with(option) && value.len() > option.len_utf8())
}

fn legacy_nice_adjustment(argument: &str) -> bool {
    argument
        .strip_prefix('-')
        .is_some_and(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_unparsed_launcher(command: &str) -> bool {
    matches!(
        command.trim_end_matches(".exe"),
        "bwrap"
            | "chrt"
            | "chroot"
            | "choom"
            | "cpulimit"
            | "daemonize"
            | "firejail"
            | "flock"
            | "gdb"
            | "ionice"
            | "lldb"
            | "ltrace"
            | "linux32"
            | "linux64"
            | "nsenter"
            | "numactl"
            | "perf"
            | "prlimit"
            | "runuser"
            | "sandbox-exec"
            | "script"
            | "setarch"
            | "setpriv"
            | "setsid"
            | "sftp"
            | "sg"
            | "scp"
            | "ssh"
            | "ssh-agent"
            | "start-stop-daemon"
            | "stdbuf"
            | "strace"
            | "su"
            | "systemd-run"
            | "taskset"
            | "unshare"
            | "valgrind"
            | "watch"
    )
}

pub(in crate::structure::suppression::policy::config::command::arguments) fn is_command_launcher(command: &str) -> bool {
    matches!(
        command.trim_end_matches(".exe"),
        "builtin" | "command" | "env" | "exec" | "nice" | "nohup" | "rustup" | "sudo" | "time" | "timeout"
    ) || is_unparsed_launcher(command)
}

#[cfg(test)]
mod tests {
    use super::{Selection, is_command_launcher, select};

    #[test]
    fn unparsed_launchers_are_opaque() {
        let arguments = vec!["/tmp/lock".to_owned(), "sh".to_owned(), "quality/lint.txt".to_owned()];

        assert!(matches!(select("flock", "flock", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/bin/flock", "flock", &arguments), Selection::Opaque));
        assert!(matches!(select("choom", "choom", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/bin/choom", "choom", &arguments), Selection::Opaque));
        assert!(matches!(select("chroot", "chroot", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/sbin/chroot", "chroot", &arguments), Selection::Opaque));
        assert!(matches!(select("setarch", "setarch", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/bin/setarch", "setarch", &arguments), Selection::Opaque));
        assert!(matches!(select("linux32", "linux32", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/bin/linux64", "linux64", &arguments), Selection::Opaque));
        assert!(matches!(select("sg", "sg", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/bin/sg", "sg", &arguments), Selection::Opaque));
        assert!(matches!(select("su", "su", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/bin/su", "su", &arguments), Selection::Opaque));
        assert!(matches!(select("runuser", "runuser", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/sbin/runuser", "runuser", &arguments), Selection::Opaque));
        assert!(matches!(select("start-stop-daemon", "start-stop-daemon", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/sbin/start-stop-daemon", "start-stop-daemon", &arguments), Selection::Opaque));
        assert!(matches!(select("ssh", "ssh", &arguments), Selection::Opaque));
        assert!(matches!(select("/usr/bin/ssh", "ssh", &arguments), Selection::Opaque));
        assert!(matches!(select("ssh-agent", "ssh-agent", &arguments), Selection::Opaque));
        assert!(matches!(select("ssh-agent.exe", "ssh-agent.exe", &arguments), Selection::Opaque));
        assert!(is_command_launcher("env"));
        assert!(is_command_launcher("env.exe"));
        assert!(is_command_launcher("ssh"));
        assert!(!is_command_launcher("git"));
    }

    #[test]
    fn rustup_run_selects_nested_commands_only_for_reviewed_toolchains() {
        let arguments = vec![
            "run".to_owned(),
            "--install".to_owned(),
            "1.97.0".to_owned(),
            "sh".to_owned(),
            "quality/lint.txt".to_owned(),
        ];
        let Selection::Nested(command) = select("rustup", "rustup", &arguments) else {
            panic!("rustup run should select its nested command");
        };
        assert_eq!(command, ["sh", "quality/lint.txt"]);
        assert!(matches!(
            select("rustup", "rustup", &["run".to_owned(), "nightly".to_owned(), "cargo-fmt".to_owned()]),
            Selection::Nested(_)
        ));
        assert!(matches!(
            select("rustup", "rustup", &["run".to_owned(), "fake".to_owned(), "cargo".to_owned(), "clippy".to_owned()]),
            Selection::Opaque
        ));
        assert!(matches!(select("rustup", "rustup", &["show".to_owned()]), Selection::NotWrapper));
        assert!(matches!(select("rustup", "rustup", &["run".to_owned(), "--help".to_owned()]), Selection::NoCommand));
    }
}
