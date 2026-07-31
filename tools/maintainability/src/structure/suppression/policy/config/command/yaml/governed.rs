use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};

use super::{is_block_scalar, leading_spaces, literal_scalar, yaml_key_value};

const WORKFLOW_PATH: &str = ".github/workflows/ci.yml";
const GATE_NAME: &str = "Run dependency unsafe gate";
const GATE_ID: &str = "audit";
const TOOLCHAIN_NAME: &str = "Install reviewed Rust toolchain";
const UPLOAD_NAME: &str = "Upload dependency audit evidence on failure";
const UPLOAD_CONDITION: &str = "failure() && steps.audit.outcome == 'failure'";
const TOOLCHAIN_RUN_SOURCE: &str = "rustup toolchain install 1.97.0 --profile minimal --component clippy --component rustfmt";
const LINUX_SHELL: &str = "/usr/bin/env -u BASH_ENV -u ENV -u LD_AUDIT -u LD_LIBRARY_PATH -u LD_PRELOAD /usr/bin/bash --noprofile --norc -e -o pipefail {0}";
const WINDOWS_SHELL: &str = r#"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$ErrorActionPreference = 'Stop'; $env:BASH_ENV = $null; $env:ENV = $null; $env:LD_AUDIT = $null; $env:LD_LIBRARY_PATH = $null; $env:LD_PRELOAD = $null; & 'C:\Program Files\Git\bin\bash.exe' --noprofile --norc -e -o pipefail '{0}'; exit $LASTEXITCODE""#;
const GATE_RUN_SOURCE: &str = r#"if [[ "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256" != "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256" ]]; then
  printf 'maintainability bootstrap differs from the workflow-reviewed digest\n' >&2
  exit 1
fi
./script/check-maintainability-bootstrap.sh --maintainability"#;
const GOVERNED_JOBS: [(&str, &str); 2] = [("dependency-unsafe-linux", "ubuntu-latest"), ("dependency-unsafe-windows", "windows-latest")];
const CHECKOUT_ACTION: &str = "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0";
const UPLOAD_ACTION: &str = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a";
const GOVERNED_STEP_COUNT: usize = 4;
const TOOLCHAIN_ENVIRONMENT: [(&str, &str); 3] = [
    ("RUSTUP_DIST_SERVER", "https://static.rust-lang.org"),
    ("RUSTUP_HOME", "${{ runner.temp }}/localhold-rustup"),
    ("RUSTUP_UPDATE_ROOT", "https://static.rust-lang.org/rustup"),
];
const GATE_ENVIRONMENT: [(&str, &str); 7] = [
    ("BASH_ENV", ""),
    ("LD_AUDIT", ""),
    ("LD_LIBRARY_PATH", ""),
    ("LD_PRELOAD", ""),
    (
        "LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256",
        "${{ hashFiles('script/check-maintainability-bootstrap.sh') }}",
    ),
    (
        "LOCALHOLD_MAINTAINABILITY_BASE_REV",
        "${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}",
    ),
    ("RUSTUP_HOME", "${{ runner.temp }}/localhold-rustup"),
];
const CHECKOUT_INPUTS: [(&str, &str); 2] = [("fetch-depth", "0"), ("persist-credentials", "false")];
const WINDOWS_CHECKOUT_ENVIRONMENT: [(&str, &str); 3] = [("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "core.autocrlf"), ("GIT_CONFIG_VALUE_0", "false")];
const LINUX_UPLOAD_INPUTS: [(&str, &str); 4] = [
    ("name", "dependency-unsafe-linux-${{ github.sha }}"),
    ("path", "target/dependency-unsafe/actual-linux"),
    ("if-no-files-found", "warn"),
    ("retention-days", "7"),
];
const WINDOWS_UPLOAD_INPUTS: [(&str, &str); 4] = [
    ("name", "dependency-unsafe-windows-${{ github.sha }}"),
    ("path", "target/dependency-unsafe/actual-windows"),
    ("if-no-files-found", "warn"),
    ("retention-days", "7"),
];

pub(super) fn is_startup_isolated_shell(shell: &str) -> bool {
    matches!(shell, LINUX_SHELL | WINDOWS_SHELL)
}

pub(super) fn validate(path: &str, source: &str) -> Result<()> {
    if path != WORKFLOW_PATH {
        return Ok(());
    }
    let mut job = None;
    let mut step = None;
    let mut governed_jobs = BTreeSet::new();
    for line in source.lines() {
        let indentation = leading_spaces(line);
        let content = line.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        if indentation == 2 && !is_sequence_item(content) {
            finish_step(job.as_mut(), step.take(), &mut governed_jobs)?;
            finish_job(job.as_ref())?;
            job = yaml_key_value(line).map(|(name, _)| Job::new(name));
            continue;
        }
        let Some(active_job) = &mut job else {
            continue;
        };
        active_job.observe_shell_default(indentation, line);
        if indentation == 4
            && let Some((key, value)) = yaml_key_value(line)
        {
            if key == "if" {
                active_job.violations.insert(JobViolation::Conditional);
            } else if key == "continue-on-error" {
                active_job.violations.insert(JobViolation::ContinuesOnError);
            } else if key == "needs" {
                active_job.violations.insert(JobViolation::HasDependencies);
            } else if key == "services" {
                active_job.violations.insert(JobViolation::HasServices);
            } else if key == "runs-on" {
                active_job.runner = literal_scalar(value);
            } else if key == "steps" && value.trim().is_empty() {
                active_job.steps_indentation = Some(indentation);
                continue;
            }
        }
        let Some(steps_indentation) = active_job.steps_indentation else {
            continue;
        };
        if indentation <= steps_indentation {
            active_job.steps_indentation = None;
            finish_step(job.as_mut(), step.take(), &mut governed_jobs)?;
            continue;
        }
        if is_sequence_item(content) {
            finish_step(job.as_mut(), step.take(), &mut governed_jobs)?;
            step = Some(Step::default());
        }
        if let Some(active_step) = &mut step {
            active_step.observe(line);
        }
    }
    finish_step(job.as_mut(), step, &mut governed_jobs)?;
    finish_job(job.as_ref())?;
    let expected_jobs = GOVERNED_JOBS.iter().map(|(name, _)| *name).collect::<BTreeSet<_>>();
    if governed_jobs != expected_jobs {
        bail!("checked-in GitHub YAML {WORKFLOW_PATH:?} must contain exactly one unconditional governed dependency-unsafe gate step in each reviewed platform job");
    }
    Ok(())
}

fn finish_step(job: Option<&mut Job>, step: Option<Step>, governed_jobs: &mut BTreeSet<&'static str>) -> Result<()> {
    let Some(step) = step else {
        return Ok(());
    };
    let job = job.expect("workflow step belongs to a job");
    if governed_job(&job.name).is_some() {
        validate_governed_step(job, &step)?;
        job.completed_steps += 1;
    }
    let named = step.name.as_deref() == Some(GATE_NAME);
    if named != step.invokes_gate() {
        bail!("checked-in GitHub YAML {WORKFLOW_PATH:?} must bind each governed dependency-unsafe invocation to its reviewed step name");
    }
    if !named {
        return Ok(());
    }
    let Some((governed_job, expected_runner)) = governed_job(&job.name) else {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} places a governed dependency-unsafe gate in unreviewed job {:?}",
            job.name
        );
    };
    if job.runner.as_deref() != Some(expected_runner) {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} runs governed dependency-unsafe job {:?} on an unreviewed runner",
            job.name
        );
    }
    if job.violations.contains(&JobViolation::Conditional) || job.violations.contains(&JobViolation::ContinuesOnError) || step.condition.is_some() || step.continues_on_error {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} makes governed dependency-unsafe gate job {:?} conditional or non-failing",
            job.name
        );
    }
    if !governed_jobs.insert(governed_job) {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} contains more than one governed dependency-unsafe gate in job {:?}",
            job.name
        );
    }
    Ok(())
}

fn validate_governed_step(job: &Job, step: &Step) -> Result<()> {
    let expected_action = match job.completed_steps {
        0 | 2 => None,
        1 => Some(CHECKOUT_ACTION),
        3 => Some(UPLOAD_ACTION),
        _ => {
            bail!(
                "checked-in GitHub YAML {WORKFLOW_PATH:?} adds an unreviewed step to governed dependency-unsafe job {:?}",
                job.name
            );
        }
    };
    validate_governed_step_inputs(job, step)?;
    if job.completed_steps == 0 {
        if step.name.as_deref() != Some(TOOLCHAIN_NAME)
            || step.uses.is_some()
            || !step.run_declared
            || step.run_source.trim_end() != TOOLCHAIN_RUN_SOURCE
            || step.condition.is_some()
            || step.continues_on_error
            || step.id.is_some()
        {
            bail!(
                "checked-in GitHub YAML {WORKFLOW_PATH:?} must install only the reviewed Rust toolchain before checkout in governed job {:?}",
                job.name
            );
        }
    } else if job.completed_steps == 2 {
        if step.name.as_deref() != Some(GATE_NAME)
            || step.id.as_deref() != Some(GATE_ID)
            || step.uses.is_some()
            || !step.run_declared
            || step.run_block_style != Some(RunBlockStyle::Canonical)
            || !step.invokes_gate()
            || step.condition.is_some()
        {
            bail!(
                "checked-in GitHub YAML {WORKFLOW_PATH:?} must run the governed dependency-unsafe gate before any repository-controlled command in job {:?}",
                job.name
            );
        }
    } else if step.uses.as_deref() != expected_action
        || step.run_declared
        || step.continues_on_error
        || step.id.is_some()
        || if job.completed_steps == 3 {
            step.name.as_deref() != Some(UPLOAD_NAME) || step.condition.as_deref() != Some(UPLOAD_CONDITION)
        } else {
            step.name.is_some() || step.condition.is_some()
        }
    {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} changes the reviewed isolated step sequence in governed dependency-unsafe job {:?}",
            job.name
        );
    }
    Ok(())
}

fn validate_governed_step_inputs(job: &Job, step: &Step) -> Result<()> {
    let expected_environment = match job.completed_steps {
        0 => TOOLCHAIN_ENVIRONMENT.as_slice(),
        1 if job.name == "dependency-unsafe-windows" => WINDOWS_CHECKOUT_ENVIRONMENT.as_slice(),
        2 => GATE_ENVIRONMENT.as_slice(),
        _ => &[],
    };
    let expected_inputs = match job.completed_steps {
        1 => CHECKOUT_INPUTS.as_slice(),
        3 if job.name == "dependency-unsafe-windows" => WINDOWS_UPLOAD_INPUTS.as_slice(),
        3 => LINUX_UPLOAD_INPUTS.as_slice(),
        _ => &[],
    };
    if !step.has_exact_environment(expected_environment) {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} must preserve the exact reviewed environment in governed job {:?}",
            job.name
        );
    }
    if !step.has_exact_inputs(expected_inputs) {
        bail!("checked-in GitHub YAML {WORKFLOW_PATH:?} changes reviewed action inputs in governed job {:?}", job.name);
    }
    Ok(())
}

fn finish_job(job: Option<&Job>) -> Result<()> {
    let Some(job) = job.filter(|job| governed_job(&job.name).is_some()) else {
        return Ok(());
    };
    let expected_shell = if job.name == "dependency-unsafe-windows" { WINDOWS_SHELL } else { LINUX_SHELL };
    if job.shell_default.as_deref() != Some(expected_shell)
        || job.violations.contains(&JobViolation::HasDependencies)
        || job.violations.contains(&JobViolation::HasServices)
        || job.violations.contains(&JobViolation::UnexpectedShell)
        || job.completed_steps != GOVERNED_STEP_COUNT
    {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} must keep the reviewed dependency-free startup-isolated step sequence in governed dependency-unsafe job {:?}",
            job.name
        );
    }
    Ok(())
}

fn governed_job(name: &str) -> Option<(&'static str, &'static str)> {
    GOVERNED_JOBS.iter().copied().find(|(job, _)| *job == name)
}

fn is_sequence_item(content: &str) -> bool {
    content == "-" || content.starts_with("- ")
}

struct Job {
    name: String,
    completed_steps: usize,
    runner: Option<String>,
    steps_indentation: Option<usize>,
    defaults_indentation: Option<usize>,
    run_defaults_indentation: Option<usize>,
    shell_default: Option<String>,
    violations: BTreeSet<JobViolation>,
}

impl Job {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            completed_steps: 0,
            runner: None,
            steps_indentation: None,
            defaults_indentation: None,
            run_defaults_indentation: None,
            shell_default: None,
            violations: BTreeSet::new(),
        }
    }

    fn observe_shell_default(&mut self, indentation: usize, line: &str) {
        if self.run_defaults_indentation.is_some_and(|parent| indentation <= parent) {
            self.run_defaults_indentation = None;
        }
        if self.defaults_indentation.is_some_and(|parent| indentation <= parent) {
            self.defaults_indentation = None;
            self.run_defaults_indentation = None;
        }
        let Some((key, value)) = yaml_key_value(line) else {
            return;
        };
        if indentation == 4 && key == "defaults" && value.trim().is_empty() {
            self.defaults_indentation = Some(indentation);
        } else if self.defaults_indentation.is_some() && indentation == 6 && key == "run" && value.trim().is_empty() {
            self.run_defaults_indentation = Some(indentation);
        } else if self.run_defaults_indentation.is_some() && indentation == 8 && key == "shell" {
            self.shell_default = literal_scalar(value);
        } else if key == "shell" {
            self.violations.insert(JobViolation::UnexpectedShell);
        }
    }
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum JobViolation {
    Conditional,
    ContinuesOnError,
    HasDependencies,
    HasServices,
    UnexpectedShell,
}

#[derive(Default)]
struct Step {
    name: Option<String>,
    condition: Option<String>,
    continues_on_error: bool,
    environment: Environment,
    environment_block_indentation: Option<usize>,
    id: Option<String>,
    inputs: Environment,
    inputs_block_indentation: Option<usize>,
    run_block_indentation: Option<usize>,
    run_block_style: Option<RunBlockStyle>,
    run_declared: bool,
    run_source: String,
    uses: Option<String>,
}

impl Step {
    fn observe(&mut self, line: &str) {
        let indentation = leading_spaces(line);
        if let Some(inputs_indentation) = self.inputs_block_indentation {
            if line.trim().is_empty() {
                return;
            }
            if indentation > inputs_indentation {
                let entry_indentation = inputs_indentation.saturating_add(2);
                self.inputs.observe(line, indentation == entry_indentation);
                return;
            }
            self.inputs_block_indentation = None;
        }
        if let Some(environment_indentation) = self.environment_block_indentation {
            if line.trim().is_empty() {
                return;
            }
            if indentation > environment_indentation {
                let entry_indentation = environment_indentation.saturating_add(2);
                self.environment.observe(line, indentation == entry_indentation);
                return;
            }
            self.environment_block_indentation = None;
        }
        if let Some(run_indentation) = self.run_block_indentation {
            if line.trim().is_empty() {
                self.run_source.push('\n');
                return;
            }
            if indentation > run_indentation {
                let content_indentation = run_indentation.saturating_add(2);
                self.run_source.push_str(line.get(content_indentation..).unwrap_or_default());
                self.run_source.push('\n');
                return;
            }
            self.run_block_indentation = None;
        }
        let Some((key, value)) = yaml_key_value(line) else {
            return;
        };
        match key {
            "name" => self.name = literal_scalar(value).or_else(|| Some(value.trim().to_owned())),
            "if" => self.condition = literal_scalar(value).or_else(|| Some(value.trim().to_owned())),
            "continue-on-error" => self.continues_on_error = true,
            "id" => self.id = literal_scalar(value),
            "env" if value.trim().is_empty() => {
                self.environment_block_indentation = Some(indentation);
            }
            "with" if value.trim().is_empty() => {
                self.inputs_block_indentation = Some(indentation);
            }
            "run" if is_block_scalar(value.trim_start()) => {
                self.run_declared = true;
                self.run_block_style = Some(if value.trim() == "|" { RunBlockStyle::Canonical } else { RunBlockStyle::Other });
                self.run_block_indentation = Some(indentation);
            }
            "run" => {
                self.run_declared = true;
                if let Some(command) = literal_scalar(value) {
                    self.run_source.push_str(&command);
                    self.run_source.push('\n');
                }
            }
            "uses" => self.uses = literal_scalar(value),
            _ => {}
        }
    }

    fn invokes_gate(&self) -> bool {
        self.run_source.trim_end() == GATE_RUN_SOURCE
    }

    fn has_exact_environment(&self, expected: &[(&str, &str)]) -> bool {
        self.environment.is_exact(expected)
    }

    fn has_exact_inputs(&self, expected: &[(&str, &str)]) -> bool {
        self.inputs.is_exact(expected)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RunBlockStyle {
    Canonical,
    Other,
}

#[derive(Default)]
struct Environment {
    entries: BTreeMap<String, String>,
    invalid: bool,
}

impl Environment {
    fn observe(&mut self, line: &str, has_expected_indentation: bool) {
        let entry = has_expected_indentation
            .then(|| yaml_key_value(line))
            .flatten()
            .and_then(|(name, value)| literal_scalar(value).map(|value| (name, value)));
        let Some((name, value)) = entry else {
            self.invalid = true;
            return;
        };
        self.invalid |= self.entries.insert(name.to_owned(), value).is_some();
    }

    fn is_exact(&self, expected: &[(&str, &str)]) -> bool {
        !self.invalid && self.entries.len() == expected.len() && expected.iter().all(|(name, value)| self.entries.get(*name).is_some_and(|actual| actual == value))
    }
}

#[cfg(test)]
mod tests;
