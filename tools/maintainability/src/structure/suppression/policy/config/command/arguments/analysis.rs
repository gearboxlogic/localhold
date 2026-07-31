#[derive(Clone, Copy)]
pub(super) struct Options {
    tool_case: ToolCase,
    backticks: BacktickPolicy,
    just_interpolation: JustInterpolationPolicy,
    integrity: IntegrityInspection,
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
enum JustInterpolationPolicy {
    Allow,
    Reject,
}

#[derive(Clone, Copy)]
enum IntegrityInspection {
    Nested,
    TopLevel,
}

impl Options {
    pub(super) const fn strict() -> Self {
        Self {
            tool_case: ToolCase::Sensitive,
            backticks: BacktickPolicy::Reject,
            just_interpolation: JustInterpolationPolicy::Reject,
            integrity: IntegrityInspection::TopLevel,
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

    pub(super) const fn allow_just_interpolation(mut self) -> Self {
        self.just_interpolation = JustInterpolationPolicy::Allow;
        self
    }

    pub(super) const fn nested(mut self) -> Self {
        self.integrity = IntegrityInspection::Nested;
        self
    }

    pub(super) const fn case_insensitive_tools(self) -> bool {
        matches!(self.tool_case, ToolCase::Insensitive)
    }

    pub(super) const fn rejects_backticks(self) -> bool {
        matches!(self.backticks, BacktickPolicy::Reject)
    }

    pub(super) const fn rejects_just_interpolation(self) -> bool {
        matches!(self.just_interpolation, JustInterpolationPolicy::Reject)
    }

    pub(super) const fn inspects_integrity(self) -> bool {
        matches!(self.integrity, IntegrityInspection::TopLevel)
    }
}
