use sha2::{Digest, Sha256};

// These exact test sources patch statically named dependencies and never use
// the resolved objects as dynamic command dispatchers. The pins apply only to
// the otherwise opaque binding check; every other process check still runs.
struct ReviewedProcessBindingSurface {
    path: &'static str,
    sha256: &'static [&'static str],
}

const REVIEWED_PROCESS_BINDING_SURFACES: &[ReviewedProcessBindingSurface] = &[
    ReviewedProcessBindingSurface {
        path: "script/tests/test_cuda_release.py",
        sha256: &[
            "850414f812aeddcf692d47b9cff1a820959aafea2fc25443161739010b6b850f",
            "9b78de542a72594628965dff2d15d100463f128c93cb98dcab39ebf289a7ced3",
        ],
    },
    ReviewedProcessBindingSurface {
        path: "script/tests/test_database_fixtures.py",
        sha256: &["616df3e8d2f444fcd24a0b668eb3e492100fc465f53f041e7a0ca41555247b57"],
    },
];

pub(super) fn is_reviewed_surface(path: &str, source: &str) -> bool {
    REVIEWED_PROCESS_BINDING_SURFACES
        .iter()
        .find(|surface| surface.path == path)
        .is_some_and(|surface| surface.sha256.contains(&format!("{:x}", Sha256::digest(source.as_bytes())).as_str()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn process_binding_exception_is_a_whole_file_pin() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for surface in REVIEWED_PROCESS_BINDING_SURFACES {
            let source = fs::read_to_string(workspace.join(surface.path)).expect("read reviewed Python process binding surface");
            assert!(is_reviewed_surface(surface.path, &source));
            assert!(!is_reviewed_surface(surface.path, &(source + "\n# changed\n")));
        }
    }
}
