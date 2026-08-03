use std::collections::BTreeSet;

use super::{collect_execution_inputs, model::ExecutionInputs, path, record_execution_inputs};
use crate::structure::suppression::policy::config::command::arguments::python as analyzer;

pub(super) fn execution_inputs(surface: &str, source: &str) -> ExecutionInputs {
    let references = analyzer::execution_references(surface, source);
    let mut inputs = BTreeSet::new();
    let mut bare_programs = BTreeSet::new();
    let reviewed_dynamic_surface = analyzer::is_reviewed_dynamic_surface(surface, source);
    let reviewed_process_surface = analyzer::is_reviewed_process_surface(surface, source);
    let reviewed_resolution_surface = reviewed_dynamic_surface || reviewed_process_surface;
    let relative_resolution = !references.inputs.is_empty() || !references.programs.is_empty();
    let mut unresolved = references.opaque && !reviewed_process_surface
        || analyzer::has_opaque_process_arguments(surface, source)
        || references.overrides.environment && !reviewed_resolution_surface
        || references.overrides.working_directory && relative_resolution && !reviewed_resolution_surface;
    record_execution_inputs(references.inputs.iter().map(String::as_str).collect(), &mut inputs, &mut unresolved);
    for program in &references.programs {
        match path::select_program(program, true) {
            path::ProgramPath::Literal(candidate) => record_execution_inputs(vec![candidate], &mut inputs, &mut unresolved),
            path::ProgramPath::NotPath if !program.contains(['/', '\\']) => {
                bare_programs.insert(program.to_ascii_lowercase());
            }
            path::ProgramPath::NotPath => {}
            path::ProgramPath::Opaque => unresolved = true,
        }
    }
    unresolved |= references.ambient_mutations.working_directory && (relative_resolution || !references.shell_commands.is_empty());
    unresolved |= references.ambient_mutations.environment && references.process_invocation;
    let (shell_inputs, shell_opaque) = collect_execution_inputs(references.shell_commands.iter().map(String::as_str), true, surface, source);
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
            references.programs.iter().any(|program| program == "script/check-time-abstraction.sh"),
            "{:?}",
            references.programs
        );
        let inputs = execution_inputs(surface, &source);
        assert!(inputs.paths.contains("script/check-time-abstraction.sh"), "{:?}", inputs.paths);
        assert!(inputs.windows_bare_programs.is_empty());
        assert!(!inputs.unresolved);

        assert!(execution_inputs(surface, &(source + "\n# changed\n")).unresolved);
    }
}
