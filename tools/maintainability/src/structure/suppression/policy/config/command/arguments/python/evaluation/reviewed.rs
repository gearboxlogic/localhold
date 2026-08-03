use sha2::{Digest, Sha256};

// These whole-file pins confine reviewed reflective Python operations. The
// classifier loader is also covered by the guarded atomic runtime profile; the
// CUDA archive wrapper uses getattr only to close its selected container.
struct ReviewedDynamicCodeSurface {
    path: &'static str,
    sha256: &'static [&'static str],
}

const REVIEWED_DYNAMIC_CODE_SURFACES: &[ReviewedDynamicCodeSurface] = &[
    ReviewedDynamicCodeSurface {
        path: "script/check_pr_classification.py",
        sha256: &["64f498229401c518ee377b5a74ec9f9c4c946b424316b49e979d5155469720e2"],
    },
    ReviewedDynamicCodeSurface {
        path: "script/prepare_cuda_runtime.py",
        sha256: &[
            "dbad298e363fefdc0a557fa023c943337aa0d423794d78daf8fa7de9fe5dd494",
            "b910ba9e57138f9381b02b154cae84c7c8f1ad1c4e2de510dd90fd9f3f727756",
        ],
    },
];

pub(super) fn matches(path: &str, source: &str) -> bool {
    REVIEWED_DYNAMIC_CODE_SURFACES
        .iter()
        .find(|reviewed| reviewed.path == path)
        .is_some_and(|reviewed| reviewed.sha256.contains(&format!("{:x}", Sha256::digest(source.as_bytes())).as_str()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn dynamic_code_exception_is_a_whole_file_pin() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for reviewed in REVIEWED_DYNAMIC_CODE_SURFACES {
            let candidate = workspace.join(reviewed.path);
            let Ok(source) = fs::read_to_string(&candidate) else {
                assert!(!candidate.exists(), "reviewed reflective Python surface must be a readable file: {}", reviewed.path);
                continue;
            };
            assert!(matches(reviewed.path, &source), "{}", reviewed.path);
            assert!(!matches(reviewed.path, &(source + "\n# changed\n")), "{}", reviewed.path);
        }
    }
}
