use std::fs;
use std::path::Path;

use super::{ReviewState, ShellMode, ShellSurface, collect_cargo_manifest_paths, collect_execution_inputs, execution_input_candidates, git};
use crate::structure::suppression::policy::config::command::arguments::tokens;

fn inputs(command: &str) -> (Vec<String>, bool) {
    let (candidates, opaque) = collect_execution_inputs(std::iter::once(command), true, "script/check.sh", false);
    (candidates.into_iter().collect(), opaque)
}

fn manifests(command: &str) -> (Vec<String>, bool) {
    let (candidates, opaque) = collect_cargo_manifest_paths(std::iter::once(command), true);
    (candidates.into_iter().collect(), opaque)
}

#[test]
fn reviewed_bootstrap_fixture_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/tests/test_maintainability_bootstrap.sh");
}

#[test]
fn reviewed_claude_fixture_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/tests/test_claude_review.sh");
}

#[test]
fn postgres_smoke_surface_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/test-postgres-smoke.sh");
}

#[test]
fn source_safety_runner_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/run-source-safety.sh");
}

#[test]
fn maintainability_gate_runner_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/run-maintainability-gate.sh");
}

#[test]
fn installer_surface_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/install.sh");
}

#[test]
fn claude_review_wrapper_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/claude-review.sh");
}

#[test]
fn publication_hygiene_surface_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/check-publication-hygiene.sh");
}

#[test]
fn protected_bootstrap_surface_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/check-maintainability-bootstrap.sh");
}

#[test]
fn developer_bootstrap_surface_is_closed() {
    assert_reviewed_shell_surface_is_closed("script/bootstrap.sh");
}

#[test]
fn justfile_surface_is_closed() {
    assert_reviewed_shell_surface_is_closed("Justfile");
}

#[test]
fn trusted_maintainability_workflow_is_closed() {
    let path = ".github/workflows/trusted-maintainability.yml";
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = fs::read_to_string(workspace.join(path)).expect("trusted workflow");
    let reviewed_source = super::super::super::surfaces::without_reviewed_dispatch(path, &source, true);
    assert_reviewed_yaml_surface_is_closed(path, &reviewed_source);
}

#[test]
fn release_workflow_is_closed() {
    let path = ".github/workflows/release.yml";
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = fs::read_to_string(workspace.join(path)).expect("release workflow");
    assert_reviewed_yaml_surface_is_closed(path, &source);
}

#[test]
fn release_smoke_workflow_is_closed() {
    let path = ".github/workflows/release-smoke.yml";
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = fs::read_to_string(workspace.join(path)).expect("release smoke workflow");
    assert_reviewed_yaml_surface_is_closed(path, &source);
}

#[test]
fn gpu_release_workflow_is_closed() {
    let path = ".github/workflows/gpu-release-gate.yml";
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = fs::read_to_string(workspace.join(path)).expect("GPU release workflow");
    assert_reviewed_yaml_surface_is_closed(path, &source);
}

#[test]
fn ci_workflow_is_closed() {
    let path = ".github/workflows/ci.yml";
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = fs::read_to_string(workspace.join(path)).expect("CI workflow");
    assert_reviewed_yaml_surface_is_closed(path, &source);
}

fn assert_reviewed_yaml_surface_is_closed(path: &str, analyzed_source: &str) {
    let mut opaque_commands = Vec::new();
    let mut opaque_substitutions = Vec::new();
    let mut opaque_runs = Vec::new();
    let powershell_runs = super::super::super::yaml::powershell_run_commands(path, analyzed_source);
    for run in super::super::super::yaml::run_commands(path, analyzed_source) {
        let analysis_run = if powershell_runs.contains(&run) {
            super::super::powershell::normalize_execution_commands(&run)
        } else {
            run.clone()
        };
        let normalized = tokens::without_noncommand_shell_data(&analysis_run);
        let functions = tokens::declared_shell_functions(&normalized);
        let surface = ShellSurface {
            path,
            mode: ShellMode {
                direct_program_paths: true,
                make_surface: false,
            },
            functions: &functions,
            review: ReviewState {
                git_wrappers: false,
                source: true,
            },
        };
        opaque_commands.extend(opaque_source_commands(surface, &normalized));
        for substitution in tokens::command_substitution_commands(&normalized, true).0 {
            opaque_substitutions.extend(opaque_source_commands(surface, &substitution));
        }
        let opaque_assignments = super::super::dynamic::opaque_command_assignment_names(path, &analysis_run);
        let (_, opaque) = collect_execution_inputs(std::iter::once(analysis_run.as_str()), true, path, true);
        if opaque {
            opaque_runs.push((
                analysis_run.clone(),
                opaque_assignments,
                super::super::untrusted_directory_change_with_quality_dispatcher(&analysis_run, true),
                tokens::has_executable_unquoted_heredoc(&analysis_run),
                tokens::process_substitution_commands(&analysis_run).1,
                tokens::command_substitution_commands(&analysis_run, true).1,
            ));
        }
    }
    let inputs = super::execution_inputs_for_surface(path, analyzed_source, true);
    assert!(
        !inputs.unresolved,
        "opaque workflow commands: {opaque_commands:#?}; substitutions: {opaque_substitutions:#?}; opaque runs: {opaque_runs:#?}; powershell runs: {powershell_runs:#?}"
    );
}

fn opaque_source_commands(surface: ShellSurface<'_>, source: &str) -> Vec<Vec<String>> {
    tokens::source_command_tokens(source)
        .into_iter()
        .filter(|command| execution_input_candidates(surface, command).1)
        .collect()
}

#[test]
fn mise_execution_inputs_are_limited_to_executable_fields() {
    let inert = super::execution_inputs_for_surface("mise.toml", "[env]\nCARGO_HOME = '{{ env.HOME }}/cargo'\n_.path = ['{{ env.HOME }}/bin']\n", false);
    assert!(!inert.unresolved);
    assert!(inert.paths.is_empty());

    let task = super::execution_inputs_for_surface("mise.toml", "[tasks.check]\nrun = './quality/check.sh'\n", false);
    assert!(!task.unresolved, "task command must resolve");
    assert_eq!(task.paths.into_iter().collect::<Vec<_>>(), ["quality/check.sh"]);

    let environment = super::execution_inputs_for_surface("mise.toml", "[env]\n_.source = 'script/environment.sh'\n", false);
    assert!(!environment.unresolved, "environment source must resolve");
    assert_eq!(environment.paths.into_iter().collect::<Vec<_>>(), ["script/environment.sh"]);

    let dynamic = super::execution_inputs_for_surface("mise.toml", "[tasks.check]\nrun = '{{ env.RUNNER }} quality/check.sh'\n", false);
    assert!(dynamic.unresolved);
}

#[test]
fn static_background_process_cleanup_commands_are_analyzable() {
    for source in [
        "sleep 300 &\nprintf '%s\\n' \"$!\" > capture/grandchild-pid\n",
        "terminate_grandchild() {\n  if [[ -z \"$grandchild_pid\" ]] || ! kill -0 \"$grandchild_pid\"; then return; fi\n  kill -TERM \"$grandchild_pid\" || true\n}\n",
        "grandchild_pid=$(< capture/grandchild-pid)\nfor _ in {1..100}; do\n  if ! kill -0 \"$grandchild_pid\"; then break; fi\n  sleep 0.01\ndone\n",
    ] {
        let inputs = super::execution_inputs_for_surface("script/process-test.sh", source, false);
        assert!(!inputs.unresolved, "{source}");
    }
}

fn assert_reviewed_shell_surface_is_closed(surface_path: &str) {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = fs::read_to_string(workspace.join(surface_path)).expect("reviewed shell surface");
    let command_source = super::direct_command_source(surface_path, &source);
    let normalized = assert_reviewed_shell_preamble_is_closed(surface_path, &command_source);
    let functions = tokens::declared_shell_functions(&normalized);
    let surface = ShellSurface {
        path: surface_path,
        mode: ShellMode {
            direct_program_paths: true,
            make_surface: false,
        },
        functions: &functions,
        review: ReviewState {
            git_wrappers: git::reviewed_shell_wrappers(surface_path, &source, true),
            source: true,
        },
    };
    let opaque_commands = tokens::source_command_tokens(&normalized)
        .into_iter()
        .filter(|command| execution_input_candidates(surface, command).1)
        .take(20)
        .collect::<Vec<_>>();
    assert!(
        opaque_commands.is_empty(),
        "reviewed shell surface {surface_path:?} has opaque commands: {opaque_commands:#?}"
    );
    let nested_surface = ShellSurface {
        mode: ShellMode {
            make_surface: false,
            ..surface.mode
        },
        ..surface
    };
    let mut opaque_substitutions = Vec::new();
    for substitution in tokens::process_substitution_commands(&normalized)
        .0
        .into_iter()
        .chain(tokens::command_substitution_commands(&normalized, true).0)
    {
        for command in tokens::source_command_tokens(&substitution) {
            if execution_input_candidates(nested_surface, &command).1 && opaque_substitutions.len() < 20 {
                opaque_substitutions.push((substitution.clone(), command));
            }
        }
    }
    assert!(
        opaque_substitutions.is_empty(),
        "reviewed shell surface {surface_path:?} has opaque substitutions: {opaque_substitutions:#?}"
    );
    let (_, opaque) = collect_execution_inputs(std::iter::once(command_source.as_str()), true, surface_path, true);
    assert!(!opaque, "reviewed shell surface {surface_path:?} became opaque");
}

fn assert_reviewed_shell_preamble_is_closed(surface_path: &str, command_source: &str) -> String {
    let opaque_assignments = super::super::dynamic::opaque_command_assignment_names(surface_path, command_source);
    assert!(
        opaque_assignments.is_empty() && !super::super::dynamic::has_opaque_command_assignment_flow(surface_path, command_source, true),
        "reviewed shell surface {surface_path:?} has opaque command assignments: {opaque_assignments:#?}"
    );
    assert!(
        !super::super::untrusted_directory_change_with_quality_dispatcher(command_source, true),
        "reviewed shell surface {surface_path:?} dispatches after an untrusted directory change"
    );
    assert!(
        !tokens::has_executable_unquoted_heredoc(command_source),
        "reviewed shell surface {surface_path:?} has an executable unquoted heredoc"
    );
    let normalized = tokens::without_noncommand_shell_data(command_source);
    let (direct_sources, direct_sources_opaque) = super::super::direct_rust_sources_for_surface(surface_path, command_source);
    let opaque_nested_text = command_source
        .replace("\\\r\n", "")
        .replace("\\\n", "")
        .split(['\n', ';', '&', '|'])
        .flat_map(tokens::command_tokens)
        .filter(|token| {
            let mut nested_sources = std::collections::BTreeSet::new();
            token.chars().any(char::is_whitespace) && super::super::collect_direct_rust_sources(token, true, &mut nested_sources)
        })
        .collect::<Vec<_>>();
    assert!(
        opaque_nested_text.is_empty(),
        "reviewed shell surface {surface_path:?} has opaque nested compiler text: {opaque_nested_text:#?}"
    );
    assert!(
        !super::super::untrusted_directory_change_with_rust_tool(command_source, true),
        "reviewed shell surface {surface_path:?} changes directory before a Rust tool"
    );
    assert!(
        !direct_sources_opaque,
        "reviewed shell surface {surface_path:?} has opaque direct Rust source discovery: {direct_sources:#?}"
    );
    assert!(
        !tokens::process_substitution_commands(&normalized).1,
        "reviewed shell surface {surface_path:?} has an opaque process substitution"
    );
    assert!(
        !tokens::command_substitution_commands(&normalized, true).1,
        "reviewed shell surface {surface_path:?} has an opaque command substitution"
    );
    normalized
}

#[test]
fn manifest_discovery_descends_only_into_relevant_command_text() {
    assert_eq!(
        manifests(r#"bash -c "cargo clippy --manifest-path tools/checker/Cargo.toml""#),
        (vec!["tools/checker/Cargo.toml".to_owned()], false)
    );
    assert_eq!(manifests(r#"write_manifest $'[package]\nname = "checker"'"#), (Vec::new(), false));
    assert_eq!(manifests(r"bash -c $'cargo clippy --manifest-path tools/checker/Cargo.toml'"), (Vec::new(), true));
}

#[test]
fn execution_inputs_distinguish_files_from_opaque_programs() {
    assert_eq!(inputs("bash quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("/usr/bin/env -i bash -e ./quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("bash -lc 'cargo clippy'"), (Vec::new(), true));
    assert_eq!(inputs("HOME=quality bash -i quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("bash --login quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("dash -il quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("fish quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("zsh quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs(r#"bash -c "$(cat quality/lint.txt)""#), (Vec::new(), true));
    assert_eq!(inputs(r#"command eval "$(printf '\143\141\162\147\157')""#), (Vec::new(), true));
    assert_eq!(inputs("Invoke-Expression $encoded"), (Vec::new(), true));
    assert_eq!(inputs("./quality/run-lints"), (vec!["quality/run-lints".to_owned()], false));
    assert_eq!(inputs("quality/run-lints"), (vec!["quality/run-lints".to_owned()], false));
    assert_eq!(inputs("./$PROGRAM"), (Vec::new(), true));
    assert_eq!(inputs("/tmp/lint"), (Vec::new(), true));
    assert_eq!(inputs("/usr/bin/uname -s"), (Vec::new(), false));
    assert_eq!(inputs("../quality/run-lints"), (Vec::new(), true));
    assert_eq!(inputs(r"'.\quality\run-lints.cmd'"), (Vec::new(), true));
    assert_eq!(inputs("status=$(./quality/run-lints)"), (vec!["quality/run-lints".to_owned()], false));
    assert_eq!(inputs("kernel=$(/usr/bin/uname -s)"), (Vec::new(), false));
    assert_eq!(inputs("values[$key]=1"), (Vec::new(), false));
    assert_eq!(inputs("flags+=(--prerelease)"), (Vec::new(), false));
    assert_eq!(inputs("flags+=($(sh quality/lint.txt))"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("((count += 1))"), (Vec::new(), false));
    assert_eq!(inputs("values[$(sh quality/lint.txt)]=1"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("value=$(<\"$reviewed_input\")"), (Vec::new(), false));
    assert_eq!(inputs("value=$(>\"$dynamic_output\")"), (Vec::new(), true));
    assert_eq!(inputs("$(printf sh) quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("`printf sh` quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs(r#""$repository_root/script/check.sh""#), (Vec::new(), true));
    assert_eq!(inputs("bash_command=$(trusted_system_command bash)"), (Vec::new(), true));
}

#[test]
fn execution_inputs_distinguish_wrappers_and_build_tools() {
    assert_eq!(inputs("timeout 10 sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(
        inputs("timeout --signal TERM --kill-after=2s 10s sh quality/lint.txt"),
        (vec!["quality/lint.txt".to_owned()], false)
    );
    assert_eq!(inputs("timeout 3s \"$binary\""), (Vec::new(), true));
    assert_eq!(inputs("timeout --unknown 3s sh quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("nice sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("nice -n 5 sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("env -u RUSTFLAGS sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("env -i HOME=/tmp sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("env | sort"), (Vec::new(), false));
    assert_eq!(inputs("sort -Vu input.txt"), (Vec::new(), false));
    assert_eq!(inputs("sort -- --compress-program=payload"), (Vec::new(), false));
    assert_eq!(inputs("nohup sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("command -p sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("command -v cargo"), (Vec::new(), false));
    assert_eq!(inputs("exec -a lint sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("builtin eval 'cargo clippy'"), (Vec::new(), true));
    assert_eq!(inputs("alias lint='sh quality/lint.txt'"), (Vec::new(), true));
    assert_eq!(inputs("builtin alias lint='sh quality/lint.txt'"), (Vec::new(), true));
    assert_eq!(inputs("script -q -e -c 'sh quality/lint.txt' /dev/null"), (Vec::new(), true));
    assert_eq!(inputs("setpriv --no-new-privs sh quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("sudo -u root sh quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("time -o report sh quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("ionice -c 3 sh quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs(r"pattern='tokio::time::(sleep|sleep_until|interval|timeout)\('"), (Vec::new(), false));
    assert_eq!(
        inputs("make -f quality/lint.rules --file=quality/common.rules"),
        (vec!["quality/common.rules".to_owned(), "quality/lint.rules".to_owned()], false)
    );
    assert_eq!(inputs("make -f $MAKEFILE"), (Vec::new(), true));
    assert_eq!(inputs("make -C quality -f lint.rules"), (vec!["lint.rules".to_owned()], true));
    assert_eq!(inputs("make -E 'all:; sh quality/lint.txt' all"), (Vec::new(), true));
    assert_eq!(inputs("make -E'all:; sh quality/lint.txt' all"), (Vec::new(), true));
    assert_eq!(inputs("make MAKEFILES=quality/lint.rules"), (Vec::new(), true));
    assert_eq!(inputs("MAKEFILES=quality/lint.rules make"), (Vec::new(), true));
    assert_eq!(inputs("make SHELL=/bin/true check"), (Vec::new(), true));
    assert_eq!(inputs("make .SHELLFLAGS=-c check"), (Vec::new(), true));
    assert_eq!(inputs("just --justfile quality/lint.data check-quality"), (Vec::new(), true));
    assert_eq!(inputs("just -fquality/lint.data check-quality"), (Vec::new(), true));
    assert_eq!(inputs("just --working-directory quality check-quality"), (Vec::new(), true));
    assert_eq!(inputs("just check-quality"), (Vec::new(), false));
    assert_eq!(inputs("rsync -e 'sh quality/lint.txt' localhost:/missing ."), (Vec::new(), true));
    assert_eq!(inputs("runner=sh; \"$runner\" quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("command=$(cat quality/lint.txt); $command"), (Vec::new(), true));
    assert_eq!(inputs("history -s 'sh quality/lint.txt'; fc -s sh"), (Vec::new(), true));
    assert_eq!(inputs("$'\\x73\\x68' quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("printf '%s' \"$'\\x73\\x68' quality/lint.txt\""), (Vec::new(), false));
}

#[test]
fn indirect_command_execution_fails_closed() {
    for command in [
        "awk -f quality/lint.awk /etc/hosts",
        "gawk --file=quality/lint.awk /etc/hosts",
        "gawk --exec quality/lint.awk /etc/hosts",
        "gawk --fil=quality/lint.awk /etc/hosts",
        "gawk -W exec=quality/lint.awk /etc/hosts",
        "awk -f $SCRIPT /etc/hosts",
        "awk 'BEGIN { system(\"sh quality/lint.txt\") }'",
        r"find /tmp -maxdepth 0 -exec sh quality/lint.txt \;",
        "find /tmp -maxdepth 0 -delete",
        "find /tmp $action",
        "xargs -a quality/args.txt sh",
        "parallel sh :::: quality/args.txt",
        "tar --checkpoint=1 --checkpoint-action=exec='sh quality/lint.txt' -cf archive.tar .",
        "tar --checkpoint-a=exec='sh quality/lint.txt' -cf archive.tar .",
        "tar --checkpoint-action exec='sh quality/lint.txt' -cf archive.tar .",
        "sort --compress-program=quality/lint.txt input.txt",
        "sort --co quality/lint.txt input.txt",
        "zip -q -T -TT 'sh quality/lint.txt' archive.zip input.txt",
        "zip -T -TTquality/lint.txt archive.zip input.txt",
        "openssl list -provider-path quality -provider lint",
        "openssl.exe req -engine lint -new",
        "cargo run --manifest-path quality/helper/Cargo.toml",
        "cargo.exe --locked run --manifest-path quality/helper/Cargo.toml",
        "cargo +1.97.0 r --manifest-path quality/helper/Cargo.toml",
        "ld.so quality/lint",
        "ld.so.1 quality/lint",
        "/lib64/ld-linux-x86-64.so.2 quality/lint",
        "/lib/ld-musl-x86_64.so.1 quality/lint",
        "go run quality/lint.go",
        "go.exe -C quality run lint.go",
        "go -C=quality run lint.go",
        "sed -nf quality/lint.sed /etc/hosts",
        "sed --file=quality/lint.sed /etc/hosts",
        "sed -f $SCRIPT /etc/hosts",
        "sed -n -e '1e sh quality/lint.txt' /etc/hosts",
        "sed --exp='1e sh quality/lint.txt' /etc/hosts",
        "sed 's/.*/sh quality\\/lint.txt/e' /etc/hosts",
        "sed -n '1w Justfile' /etc/hosts",
        "sed -n '1W Justfile' /etc/hosts",
        "sed 's/accepted/replaced/w Justfile' /etc/hosts",
        "sed '$program' /etc/hosts",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
}

#[test]
fn more_indirect_command_execution_fails_closed() {
    for command in [
        "sqlite3 :memory: '.shell sh quality/lint.txt'",
        "dbus-run-session -- sh quality/lint.txt",
        "gio launch quality/lint.desktop",
        "m4 quality/lint.m4",
        "m4.exe quality/lint.m4",
        "dpkg --pre-invoke='sh quality/lint.txt' --unpack quality/missing.deb",
        "wget --use-askpass=/tmp/askpass https://example.invalid/archive",
        "yarn exec \"sh quality/lint.txt\"",
        "yarn.cmd exec \"sh quality/lint.txt\"",
        "protoc --plugin=protoc-gen-x=quality/lint.txt --x_out=target quality/input.proto",
        "protoc.exe --plugin=protoc-gen-x=quality/lint.txt --x_out=target quality/input.proto",
        "rake --rakefile quality/lint.txt",
        "run-parts quality/hooks",
        "gcc -wrapper sh,quality/lint.txt -c quality/input.c",
        "clang -fplugin=quality/lint.so -c quality/input.c",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
    for command in [
        "find /tmp -maxdepth 0 -print",
        "tar --checkpoint=1 -cf archive.tar .",
        "tar -cf archive.tar .",
        "tar --zstd -xf dist/archive.tar.zst -C extracted",
        "tar --zstd -xf dist/archive.tar.zst -C \"$RUNNER_TEMP/archive-extracted\"",
        "openssl version",
        "openssl dgst quality/input.txt",
        "cargo metadata --manifest-path quality/helper/Cargo.toml",
        "cargo test --manifest-path Cargo.toml",
        "cargo test --manifest-path tools/maintainability/Cargo.toml",
        "cargo run --manifest-path tools/maintainability/Cargo.toml --locked -- check",
        "cargo run --manifest-path=tools/dependency-unsafe/Cargo.toml --locked -- check",
        "ld --version",
        "sed -n -e '1p' /etc/hosts",
        "sed 's/../\\\\x&/g' /etc/hosts",
    ] {
        assert_eq!(inputs(command), (Vec::new(), false), "{command}");
    }
}

#[test]
fn cargo_script_dispatch_fails_closed() {
    for command in [
        "cargo +nightly -Zscript quality/lint.rs",
        "cargo.exe -Z script quality/lint.rs",
        "cargo +nightly -Zscript -- quality/lint.rs",
        "rustup run nightly cargo -Z=script quality/lint.rs",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
}

#[test]
fn mise_exec_dispatch_fails_closed_unless_the_nested_command_is_explicit() {
    assert_eq!(inputs("mise x -- sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("mise exec -- cargo fetch"), (Vec::new(), false));
    for command in ["mise x -c 'sh quality/lint.txt'", "mise x -C quality -- sh lint.txt", "mise --quiet x -- true"] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
}

#[test]
fn python_package_installers_fail_closed() {
    for command in [
        "pip install quality/helper",
        "pip3.12.exe install quality/helper.whl",
        "/usr/bin/pip3 install quality/helper",
        "env pip3.13 install quality/helper",
        "py -m pip install quality/helper",
        "pythonw.exe -m pip install quality/helper",
        "pypy3 -m pip install quality/helper",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
}

#[test]
fn python_documentation_launchers_fail_closed() {
    for command in [
        "pydoc quality/lint.py",
        "pydoc.exe quality/lint.py",
        "pydoc3 quality/lint.py",
        "/usr/bin/pydoc3.12 quality/lint.py",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
}

#[test]
fn go_dispatch_fails_closed() {
    for command in [
        "go version",
        "go env GOROOT",
        "go test ./quality/helper",
        "go.exe -C quality test ./helper",
        "go build -toolexec=quality/lint ./...",
        "go tool quality/lint",
        "go vet -vettool=quality/lint ./...",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
}

#[test]
fn compiler_dispatch_wrappers_fail_closed() {
    for command in [
        "CCACHE_DISABLE=1 ccache sh quality/lint.txt",
        "sccache sh quality/lint.txt",
        "distcc sh quality/lint.txt",
        "gomacc sh quality/lint.txt",
        "pump sh quality/lint.txt",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
}

#[test]
fn archive_and_language_dispatch_fails_closed() {
    for command in [
        "tar -xf payload.tar --transform='s|quality/lint.data|Justfile|'",
        "tar xf payload.tar",
        "tar --extract --file=payload.tar",
        "tar --get --file=payload.tar",
        "tar --extr --file=payload.tar",
        "tar --ge --file=payload.tar",
        "tar -xf payload.tar -C extracted --transform='s|payload|../Justfile|'",
        "tar -xf payload.tar -C extracted --to-command='sh quality/lint.txt'",
        "tar -I quality/lint.txt -xf payload.tar -C extracted",
        "go generate ./quality/helper",
        "go.exe -C quality generate ./helper",
        "go -C=quality generate ./helper",
        "cmd.exe /d /s /c \"powershell.exe -NoProfile -EncodedCommand payload\"",
        "cmd /c echo accepted",
        "command.com /c echo accepted",
        "printf 'x\\n' | split -l 1 --filter='sh quality/lint.txt'",
        "split --f 'sh quality/lint.txt' input",
        "unzip -oq target/payload.zip",
        "unzip -oqd . target/payload.zip",
        "unzip -oq -d extracted -: target/payload.zip",
        "unzip -T target/payload.zip -d extracted",
        "unzip -oq target/payload.zip -d $RUNNER_TEMP/extracted",
        "ar x quality/payload.a",
        "llvm-ar --output extracted -xv quality/payload.a",
        "gcc-ar $operation quality/payload.a",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
    for command in [
        "unzip -l target/payload.zip",
        "unzip -p target/payload.zip Justfile",
        "unzip -oq target/payload.zip -d extracted",
        "unzip -oqd extracted target/payload.zip",
        "unzip -oqdextracted target/payload.zip",
        "unzip -Ppassword -oq target/payload.zip -d extracted",
        "ar t quality/payload.a",
        "ar p quality/payload.a member",
    ] {
        assert_eq!(inputs(command), (Vec::new(), false), "{command}");
    }
}

#[test]
fn ripgrep_preprocessors_fail_closed() {
    for command in ["rg --pre='sh quality/lint.txt' pattern .", "ripgrep --pre quality/lint.txt pattern ."] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
    assert_eq!(inputs("rg pattern ."), (Vec::new(), false));
}

#[test]
fn standalone_cargo_execution_fails_closed() {
    for command in [
        "cargo test --manifest-path quality/helper/Cargo.toml",
        "cargo build --manifest-path=quality/helper/Cargo.toml",
        "cargo nextest run --manifest-path quality/helper/Cargo.toml",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
}

#[test]
fn native_plugin_loading_fails_closed() {
    for command in [
        "ar --plugin=quality/lint.so rc quality/archive.a quality/input.o",
        "gcc-ar --plugin quality/lint.so rc quality/archive.a quality/input.o",
        "ld -plugin quality/lint.so -o quality/output quality/input.o",
        "x86_64-linux-gnu-ld @quality/link.args",
        "ssh-keygen -D quality/lint.so",
    ] {
        assert_eq!(inputs(command), (Vec::new(), true), "{command}");
    }
}

#[test]
fn language_and_git_dispatch_inputs_fail_closed() {
    assert_eq!(inputs(r#"tag="$(python3 script/release.py tag)""#), (vec!["script/release.py".to_owned()], false));
    assert_eq!(inputs("python3 quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("/usr/bin/python3.12 quality/lint.py"), (vec!["quality/lint.py".to_owned()], false));
    assert_eq!(inputs("/usr/bin/python3.13t quality/lint.py"), (vec!["quality/lint.py".to_owned()], false));
    assert_eq!(inputs("python -m quality.lint"), (Vec::new(), true));
    assert_eq!(inputs("python3 -m timeit 'import os; os.system(\"sh quality/lint.txt\")'"), (Vec::new(), true));
    assert_eq!(inputs("python -m $MODULE"), (Vec::new(), true));
    assert_eq!(inputs("pwsh -File quality/lint.ps1"), (vec!["quality/lint.ps1".to_owned()], false));
    assert_eq!(inputs("perl quality/lint.pl"), (Vec::new(), true));
    assert_eq!(inputs("ruby -- quality/lint.rb"), (Vec::new(), true));
    assert_eq!(inputs("swift quality/lint.swift"), (Vec::new(), true));
    assert_eq!(inputs("tclsh quality/lint.tcl"), (Vec::new(), true));
    assert_eq!(inputs("/usr/bin/tclsh8.6 quality/lint.tcl"), (Vec::new(), true));
    assert_eq!(inputs("perl -e 'system q(cargo clippy)'"), (Vec::new(), true));
    assert_eq!(inputs("git -c alias.lint='!sh quality/lint.txt' lint"), (Vec::new(), true));
    assert_eq!(inputs("git -c core.fsmonitor='sh quality/lint.txt' status"), (Vec::new(), true));
    assert_eq!(inputs("git --exec-path=/tmp lint"), (Vec::new(), true));
    assert_eq!(inputs("git --exec-path /tmp lint"), (Vec::new(), true));
    assert_eq!(inputs("git config --global alias.lint '!sh quality/lint.txt'"), (Vec::new(), true));
    assert_eq!(inputs("git lint"), (Vec::new(), true));
    assert_eq!(inputs("git -c core.autocrlf=false status"), (Vec::new(), true));
    assert_eq!(inputs("git config --global core.autocrlf false"), (Vec::new(), true));
}

#[test]
fn compact_substitutions_and_dynamic_builtins_are_opaque() {
    assert_eq!(inputs("printf '%s' \"$(/tmp/lint)\""), (Vec::new(), true));
    assert_eq!(inputs("printf '%s' 'install pinned tools with `mise install`'"), (Vec::new(), false));
    assert_eq!(inputs("enable -f /tmp/helper.so helper"), (Vec::new(), true));
}

#[test]
fn mapfile_callbacks_fail_closed_without_rejecting_literal_array_reads() {
    assert_eq!(inputs("mapfile -C 'sh quality/lint.txt' -c 1 </etc/hosts"), (Vec::new(), true));
    assert_eq!(inputs("readarray -tC 'sh quality/lint.txt' -c 1 </etc/hosts"), (Vec::new(), true));
    assert_eq!(inputs("mapfile $OPTIONS lines </etc/hosts"), (Vec::new(), true));
    assert_eq!(inputs("mapfile -t lines </etc/hosts"), (Vec::new(), false));
}

#[test]
fn shell_dispatch_inputs_fail_closed_without_matching_inert_text() {
    assert_eq!(inputs("cat <(sh quality/lint.txt)"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("cat <(printf ok;# )\nsh quality/lint.txt\n)"), (vec!["quality/lint.txt".to_owned()], false));
    assert_eq!(inputs("coproc sh quality/lint.txt"), (Vec::new(), true));
    assert_eq!(inputs("printf '%s' '<(sh quality/lint.txt)'"), (Vec::new(), false));
    assert_eq!(inputs("printf ok # <(sh quality/lint.txt)"), (Vec::new(), false));
}
