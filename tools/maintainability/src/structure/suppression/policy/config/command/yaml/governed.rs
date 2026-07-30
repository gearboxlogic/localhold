use anyhow::{Result, bail};

use super::{leading_spaces, literal_scalar, yaml_key_value};

const WORKFLOW_PATH: &str = ".github/workflows/ci.yml";
const GATE_NAME: &str = "Run dependency unsafe gate";
const GATE_COMMAND: &str = "./script/check-maintainability-bootstrap.sh --maintainability";

pub(super) fn validate(path: &str, source: &str) -> Result<()> {
    if path != WORKFLOW_PATH {
        return Ok(());
    }
    let mut job = None;
    let mut step = None;
    let mut governed_steps = 0;
    for line in source.lines() {
        let indentation = leading_spaces(line);
        let content = line.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        if indentation == 2 && !content.starts_with("- ") {
            finish_step(job.as_ref(), step.take(), &mut governed_steps)?;
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
            finish_step(job.as_ref(), step.take(), &mut governed_steps)?;
            continue;
        }
        if content.starts_with("- ") {
            finish_step(job.as_ref(), step.take(), &mut governed_steps)?;
            step = Some(Step::default());
        }
        if let Some(active_step) = &mut step {
            active_step.observe(line);
        }
    }
    finish_step(job.as_ref(), step, &mut governed_steps)?;
    if governed_steps != 2 {
        bail!("checked-in GitHub YAML {WORKFLOW_PATH:?} must contain exactly two unconditional governed dependency-unsafe gate steps");
    }
    Ok(())
}

fn finish_step(job: Option<&Job>, step: Option<Step>, governed_steps: &mut usize) -> Result<()> {
    let Some(step) = step else {
        return Ok(());
    };
    let named = step.name.as_deref() == Some(GATE_NAME);
    if named != step.invokes_gate {
        bail!("checked-in GitHub YAML {WORKFLOW_PATH:?} must bind each governed dependency-unsafe invocation to its reviewed step name");
    }
    if !named {
        return Ok(());
    }
    let job = job.expect("workflow step belongs to a job");
    if job.conditional || step.conditional || step.continues_on_error {
        bail!(
            "checked-in GitHub YAML {WORKFLOW_PATH:?} makes governed dependency-unsafe gate job {:?} conditional or non-failing",
            job.name
        );
    }
    *governed_steps += 1;
    Ok(())
}

struct Job {
    name: String,
    conditional: bool,
    steps_indentation: Option<usize>,
}

impl Job {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            conditional: false,
            steps_indentation: None,
        }
    }
}

#[derive(Default)]
struct Step {
    name: Option<String>,
    invokes_gate: bool,
    conditional: bool,
    continues_on_error: bool,
}

impl Step {
    fn observe(&mut self, line: &str) {
        self.invokes_gate |= line.contains(GATE_COMMAND);
        let Some((key, value)) = yaml_key_value(line) else {
            return;
        };
        match key {
            "name" => self.name = literal_scalar(value).or_else(|| Some(value.trim().to_owned())),
            "if" => self.conditional = true,
            "continue-on-error" => self.continues_on_error = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validate;

    const JOB: &str = "  dependency-unsafe-linux:\n    runs-on: ubuntu-latest\n    steps:\n      - name: Run dependency unsafe gate\n        run: |\n          ./script/check-maintainability-bootstrap.sh --maintainability\n";

    #[test]
    fn governed_gate_steps_are_exact_and_unconditional() {
        let accepted = format!("name: CI\non: push\njobs:\n{JOB}{}", JOB.replace("linux", "windows"));
        validate(".github/workflows/ci.yml", &accepted).expect("two unconditional gates");

        let conditional_step = accepted.replacen("        run: |", "        if: false\n        run: |", 1);
        assert!(validate(".github/workflows/ci.yml", &conditional_step).is_err());

        let conditional_job = accepted.replacen("    runs-on:", "    if: false\n    runs-on:", 1);
        assert!(validate(".github/workflows/ci.yml", &conditional_job).is_err());

        let non_failing_step = accepted.replacen("        run: |", "        continue-on-error: true\n        run: |", 1);
        assert!(validate(".github/workflows/ci.yml", &non_failing_step).is_err());

        let renamed = accepted.replacen("Run dependency unsafe gate", "Skip dependency unsafe gate", 1);
        assert!(validate(".github/workflows/ci.yml", &renamed).is_err());
    }
}
