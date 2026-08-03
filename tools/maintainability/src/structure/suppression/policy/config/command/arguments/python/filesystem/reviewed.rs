use sha2::{Digest, Sha256};

// These sources have reviewed dynamic writes confined by their complete
// release-staging, temporary-fixture, or brand-output control flow. A source
// change invalidates the whole-file pin and must land through the protected
// checker ratchet before that changed writer can execute in governed CI.
struct ReviewedDynamicWriteSurface {
    path: &'static str,
    sha256: &'static [&'static str],
}

impl ReviewedDynamicWriteSurface {
    const fn new(path: &'static str, sha256: &'static [&'static str]) -> Self {
        Self { path, sha256 }
    }
}

const REVIEWED_DYNAMIC_WRITE_SURFACES: &[ReviewedDynamicWriteSurface] = &[
    ReviewedDynamicWriteSurface::new(
        "assets/brand/explorations/fortgen.py",
        &["87c07e9806016fd4bc348ddc6c2e7f9ade770919c4e050c7084d9e9dc64bfca7"],
    ),
    ReviewedDynamicWriteSurface::new("assets/brand/explorations/round2.py", &["6044c0d67fcbaa66c71a547e63487ebf59c22e02fb10ca901e545141bb950459"]),
    ReviewedDynamicWriteSurface::new("assets/brand/explorations/round3.py", &["bcb3af485db2d08a6e3a0c561444c38b565bbe76a8a4b6a2dfc3f5dd71c4fab6"]),
    ReviewedDynamicWriteSurface::new(
        "script/package_release.py",
        &[
            "163b91d31ae73bdee732512ac56a615330507a978cfa78d5cd680e008d3a87a4",
            "f3ec254d0fdf9d58b3f7f9f950e950b86af5574fefa13e02e71165af75d08c99",
        ],
    ),
    ReviewedDynamicWriteSurface::new(
        "script/prepare_cuda_runtime.py",
        &[
            "dbad298e363fefdc0a557fa023c943337aa0d423794d78daf8fa7de9fe5dd494",
            "b910ba9e57138f9381b02b154cae84c7c8f1ad1c4e2de510dd90fd9f3f727756",
        ],
    ),
    ReviewedDynamicWriteSurface::new(
        "script/release.py",
        &[
            "81490a55ea69c1119411621a9d1da558bae8b574d16ac9e57e069e73f3c284ea",
            "be6e7ba8613f8ea646043eab3c60ea352373e6d8c962361dc6e04e940ccc8e79",
        ],
    ),
    ReviewedDynamicWriteSurface::new(
        "script/tests/test_cuda_release.py",
        &[
            "850414f812aeddcf692d47b9cff1a820959aafea2fc25443161739010b6b850f",
            "9b78de542a72594628965dff2d15d100463f128c93cb98dcab39ebf289a7ced3",
        ],
    ),
    ReviewedDynamicWriteSurface::new(
        "script/tests/test_database_fixtures.py",
        &[
            "616df3e8d2f444fcd24a0b668eb3e492100fc465f53f041e7a0ca41555247b57",
            "12ee731aaeebc0c033354315ac2e4f464d97f9e1342bdad2f844e90a408d3b49",
        ],
    ),
    ReviewedDynamicWriteSurface::new(
        "script/tests/test_time_abstraction.py",
        &["b797b46d0f6c1ebe3ef8496dfa7e1e6e81d02190430dd62b8b3ae83282e07c40"],
    ),
];

pub(super) fn is_reviewed_dynamic_write_surface(path: &str, source: &str) -> bool {
    let Some(reviewed) = REVIEWED_DYNAMIC_WRITE_SURFACES.iter().find(|reviewed| reviewed.path == path) else {
        return false;
    };
    let actual = format!("{:x}", Sha256::digest(source.as_bytes()));
    reviewed.sha256.contains(&actual.as_str())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn reviewed_dynamic_writers_are_whole_file_pins() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for reviewed in REVIEWED_DYNAMIC_WRITE_SURFACES {
            let source = fs::read_to_string(workspace.join(reviewed.path)).expect("read reviewed Python writer");
            assert!(is_reviewed_dynamic_write_surface(reviewed.path, &source), "{}", reviewed.path);
            assert!(!is_reviewed_dynamic_write_surface(reviewed.path, &(source + "\n# changed\n")), "{}", reviewed.path);
        }
    }
}
