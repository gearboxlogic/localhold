#[derive(Clone, Copy)]
pub(super) struct Options {
    tool_case: ToolCase,
    backticks: BacktickPolicy,
    command_substitutions: CommandSubstitutionPolicy,
    just_interpolation: JustInterpolationPolicy,
    integrity: IntegrityInspection,
    initial_errexit: bool,
    nesting_depth: u8,
}

#[derive(Clone, Copy)]
enum ToolCase {
    Sensitive,
    Insensitive,
}

#[derive(Clone, Copy)]
enum BacktickPolicy {
    Allow,
    Reject,
}

#[derive(Clone, Copy)]
enum CommandSubstitutionPolicy {
    Inspect,
    Ignore,
}

#[derive(Clone, Copy)]
enum JustInterpolationPolicy {
    Allow,
    Reject,
}

#[derive(Clone, Copy)]
enum IntegrityInspection {
    Nested,
    CommandsOnly,
    TopLevel,
}

impl Options {
    pub(super) const fn strict() -> Self {
        Self {
            tool_case: ToolCase::Sensitive,
            backticks: BacktickPolicy::Reject,
            command_substitutions: CommandSubstitutionPolicy::Inspect,
            just_interpolation: JustInterpolationPolicy::Reject,
            integrity: IntegrityInspection::TopLevel,
            initial_errexit: true,
            nesting_depth: 0,
        }
    }

    pub(super) const fn with_case_insensitive_tools(mut self) -> Self {
        self.tool_case = ToolCase::Insensitive;
        self
    }

    pub(super) const fn allow_backticks(mut self) -> Self {
        self.backticks = BacktickPolicy::Allow;
        self
    }

    pub(super) const fn ignore_command_substitutions(mut self) -> Self {
        self.command_substitutions = CommandSubstitutionPolicy::Ignore;
        self
    }

    pub(super) const fn allow_just_interpolation(mut self) -> Self {
        self.just_interpolation = JustInterpolationPolicy::Allow;
        self
    }

    pub(super) const fn ignore_function_definitions(mut self) -> Self {
        if matches!(self.integrity, IntegrityInspection::TopLevel) {
            self.integrity = IntegrityInspection::CommandsOnly;
        }
        self
    }

    pub(super) const fn without_initial_errexit(mut self) -> Self {
        self.initial_errexit = false;
        self
    }

    pub(super) const fn nested(mut self) -> Self {
        self.integrity = IntegrityInspection::Nested;
        self.nesting_depth = self.nesting_depth.saturating_add(1);
        self
    }

    pub(super) const fn can_descend(self) -> bool {
        self.nesting_depth < 32
    }

    pub(super) const fn case_insensitive_tools(self) -> bool {
        matches!(self.tool_case, ToolCase::Insensitive)
    }

    pub(super) const fn rejects_backticks(self) -> bool {
        matches!(self.backticks, BacktickPolicy::Reject)
    }

    pub(super) const fn command_substitution_backticks(self) -> Option<bool> {
        match self.command_substitutions {
            CommandSubstitutionPolicy::Inspect => Some(self.rejects_backticks()),
            CommandSubstitutionPolicy::Ignore => None,
        }
    }

    pub(super) const fn rejects_just_interpolation(self) -> bool {
        matches!(self.just_interpolation, JustInterpolationPolicy::Reject)
    }

    pub(super) const fn inspects_integrity(self) -> bool {
        matches!(self.integrity, IntegrityInspection::CommandsOnly | IntegrityInspection::TopLevel)
    }

    pub(super) const fn inspects_function_definitions(self) -> bool {
        matches!(self.integrity, IntegrityInspection::TopLevel)
    }

    pub(super) const fn initial_errexit(self) -> bool {
        self.initial_errexit
    }
}
