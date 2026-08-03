use sha2::{Digest, Sha256};

// This exact test source passes the package's subprocess module to
// unittest.mock.patch.object. The pin is consulted only for the otherwise
// opaque module-as-a-value binding check; all other process checks still run.
const REVIEWED_PROCESS_BINDING_PATH: &str = "script/tests/test_cuda_release.py";
const REVIEWED_PROCESS_BINDING_HASHES: &[&str] = &[
    "850414f812aeddcf692d47b9cff1a820959aafea2fc25443161739010b6b850f",
    "9b78de542a72594628965dff2d15d100463f128c93cb98dcab39ebf289a7ced3",
];

pub(super) fn is_reviewed_surface(path: &str, source: &str) -> bool {
    path == REVIEWED_PROCESS_BINDING_PATH && REVIEWED_PROCESS_BINDING_HASHES.contains(&format!("{:x}", Sha256::digest(source.as_bytes())).as_str())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn process_binding_exception_is_a_whole_file_pin() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = REVIEWED_PROCESS_BINDING_PATH;
        let source = fs::read_to_string(workspace.join(path)).expect("read reviewed Python process binding surface");
        assert!(is_reviewed_surface(path, &source));
        assert!(!is_reviewed_surface(path, &(source + "\n# changed\n")));
    }
}
