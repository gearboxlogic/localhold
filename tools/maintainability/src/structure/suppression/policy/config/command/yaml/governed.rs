use std::collections::BTreeSet;

use anyhow::{Result, bail};

use super::{is_block_scalar, leading_spaces, literal_scalar, yaml_key_value};

const WORKFLOW_PATH: &str = ".github/workflows/ci.yml";
const GATE_NAME: &str = "Run dependency unsafe gate";
const GATE_RUN_SOURCE: &str = r#"if [[ "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256" != "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256" ]]; then
  printf 'maintainability bootstrap differs from the workflow-reviewed digest\n' >&2
  exit 1
fi
./script/check-maintainability-bootstrap.sh --maintainability"#;
const GOVERNED_JOBS: [(&str, &str); 2] = [("dependency-unsafe-linux", "ubuntu-latest"), ("dependency-unsafe-windows", "windows-latest")];
const CHECKOUT_ACTION: &str = "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0";
const CACHE_ACTION: &str = "actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9";
const MISE_ACTION: &str = "jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d";
const UPLOAD_ACTION: &str = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a";
const GOVERNED_STEP_COUNT: usize = 5;

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
        if indentation == 2 && !content.starts_with("- ") {
            finish_step(job.as_mut(), step.take(), &mut governed_jobs)?;
            finish_job(job.as_ref())?;
            job = yaml_key_value(line).map(|(name, _)| Job::new(name));
            continue;
        }
        let Some(active_job) = &mut job else {
            continue;
        };
        if indentation == 4
            && let Some((key, value)) = yaml_key_value(line)
        {
            if key == "if" {
                active_job.conditional = true;
            } else if key == "continue-on-error" {
                active_job.continues_on_error = true;
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
        if content.starts_with("- ") {
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
    if job.conditional || job.continues_on_error || step.conditional || step.continues_on_error {
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
        0 => Some(CHECKOUT_ACTION),
        1 => Some(CACHE_ACTION),
        2 => Some(MISE_ACTION),
        3 => None,
        4 => Some(UPLOAD_ACTION),
        _ => {
            bail!(
                "checked-in GitHub YAML {WORKFLOW_PATH:?} adds an unreviewed step to governed dependency-unsafe job {:?}",
                job.name
            );
        }
    };
    if job.completed_steps == 3 {
        if step.name.as_deref() != Some(GATE_NAME) || step.uses.is_some() || !step.run_declared || !step.invokes_gate() {
            bail!(
                "checked-in GitHub YAML {WORKFLOW_PATH:?} must run the governed dependency-unsafe gate before any repository-controlled command in job {:?}",
                job.name
            );
        }
    } else if step.uses.as_deref() != expected_action || step.run_declared || step.continues_on_error || job.completed_steps < 4 && step.conditional {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} changes the reviewed isolated step sequence in governed dependency-unsafe job {:?}",
            job.name
        );
    }
    Ok(())
}

fn finish_job(job: Option<&Job>) -> Result<()> {
    let Some(job) = job.filter(|job| governed_job(&job.name).is_some()) else {
        return Ok(());
    };
    if job.completed_steps != GOVERNED_STEP_COUNT {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} must keep the reviewed isolated step sequence in governed dependency-unsafe job {:?}",
            job.name
        );
    }
    Ok(())
}

fn governed_job(name: &str) -> Option<(&'static str, &'static str)> {
    GOVERNED_JOBS.iter().copied().find(|(job, _)| *job == name)
}

struct Job {
    name: String,
    conditional: bool,
    continues_on_error: bool,
    completed_steps: usize,
    runner: Option<String>,
    steps_indentation: Option<usize>,
}

impl Job {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            conditional: false,
            continues_on_error: false,
            completed_steps: 0,
            runner: None,
            steps_indentation: None,
        }
    }
}

#[derive(Default)]
struct Step {
    name: Option<String>,
    conditional: bool,
    continues_on_error: bool,
    run_block_indentation: Option<usize>,
    run_declared: bool,
    run_source: String,
    uses: Option<String>,
}

impl Step {
    fn observe(&mut self, line: &str) {
        let indentation = leading_spaces(line);
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
            "if" => self.conditional = true,
            "continue-on-error" => self.continues_on_error = true,
            "run" if is_block_scalar(value.trim_start()) => {
                self.run_declared = true;
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
}

#[cfg(test)]
mod tests;
