use std::collections::BTreeSet;

use super::{ValueSemantics, argv, collect_execution_inputs_with_policy, model::ExecutionInputs, mutation, record_execution_inputs};
use crate::structure::suppression::policy::config::command::arguments::python as analyzer;

#[cfg(test)]
pub(super) fn execution_inputs(surface: &str, source: &str) -> ExecutionInputs {
    execution_inputs_with_path_policy(surface, source, None)
}

pub(super) fn execution_inputs_with_path_policy(surface: &str, source: &str, path_policy: Option<&mutation::PathPolicy>) -> ExecutionInputs {
    let references = analyzer::execution_references(surface, source);
    let mut inputs = BTreeSet::new();
    let mut bare_programs = BTreeSet::new();
    let reviewed_dynamic_surface = analyzer::is_reviewed_dynamic_surface(surface, source);
    let reviewed_process_surface = analyzer::is_reviewed_process_surface(surface, source);
    let reviewed_resolution_surface = reviewed_dynamic_surface || reviewed_process_surface;
    let relative_resolution = !references.inputs.is_empty() || !references.argv_invocations.is_empty();
    let mut unresolved = references.opaque && !reviewed_process_surface
        || analyzer::has_opaque_process_arguments(surface, source)
        || references.overrides.environment && !reviewed_resolution_surface
        || references.overrides.working_directory && relative_resolution && !reviewed_resolution_surface;
    record_execution_inputs(
        references.inputs.iter().map(String::as_str).collect(),
        ValueSemantics::Literal,
        &mut inputs,
        &mut unresolved,
    );
    for invocation in &references.argv_invocations {
        let argv_inputs = argv::execution_inputs_with_path_policy(
            surface,
            &invocation.program,
            invocation.arguments.iter().map(|argument| match argument {
                analyzer::ArgvArgument::Literal(value) => argv::Argument::Literal(value),
                analyzer::ArgvArgument::Unknown => argv::Argument::Unknown,
            }),
            path_policy,
        );
        inputs.extend(argv_inputs.paths);
        bare_programs.extend(argv_inputs.windows_bare_programs);
        if !reviewed_process_surface {
            unresolved |= argv_inputs.unresolved;
        }
    }
    unresolved |= references.ambient_mutations.working_directory && (relative_resolution || !references.shell_commands.is_empty());
    unresolved |= references.ambient_mutations.environment && references.process_invocation;
    let (shell_inputs, shell_opaque) = collect_execution_inputs_with_policy(references.shell_commands.iter().map(String::as_str), true, surface, false, path_policy);
    inputs.extend(shell_inputs);
    ExecutionInputs {
        paths: inputs,
        windows_bare_programs: bare_programs,
        unresolved: unresolved || shell_opaque,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn inputs_are_collected_without_shell_false_positives() {
        let source = "if previous is None or _review_key(review) > _review_key(previous):\n    latest[normalized] = review\n";
        let inputs = execution_inputs("script/reviews.py", source);
        assert!(inputs.paths.is_empty());
        assert!(inputs.windows_bare_programs.is_empty());
        assert!(!inputs.unresolved);

        let source = r#"
subprocess.run([sys.executable, "quality/child.py"])
subprocess.run(["quality/helper.exe"])
os.system("sh quality/check.sh")
posix.system("sh quality/posix.sh")
"#;
        let inputs = execution_inputs("script/reviews.py", source);
        assert_eq!(
            inputs.paths.into_iter().collect::<Vec<_>>(),
            ["quality/check.sh", "quality/child.py", "quality/helper.exe", "quality/posix.sh"]
        );
        assert!(inputs.windows_bare_programs.is_empty());
        assert!(!inputs.unresolved);
    }

    #[test]
    fn unresolved_or_unconfined_targets_fail_closed() {
        for source in [
            r"subprocess.run([sys.executable, script])",
            r#"subprocess.run([sys.executable, "-c", payload])"#,
            r#"subprocess.run([r".\quality\helper.exe"])"#,
            r#"subprocess.run(["C:hidden.exe"])"#,
            r#"subprocess.run(["/tmp/helper"])"#,
            r#"subprocess.run(["git", "status"], shell=True)"#,
            r#"subprocess.run(["git", "status"], env=environment)"#,
            r#"subprocess.run(["quality/helper.exe"], cwd=root)"#,
        ] {
            assert!(execution_inputs("script/reviews.py", source).unresolved, "{source}");
        }
    }

    #[test]
    fn python_argv_uses_the_complete_direct_command_policy() {
        assert_python_argv_opaque(&[
            (
                r#"awk 'BEGIN { system("sh quality/lint.txt") }'"#,
                "awk",
                &[r#"BEGIN { system("sh quality/lint.txt") }"#][..],
            ),
            (r"find . -exec sh quality/lint.txt ;", "find", &[".", "-exec", "sh", "quality/lint.txt", ";"]),
            ("git -c alias.lint=!sh-quality/lint lint", "git", &["-c", "alias.lint=!sh-quality/lint", "lint"]),
            (
                "git grep --open-files-in-pager=quality/lint lint",
                "git",
                &["grep", "--open-files-in-pager=quality/lint", "lint"],
            ),
            (
                "cargo run --manifest-path quality/helper/Cargo.toml",
                "cargo",
                &["run", "--manifest-path", "quality/helper/Cargo.toml"],
            ),
            (
                "cargo --config target.x86_64-unknown-linux-gnu.runner=quality/lint run --manifest-path tools/maintainability/Cargo.toml",
                "cargo",
                &[
                    "--config",
                    "target.x86_64-unknown-linux-gnu.runner=quality/lint",
                    "run",
                    "--manifest-path",
                    "tools/maintainability/Cargo.toml",
                ],
            ),
            (
                "cargo +nightly -Z unstable-options -C quality build",
                "cargo",
                &["+nightly", "-Z", "unstable-options", "-C", "quality", "build"],
            ),
            ("cargo build --target quality/host.json", "cargo", &["build", "--target", "quality/host.json"]),
            (
                "rustc --target=quality/host.json - -o target/output",
                "rustc",
                &["--target=quality/host.json", "-", "-o", "target/output"],
            ),
            (
                "gcc -fplugin=quality/lint.so -c quality/input.c",
                "gcc",
                &["-fplugin=quality/lint.so", "-c", "quality/input.c"],
            ),
            ("ssh-keygen -D quality/lint.so", "ssh-keygen", &["-D", "quality/lint.so"]),
            ("ln target/input.txt target/output.txt", "ln", &["target/input.txt", "target/output.txt"]),
            ("cp -s payload/report target/pivot", "cp", &["-s", "payload/report", "target/pivot"]),
            ("tar -cf Justfile payload", "tar", &["-cf", "Justfile", "payload"]),
            ("ld.so quality/lint", "ld.so", &["quality/lint"]),
            (
                "tar --to-command=quality/lint -xf payload.tar -C extracted",
                "tar",
                &["--to-command=quality/lint", "-xf", "payload.tar", "-C", "extracted"],
            ),
            ("sort --compress-program=quality/lint input", "sort", &["--compress-program=quality/lint", "input"]),
            ("rg --pre quality/lint pattern .", "rg", &["--pre", "quality/lint", "pattern", "."]),
            (
                "just --justfile quality/lint.data check-quality",
                "just",
                &["--justfile", "quality/lint.data", "check-quality"],
            ),
            ("unknown-runner --eval quality/lint", "unknown-runner", &["--eval", "quality/lint"]),
            ("env -- awk program", "env", &["--", "awk", "program"]),
            ("python -m timeit pass", "python", &["-m", "timeit", "pass"]),
            ("tools/mv quality/lint.data script/check.sh", "tools/mv", &["quality/lint.data", "script/check.sh"]),
            ("quality/rustc --extern lint=quality/lint.rlib", "quality/rustc", &["--extern", "lint=quality/lint.rlib"]),
            (r#"/usr/bin/awk 'BEGIN { system("true") }'"#, "/usr/bin/awk", &[r#"BEGIN { system("true") }"#]),
            ("AWK.EXE program", "AWK.EXE", &["program"]),
            ("GIT.EXE -c alias.lint=!quality/lint lint", "GIT.EXE", &["-c", "alias.lint=!quality/lint", "lint"]),
            ("SSH-KEYGEN.EXE -D quality/lint.dll", "SSH-KEYGEN.EXE", &["-D", "quality/lint.dll"]),
        ]);
    }

    type ArgvCase<'a> = (&'a str, &'a str, &'a [&'a str]);

    fn assert_python_argv_opaque(cases: &[ArgvCase<'_>]) {
        for &(direct, program, arguments) in cases {
            let (_, direct_opaque) = super::super::collect_execution_inputs(std::iter::once(direct), true, "script/check.py", false);
            assert!(direct_opaque, "direct policy accepted {direct}");
            let source = argv_source(program, arguments);
            assert!(execution_inputs("script/check.py", &source).unresolved, "Python policy accepted {source}");
        }
    }

    #[test]
    fn safe_python_argv_matches_the_direct_command_policy() {
        for (direct, program, arguments) in [
            ("cp quality/report.txt target/report.txt", "cp", &["quality/report.txt", "target/report.txt"][..]),
            ("rg pattern .", "rg", &["pattern", "."]),
            ("git rev-parse --verify HEAD", "git", &["rev-parse", "--verify", "HEAD"]),
            ("cargo metadata --no-deps", "cargo", &["metadata", "--no-deps"]),
            (
                "cargo metadata --target x86_64-unknown-linux-gnu",
                "cargo",
                &["metadata", "--target", "x86_64-unknown-linux-gnu"],
            ),
            ("cargo deny check --config deny.toml", "cargo", &["deny", "check", "--config", "deny.toml"]),
            ("gcc -c quality/input.c -o target/input.o", "gcc", &["-c", "quality/input.c", "-o", "target/input.o"]),
            (
                "rustc --target x86_64-unknown-linux-gnu - -o target/output",
                "rustc",
                &["--target", "x86_64-unknown-linux-gnu", "-", "-o", "target/output"],
            ),
            ("tar -cf target/archive.tar data", "tar", &["-cf", "target/archive.tar", "data"]),
            ("/usr/bin/uname -a", "/usr/bin/uname", &["-a"]),
            ("quality/helper --check", "quality/helper", &["--check"]),
            ("quality/rustc --version", "quality/rustc", &["--version"]),
        ] {
            let (direct_paths, direct_opaque) = super::super::collect_execution_inputs(std::iter::once(direct), true, "script/check.py", false);
            assert!(!direct_opaque, "direct policy rejected {direct}");
            let source = argv_source(program, arguments);
            let inputs = execution_inputs("script/check.py", &source);
            assert!(!inputs.unresolved, "Python policy rejected {source}");
            assert_eq!(inputs.paths, direct_paths, "path mismatch for {source}");
        }
        let redirection_operand = argv_source("printf", &["%s", ">Justfile"]);
        assert!(!execution_inputs("script/check.py", &redirection_operand).unresolved);
    }

    #[test]
    fn argv_program_is_not_parsed_as_shell_syntax() {
        for program in ["<", "if", "(", "-runner", "cd", "compgen", "mapfile", "readarray", "source", "source.exe", "trap"] {
            let source = argv_source(program, &["quality/lint"]);
            assert!(execution_inputs("script/check.py", &source).unresolved, "{source}");
        }
    }

    #[test]
    fn path_qualified_programs_are_recorded_before_shell_name_classification() {
        for program in ["quality/if", "quality/-runner", "quality/cd", "quality/mapfile", "quality/source", "quality/trap"] {
            let source = argv_source(program, &["--check"]);
            let inputs = execution_inputs("script/check.py", &source);
            assert!(!inputs.unresolved, "{source}");
            assert!(inputs.paths.contains(program), "{source}: {:?}", inputs.paths);
        }
    }

    #[test]
    fn cargo_controls_and_custom_targets_fail_closed_in_python_argv() {
        for source in [
            r#"subprocess.run(["cargo", "--config", 'target.x86_64-unknown-linux-gnu.runner=["sh","-c","touch Justfile"]', "run", "--manifest-path", "tools/maintainability/Cargo.toml"], check=True)"#,
            r#"subprocess.run(["cargo", "+nightly", "-Z", "unstable-options", "-C", "quality", "build"], check=True)"#,
            r#"subprocess.run(["cargo", "build", "--target", "quality/host.json"], check=True)"#,
            r#"subprocess.run(["rustc", "--target=quality/host.json", "-", "-o", "target/output"], input=source, text=True, check=True)"#,
            r#"subprocess.run(["rustdoc", "--target", "quality\\host.JSON", "-", "-o", "target/output"], input=source, text=True, check=True)"#,
        ] {
            assert!(execution_inputs("script/check.py", source).unresolved, "{source}");
        }
    }

    #[test]
    fn literal_argv_metacharacters_remain_data() {
        for (program, arguments) in [
            ("rg", &["$*?[{~", "."][..]),
            ("git", &["grep", "-e", "$*?[{~", "--", "."]),
            ("gcc", &["-c", "quality/$input.c", "-o", "target/$output.o"]),
            ("cp", &["quality/$report.txt", "target/$report.txt"]),
            ("wc", &["quality/$report.txt"]),
            ("quality/$helper", &["$*?[{~"]),
        ] {
            let source = argv_source(program, arguments);
            assert!(!execution_inputs("script/check.py", &source).unresolved, "{source}");
        }
        let inputs = execution_inputs("script/check.py", &argv_source("quality/$helper", &["--check"]));
        assert!(inputs.paths.contains("quality/$helper"), "{:?}", inputs.paths);
    }

    #[test]
    fn dynamic_python_argv_is_classified_per_argument() {
        for source in [
            r#"subprocess.run(["cp", source, destination], check=True)"#,
            r#"subprocess.run(["git", "grep", pattern], check=False)"#,
            r#"subprocess.run(["git", "show", reference], check=True)"#,
            r#"subprocess.run(["gcc", compiler_argument], check=True)"#,
            r#"subprocess.run(["ssh-keygen", provider_option], check=True)"#,
            r#"subprocess.run(["rg", pattern, "."], check=True)"#,
            r#"subprocess.run(["wc", path], check=True)"#,
            r#"subprocess.run(["quality/helper", option], check=True)"#,
        ] {
            assert!(execution_inputs("script/check.py", source).unresolved, "{source}");
        }
    }

    #[test]
    fn ambient_environment_mutation_rejects_explicit_interpreter_launches() {
        let source = r#"
os.environ.update(load_configuration())
subprocess.run([sys.executable, "script/child.py"], check=True)
"#;
        assert!(execution_inputs("script/reviews.py", source).unresolved);

        let source = r#"
keys = ["PATH"]
os.environ[keys[0]]: str = "target/bin"
subprocess.run(["git", "status"], check=True)
"#;
        assert!(execution_inputs("script/reviews.py", source).unresolved);

        let source = r#"
del (os.environ["PATH"])
subprocess.run(["git", "status"], check=True)
"#;
        assert!(execution_inputs("script/reviews.py", source).unresolved);
    }

    #[test]
    fn reviewed_test_keeps_its_confined_environment_override() {
        let surface = "script/tests/test_time_abstraction.py";
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source = fs::read_to_string(workspace.join(surface)).expect("reviewed Python test");
        let references = analyzer::execution_references(surface, &source);
        assert!(
            references
                .argv_invocations
                .iter()
                .any(|invocation| invocation.program == "script/check-time-abstraction.sh"),
            "{:?}",
            references.argv_invocations
        );
        let inputs = execution_inputs(surface, &source);
        assert!(inputs.paths.contains("script/check-time-abstraction.sh"), "{:?}", inputs.paths);
        assert!(inputs.windows_bare_programs.is_empty());
        assert!(!inputs.unresolved);

        assert!(execution_inputs(surface, &(source + "\n# changed\n")).unresolved);
    }

    #[test]
    fn only_exact_python_process_profiles_can_override_argv_policy() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for surface in ["script/database_fixtures.py", "script/package_release.py"] {
            let source = fs::read_to_string(workspace.join(surface)).expect("reviewed Python process source");
            assert!(analyzer::is_reviewed_process_surface(surface, &source), "{surface}");
            assert!(!execution_inputs(surface, &source).unresolved, "{surface}");
            assert!(execution_inputs(surface, &(source + "\n# changed\n")).unresolved, "{surface}");
        }
    }

    fn argv_source(program: &str, arguments: &[&str]) -> String {
        let argv = std::iter::once(program).chain(arguments.iter().copied()).collect::<Vec<_>>();
        format!("import subprocess\nsubprocess.run({argv:?}, check=True)\n")
    }
}
