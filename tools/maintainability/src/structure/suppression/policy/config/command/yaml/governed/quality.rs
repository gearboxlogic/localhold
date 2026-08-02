use anyhow::{Result, bail};

use super::{Job, JobViolation, Step};

const JOB_NAME: &str = "check";
const RUNNER: &str = "ubuntu-latest";
const STEP_NAME: &str = "Run CI gate";
const RUN_SOURCE: &str = "just check-quality";

#[derive(Default)]
pub(super) struct Tracker {
    seen: bool,
}

impl Tracker {
    pub(super) fn observe(&mut self, job: &Job, step: &Step) -> Result<()> {
        let named = step.name.as_deref() == Some(STEP_NAME);
        let invokes_gate = step.run_source.trim_end() == RUN_SOURCE;
        if named != invokes_gate {
            bail!("checked-in GitHub YAML must bind the required CI quality gate to its reviewed step name and command");
        }
        if !named {
            return Ok(());
        }
        let job_can_skip = job.violations.contains(&JobViolation::Conditional)
            || job.violations.contains(&JobViolation::ContinuesOnError)
            || job.violations.contains(&JobViolation::HasDependencies);
        let step_can_skip = step.condition.is_some() || step.continues_on_error;
        let exact_step = step.uses.is_none() && step.id.is_none() && step.has_exact_environment(&[]) && step.has_exact_inputs(&[]);
        if self.seen || job.name != JOB_NAME || job.runner.as_deref() != Some(RUNNER) || job_can_skip || step_can_skip || !exact_step {
            bail!("checked-in GitHub YAML must keep one unconditional, failing required CI quality gate in the reviewed check job");
        }
        self.seen = true;
        Ok(())
    }

    pub(super) fn finish(self) -> Result<()> {
        if !self.seen {
            bail!("checked-in GitHub YAML must keep the required CI quality gate");
        }
        Ok(())
    }
}
