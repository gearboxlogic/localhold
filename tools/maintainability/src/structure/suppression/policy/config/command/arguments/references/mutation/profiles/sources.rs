pub(super) struct SourceProfile {
    pub(super) id: &'static str,
    pub(super) path: &'static str,
    pub(super) current_sha256: &'static str,
    pub(super) preapproved_next_sha256: Option<&'static str>,
}

impl SourceProfile {
    pub(super) fn accepts(&self, sha256: &str) -> bool {
        sha256 == self.current_sha256 || self.preapproved_next_sha256 == Some(sha256)
    }
}

pub(super) const SOURCE_PROFILES: &[SourceProfile] = &[
    profile("justfile", "Justfile", "e7e0630e3bf9a4c042ab90c888fcdc46c3b9ccfd5c650d1b3fd69aa74c0df6f1"),
    profile(
        "fixture-bootstrap",
        "script/tests/test_maintainability_bootstrap.sh",
        "b469af64538abb9e03a251da06c93c329fee0d9b62d3347c145a45cfc1916b82",
    ),
    profile(
        "developer-bootstrap",
        "script/bootstrap.sh",
        "e0302179ecc01f9feb178b74420db156736c04e33a4e67d304fa3bc2390fdbf3",
    ),
    profile("installer", "script/install.sh", "20388ad69f99b25bc627f77cae34eaad837da4004e5db726eaa069dba2b2b2ac"),
    profile(
        "dependency-audit",
        "script/dep-audit.sh",
        "03b36529705c704b244dd5e128e1dd1461a66677bdda0bcceedaa582015160dc",
    ),
    profile(
        "postgres-smoke",
        "script/test-postgres-smoke.sh",
        "88a8e659f6e4c238041d037e4a49301806361a42c48706241c39ee8ad01e9724",
    ),
    profile(
        "protected-bootstrap",
        "script/check-maintainability-bootstrap.sh",
        "9443771a37693340cf16b45682f77c35c05353569c2c1aee0f23c74b49c0b4dc",
    ),
    profile(
        "gate-runner",
        "script/run-maintainability-gate.sh",
        "b15c0fe7aa61af07095bf174836269d7c3c98ee688fbab669305a6153123e257",
    ),
    profile(
        "source-safety-runner",
        "script/run-source-safety.sh",
        "cd756b8a6039e1192bb0c95e7c42e66148f7b883f3b12662b31c70269165a468",
    ),
    profile(
        "claude-review",
        "script/claude-review.sh",
        "88f24ff35d6c30eb4feb74be4fdb4c8039e873bcbede88015ebef779d0dc6c70",
    ),
    profile(
        "claude-review-tests",
        "script/tests/test_claude_review.sh",
        "e75b1d8db0dd62e5fe4fa93fc3e28ebc54545feeba8149c4198d3e996871ecbd",
    ),
    profile(
        "gpu-release-gate",
        ".github/workflows/gpu-release-gate.yml",
        "0765b8c7b974ef28bbc6daf54fc8f716a548918e387961b4a23cdbedb8591e1c",
    ),
    profile(
        "ci-workflow",
        ".github/workflows/ci.yml",
        "cb050701091e3ebd75c64feeed4b37ed80ad6cc08ba433697748e95b9f877e25",
    ),
    profile(
        "release-workflow",
        ".github/workflows/release.yml",
        "3f7a599eae1c47eb617347aaf08614f704a9c640a9de3891fb87a3094a5b4426",
    ),
    profile(
        "release-smoke-workflow",
        ".github/workflows/release-smoke.yml",
        "1a191677f19355451057ac62501bceecf923b4d09da96c649efe87412a622092",
    ),
    profile(
        "trusted-maintainability-workflow",
        ".github/workflows/trusted-maintainability.yml",
        "c2a4437c282c0a68f1be6e87358e657cc737b723bdeb48fc651e28aea5915556",
    ),
];

const fn profile(id: &'static str, path: &'static str, current_sha256: &'static str) -> SourceProfile {
    SourceProfile {
        id,
        path,
        current_sha256,
        preapproved_next_sha256: None,
    }
}
