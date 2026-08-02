use sha2::{Digest, Sha256};

// These sources have reviewed dynamic writes confined by their complete
// release-staging, temporary-fixture, or brand-output control flow. A source
// change invalidates the whole-file pin and must land through the protected
// checker ratchet before that changed writer can execute in governed CI.
const REVIEWED_DYNAMIC_WRITE_SURFACES: &[(&str, &str)] = &[
    ("assets/brand/explorations/fortgen.py", "87c07e9806016fd4bc348ddc6c2e7f9ade770919c4e050c7084d9e9dc64bfca7"),
    ("assets/brand/explorations/round2.py", "6044c0d67fcbaa66c71a547e63487ebf59c22e02fb10ca901e545141bb950459"),
    ("assets/brand/explorations/round3.py", "bcb3af485db2d08a6e3a0c561444c38b565bbe76a8a4b6a2dfc3f5dd71c4fab6"),
    ("script/package_release.py", "163b91d31ae73bdee732512ac56a615330507a978cfa78d5cd680e008d3a87a4"),
    ("script/prepare_cuda_runtime.py", "dbad298e363fefdc0a557fa023c943337aa0d423794d78daf8fa7de9fe5dd494"),
    ("script/release.py", "81490a55ea69c1119411621a9d1da558bae8b574d16ac9e57e069e73f3c284ea"),
    ("script/tests/test_cuda_release.py", "850414f812aeddcf692d47b9cff1a820959aafea2fc25443161739010b6b850f"),
    ("script/tests/test_database_fixtures.py", "616df3e8d2f444fcd24a0b668eb3e492100fc465f53f041e7a0ca41555247b57"),
    ("script/tests/test_time_abstraction.py", "b797b46d0f6c1ebe3ef8496dfa7e1e6e81d02190430dd62b8b3ae83282e07c40"),
];

pub(super) fn is_reviewed_dynamic_write_surface(path: &str, source: &str) -> bool {
    let Some((_, expected)) = REVIEWED_DYNAMIC_WRITE_SURFACES.iter().find(|(reviewed, _)| *reviewed == path) else {
        return false;
    };
    format!("{:x}", Sha256::digest(source.as_bytes())) == *expected
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn reviewed_dynamic_writers_are_whole_file_pins() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (path, _) in REVIEWED_DYNAMIC_WRITE_SURFACES {
            let source = fs::read_to_string(workspace.join(path)).expect("read reviewed Python writer");
            assert!(is_reviewed_dynamic_write_surface(path, &source), "{path}");
            assert!(!is_reviewed_dynamic_write_surface(path, &(source + "\n# changed\n")), "{path}");
        }
    }
}
