pub(super) struct SourceProfile {
    pub(super) id: &'static str,
    pub(super) path: &'static str,
}

pub(super) const SOURCE_PROFILES: &[SourceProfile] = &[
    profile("justfile", "Justfile"),
    profile("fixture-bootstrap", "script/tests/test_maintainability_bootstrap.sh"),
    profile("developer-bootstrap", "script/bootstrap.sh"),
    profile("installer", "script/install.sh"),
    profile("dependency-audit", "script/dep-audit.sh"),
    profile("postgres-smoke", "script/test-postgres-smoke.sh"),
    profile("protected-bootstrap", "script/check-maintainability-bootstrap.sh"),
    profile("gate-runner", "script/run-maintainability-gate.sh"),
    profile("source-safety-runner", "script/run-source-safety.sh"),
    profile("claude-review", "script/claude-review.sh"),
    profile("claude-review-tests", "script/tests/test_claude_review.sh"),
    profile("gpu-release-gate", ".github/workflows/gpu-release-gate.yml"),
    profile("ci-workflow", ".github/workflows/ci.yml"),
    profile("release-workflow", ".github/workflows/release.yml"),
    profile("release-smoke-workflow", ".github/workflows/release-smoke.yml"),
    profile("trusted-maintainability-workflow", ".github/workflows/trusted-maintainability.yml"),
];

const fn profile(id: &'static str, path: &'static str) -> SourceProfile {
    SourceProfile { id, path }
}
