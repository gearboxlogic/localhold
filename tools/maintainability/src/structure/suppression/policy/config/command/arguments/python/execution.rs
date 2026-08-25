use std::ops::Range;

use self::syntax::CallScanner;

mod syntax;

#[derive(Default)]
pub(in crate::structure::suppression::policy::config::command::arguments) struct ResolutionChannels {
    pub(in crate::structure::suppression::policy::config::command::arguments) environment: bool,
    pub(in crate::structure::suppression::policy::config::command::arguments) working_directory: bool,
}

#[derive(Default)]
pub(in crate::structure::suppression::policy::config::command::arguments) struct References {
    pub(in crate::structure::suppression::policy::config::command::arguments) inputs: Vec<String>,
    pub(in crate::structure::suppression::policy::config::command::arguments) argv_invocations: Vec<ArgvInvocation>,
    pub(in crate::structure::suppression::policy::config::command::arguments) shell_commands: Vec<String>,
    pub(in crate::structure::suppression::policy::config::command::arguments) overrides: ResolutionChannels,
    pub(in crate::structure::suppression::policy::config::command::arguments) ambient_mutations: ResolutionChannels,
    pub(in crate::structure::suppression::policy::config::command::arguments) process_invocation: bool,
    pub(in crate::structure::suppression::policy::config::command::arguments) opaque: bool,
}

#[derive(Debug)]
pub(in crate::structure::suppression::policy::config::command::arguments) struct ArgvInvocation {
    pub(in crate::structure::suppression::policy::config::command::arguments) program: String,
    pub(in crate::structure::suppression::policy::config::command::arguments) arguments: Vec<ArgvArgument>,
}

#[derive(Debug)]
pub(in crate::structure::suppression::policy::config::command::arguments) enum ArgvArgument {
    Literal(String),
    Unknown,
}

pub(super) fn collect(source: &str) -> References {
    let mut references = References {
        ambient_mutations: ResolutionChannels {
            environment: super::mutates_process_environment(source),
            working_directory: super::mutates_process_working_directory(source),
        },
        ..References::default()
    };
    let mut scanner = CallScanner::new(source);
    while let Some(call) = scanner.next() {
        let Some(kind) = process_kind(&call.name) else {
            continue;
        };
        references.process_invocation = true;
        if kind == ProcessKind::Unsupported {
            references.opaque = true;
            continue;
        }
        let Some(arguments) = scanner.arguments(call.opening_parenthesis) else {
            references.opaque = true;
            continue;
        };
        references.overrides.environment |= keyword_override(&scanner, &arguments, "env");
        references.overrides.working_directory |= keyword_override(&scanner, &arguments, "cwd");
        if arguments.iter().skip(1).any(|argument| {
            let compact = scanner.compact(argument.clone());
            compact.starts_with('*')
                || !is_keyword_argument(&compact)
                || keyword_is_overridden(&compact, "executable")
                || keyword_is_overridden(&compact, "preexec_fn")
                || compact.starts_with("shell=") && compact != "shell=False"
        }) {
            references.opaque = true;
            continue;
        }
        let Some(command_argument) = arguments.first() else {
            references.opaque = true;
            continue;
        };
        if kind == ProcessKind::Shell {
            match scanner.literal(command_argument.clone()) {
                Some(command) => references.shell_commands.push(command),
                None => references.opaque = true,
            }
            continue;
        }
        let expressions = scanner.sequence(command_argument.clone()).unwrap_or_else(|| vec![command_argument.clone()]);
        let Some(command) = expressions.first() else {
            references.opaque = true;
            continue;
        };
        if scanner.compact(command.clone()) == "sys.executable" {
            collect_python_input(&scanner, &expressions[1..], &mut references);
            continue;
        }
        let Some(program) = scanner.literal(command.clone()) else {
            references.opaque = true;
            continue;
        };
        if program.is_empty() || program.chars().any(char::is_whitespace) {
            references.opaque = true;
            continue;
        }
        let arguments = expressions[1..]
            .iter()
            .map(|argument| scanner.literal(argument.clone()).map_or(ArgvArgument::Unknown, ArgvArgument::Literal))
            .collect();
        references.argv_invocations.push(ArgvInvocation { program, arguments });
    }
    references.opaque |= scanner.has_opaque_formatted_process_expression();
    references
}

fn collect_python_input(scanner: &CallScanner, arguments: &[Range<usize>], references: &mut References) {
    let mut after_options = false;
    for argument in arguments {
        let Some(value) = scanner.literal(argument.clone()) else {
            references.opaque = true;
            return;
        };
        if !after_options && matches!(value.as_str(), "-h" | "--help" | "-V" | "--version") {
            return;
        }
        if (!after_options && matches!(value.as_str(), "-c" | "-m")) || value == "-" {
            references.opaque = true;
            return;
        }
        if !after_options && value == "--" {
            after_options = true;
            continue;
        }
        if !after_options && value.starts_with('-') {
            if python_flag_without_operand(&value) {
                continue;
            }
            references.opaque = true;
            return;
        }
        if !value.rsplit_once('.').is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("py")) {
            references.opaque = true;
            return;
        }
        references.inputs.push(value);
        return;
    }
    references.opaque = true;
}

fn python_flag_without_operand(argument: &str) -> bool {
    matches!(
        argument,
        "-b" | "-bb" | "-B" | "-d" | "-E" | "-i" | "-I" | "-O" | "-OO" | "-P" | "-q" | "-R" | "-s" | "-S" | "-u" | "-v" | "-x"
    ) || argument.starts_with("-W") && argument.len() > 2
        || argument.starts_with("-X") && argument.len() > 2
        || argument.starts_with("--check-hash-based-pycs=")
}

fn is_keyword_argument(argument: &str) -> bool {
    argument.split_once('=').is_some_and(|(name, value)| {
        !value.starts_with('=')
            && !name.is_empty()
            && name
                .chars()
                .enumerate()
                .all(|(index, character)| character == '_' || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit()))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessKind {
    Argv,
    Shell,
    Unsupported,
}

pub(super) fn process_kind(name: &str) -> Option<ProcessKind> {
    let name = name.to_ascii_lowercase();
    if name
        .split('.')
        .any(|component| matches!(component, "create_subprocess_exec" | "create_subprocess_shell" | "subprocess_exec" | "subprocess_shell"))
    {
        return Some(ProcessKind::Unsupported);
    }
    if name.contains('.') && (super::rejected_python_module(&name) || super::unconditional_execution_module(&name)) {
        return Some(ProcessKind::Unsupported);
    }
    if matches!(
        name.as_str(),
        "os.system" | "os.popen" | "posix.system" | "posix.popen" | "subprocess.getoutput" | "subprocess.getstatusoutput"
    ) {
        return Some(ProcessKind::Shell);
    }
    if matches!(
        name.as_str(),
        "asyncio.create_subprocess_exec"
            | "asyncio.create_subprocess_shell"
            | "os.startfile"
            | "os.posix_spawn"
            | "os.posix_spawnp"
            | "posix.posix_spawn"
            | "posix.posix_spawnp"
            | "posix_spawn"
            | "posix_spawnp"
            | "pty.spawn"
            | "subprocess._fork_exec"
    ) || name.starts_with("os.exec")
        || name.starts_with("os.spawn")
        || name.starts_with("posix.exec")
        || name.starts_with("posix.spawn")
    {
        return Some(ProcessKind::Unsupported);
    }
    if matches!(
        name.as_str(),
        "subprocess.run" | "subprocess.call" | "subprocess.check_call" | "subprocess.check_output" | "subprocess.popen"
    ) {
        return Some(ProcessKind::Argv);
    }
    None
}

fn keyword_override(scanner: &CallScanner, arguments: &[Range<usize>], keyword: &str) -> bool {
    arguments
        .iter()
        .skip(1)
        .map(|argument| scanner.compact(argument.clone()))
        .any(|argument| keyword_is_overridden(&argument, keyword))
}

fn keyword_is_overridden(argument: &str, keyword: &str) -> bool {
    argument
        .strip_prefix(keyword)
        .and_then(|value| value.strip_prefix('='))
        .is_some_and(|value| value != "None")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{CallScanner, collect};

    #[test]
    fn local_programs_and_python_inputs_are_closed() {
        let references = collect(
            r#"
subprocess.run([sys.executable, "quality/hidden.py"], check=True)
subprocess.run(["quality/helper.exe", "--check"], check=True)
subprocess.run(
    ["script/check-time-abstraction.sh"],
    cwd=root,
    env=environment,
)
os.system("sh quality/lint.txt")
posix.system("sh quality/posix.txt")
"#,
        );
        assert_eq!(references.inputs, ["quality/hidden.py"]);
        assert_eq!(programs(&references), ["quality/helper.exe", "script/check-time-abstraction.sh"]);
        assert_eq!(arguments(&references, 0), vec![Some("--check")]);
        assert_eq!(references.shell_commands, ["sh quality/lint.txt", "sh quality/posix.txt"]);
        assert!(references.overrides.environment);
        assert!(references.overrides.working_directory);
        assert!(!references.opaque);
    }

    #[test]
    fn python_interpreter_inputs_require_a_python_source_extension() {
        for source in [
            r#"subprocess.run([sys.executable, "quality/hidden.txt"], check=True)"#,
            r#"subprocess.run([sys.executable, "quality/hidden.pyw"], check=True)"#,
            r#"subprocess.run([sys.executable, "quality/hidden"], check=True)"#,
        ] {
            assert!(collect(source).opaque, "{source}");
        }
        assert!(!collect(r#"subprocess.run([sys.executable, "quality/hidden.PY"], check=True)"#).opaque);
    }

    #[test]
    fn whitespace_qualified_process_calls_are_closed() {
        let references = collect(
            r#"
os . system("sh quality/lint.txt")
posix . popen("sh quality/posix.txt")
subprocess . run(["quality/helper.exe"], check=True)
"#,
        );
        assert_eq!(programs(&references), ["quality/helper.exe"]);
        assert_eq!(references.shell_commands, ["sh quality/lint.txt", "sh quality/posix.txt"]);
        assert!(references.process_invocation);
        assert!(!references.opaque);
    }

    #[test]
    fn dynamic_interpreter_and_windows_paths_fail_closed() {
        for source in [
            r"subprocess.run([sys.executable, script])",
            r#"subprocess.run([sys.executable, "-c", payload])"#,
            r#"subprocess.run([r".\quality\helper.exe"])"#,
            r#"subprocess.run(["git", "status"], shell=True)"#,
            r#"subprocess.run(["git", "status"], preexec_fn=callback)"#,
            r#"subprocess.run(["git", "status"], **kwargs)"#,
            r#"subprocess.run("git status")"#,
            r"os.execvp(program, arguments)",
            r"os.spawnvp(os.P_WAIT, program, arguments)",
            r#"os.startfile("quality/helper.exe")"#,
            r#"asyncio.create_subprocess_exec("quality/helper.exe")"#,
            r#"asyncio.create_subprocess_shell("sh quality/check.sh")"#,
            r#"os.posix_spawn("quality/helper.exe", ["quality/helper.exe"], os.environ)"#,
            r#"posix.posix_spawnp("quality/helper.exe", ["quality/helper.exe"], os.environ)"#,
            r#"posix_spawn("quality/helper.exe", ["quality/helper.exe"], os.environ)"#,
            r#"pty.spawn(["quality/helper.exe"])"#,
        ] {
            assert!(collect(source).opaque, "{source}");
        }
    }

    #[test]
    fn supported_calls_cannot_mask_unsupported_process_apis() {
        for source in [
            "asyncio.create_subprocess_exec('quality/hidden.py')\nsubprocess.run(['git', 'status'])\n",
            "os.posix_spawn('quality/hidden.py', ['quality/hidden.py'], os.environ)\nsubprocess.run(['git', 'status'])\n",
            "posix.posix_spawnp('quality/hidden.py', ['quality/hidden.py'], {})\nsubprocess.run(['git', 'status'])\n",
            "posix_spawn('quality/hidden.py', ['quality/hidden.py'], {})\nsubprocess.run(['git', 'status'])\n",
            "pty.spawn(['quality/hidden.py'])\nsubprocess.run(['git', 'status'])\n",
            "pydoc.pipepager('', 'sh quality/hidden.txt')\nsubprocess.run(['git', 'status'])\n",
            "webbrowser.BackgroundBrowser('sh').open('quality/hidden.txt')\nsubprocess.run(['git', 'status'])\n",
        ] {
            assert!(collect(source).opaque, "{source}");
        }
    }

    #[test]
    fn private_subprocess_dispatch_is_unsupported() {
        for source in [
            "subprocess._fork_exec(*arguments)",
            "_posixsubprocess.fork_exec(*arguments)",
            "pip._internal.cli.main.main(arguments)",
            "ensurepip._run_pip(arguments)",
        ] {
            assert!(collect(source).opaque, "{source}");
        }
    }

    #[test]
    fn rejected_module_calls_fail_closed() {
        for source in [
            r#"asyncio.get_running_loop().subprocess_shell(asyncio.SubprocessProtocol, "sh quality/hidden.txt")"#,
            r#"asyncio.get_event_loop_policy().new_event_loop().subprocess_exec(asyncio.SubprocessProtocol, "quality/hidden.py")"#,
            r#"asyncio.Runner().get_loop().subprocess_shell(asyncio.SubprocessProtocol, "sh quality/hidden.txt")"#,
            r#"loop.subprocess_exec(protocol, "quality/hidden.py")"#,
            r#"subprocess_shell(protocol, "sh quality/hidden.txt")"#,
            r#"loop.subprocess_exec.__call__(protocol, "quality/hidden.py")"#,
            "inspect.signature(callback)",
            "operator.itemgetter(0)",
            r#"pydoc.pipepager("", "sh quality/hidden.txt")"#,
            r#"pydoc.render_doc("topic")"#,
            r#"webbrowser.BackgroundBrowser("sh").open("quality/hidden.txt")"#,
            r#"webbrowser.open("https://example.com")"#,
        ] {
            assert!(collect(source).opaque, "{source}");
        }
        assert!(!collect("await asyncio.sleep(1)").opaque);
    }

    #[test]
    fn positional_popen_controls_fail_closed() {
        for source in [
            r#"subprocess.Popen(["placeholder"], -1, "quality/helper.exe")"#,
            r#"subprocess.Popen(["git"], -1, None, None, None, None, callback)"#,
            r#"subprocess.Popen(["git"], -1, None, None, None, None, None, True, True)"#,
            r#"subprocess.Popen(["git"], -1, None, None, None, None, None, True, False, root)"#,
            r#"subprocess.Popen(["git"], -1, None, None, None, None, None, True, False, None, environment)"#,
        ] {
            assert!(collect(source).opaque, "{source}");
        }
    }

    #[test]
    fn bare_programs_are_reported_for_windows_shadow_detection() {
        let references = collect(r#"subprocess.run(["GiT", "status"], check=True)"#);
        assert_eq!(programs(&references), ["GiT"]);
        assert_eq!(arguments(&references, 0), vec![Some("status")]);
        assert!(!references.opaque);
    }

    #[test]
    fn argv_arguments_preserve_literal_and_unresolved_states() {
        let references = collect("subprocess.run(['cp', 'quality/lint.data', 'Justfile'], check=True)\nsubprocess.run(['mv', source, destination], check=True)\n");
        assert_eq!(programs(&references), ["cp", "mv"]);
        assert_eq!(arguments(&references, 0), vec![Some("quality/lint.data"), Some("Justfile")]);
        assert_eq!(arguments(&references, 1), vec![None, None]);
        assert!(!references.opaque);
    }

    #[test]
    fn ambient_resolution_mutations_are_reported() {
        let references = collect(
            r#"
os.chdir("quality")
os.environ["Path"] = "quality"
subprocess.run(["git", "status"], check=True)
"#,
        );
        assert!(references.ambient_mutations.environment);
        assert!(references.ambient_mutations.working_directory);
        assert!(!references.opaque);
    }

    #[test]
    fn process_calls_in_formatted_expressions_fail_closed() {
        assert!(collect(r#"message = f"{subprocess.run(['quality/helper.exe'])}""#).opaque);
    }

    #[test]
    fn process_environment_and_working_directory_overrides_are_reported() {
        let references = collect(r#"subprocess.run(["quality/helper.exe"], env=environment, cwd=root)"#);
        assert_eq!(programs(&references), ["quality/helper.exe"]);
        assert!(references.overrides.environment);
        assert!(references.overrides.working_directory);
        assert!(!references.opaque);
    }

    #[test]
    fn scanner_finds_multiline_process_calls_in_repository_sources() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source = fs::read_to_string(workspace.join("script/tests/test_time_abstraction.py")).expect("Python process test source");
        let mut scanner = CallScanner::new(&source);
        let mut names = Vec::new();
        while let Some(call) = scanner.next() {
            names.push(call.name);
        }
        assert!(names.iter().any(|name| name == "subprocess.run"), "{names:?}");
    }

    fn programs(references: &super::References) -> Vec<&str> {
        references.argv_invocations.iter().map(|invocation| invocation.program.as_str()).collect()
    }

    fn arguments(references: &super::References, index: usize) -> Vec<Option<&str>> {
        references.argv_invocations[index]
            .arguments
            .iter()
            .map(|argument| match argument {
                super::ArgvArgument::Literal(value) => Some(value.as_str()),
                super::ArgvArgument::Unknown => None,
            })
            .collect()
    }
}
