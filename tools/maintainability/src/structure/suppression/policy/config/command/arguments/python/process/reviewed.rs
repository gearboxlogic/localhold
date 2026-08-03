use sha2::{Digest, Sha256};

// The pre-hardening fixture helper and release packager use cwd overrides or
// pathlib expressions for repository-rooted subprocess inputs. These exact
// sources remain accepted only while the delivery removes that indirection.
const REVIEWED_PROCESS_SURFACES: &[(&str, &str)] = &[
    ("script/database_fixtures.py", "698b288b56e2a16ea4878ec2f009b449fd4cd376d2e3c4358445ae4b4ed1fb3f"),
    ("script/package_release.py", "163b91d31ae73bdee732512ac56a615330507a978cfa78d5cd680e008d3a87a4"),
];

pub(super) fn matches(path: &str, source: &str) -> bool {
    REVIEWED_PROCESS_SURFACES
        .iter()
        .find(|(reviewed, _)| *reviewed == path)
        .is_some_and(|(_, expected)| format!("{:x}", Sha256::digest(source.as_bytes())) == *expected)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn staged_process_exception_is_a_whole_file_pin() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (path, _) in REVIEWED_PROCESS_SURFACES {
            let source = fs::read_to_string(workspace.join(path)).expect("read reviewed Python process surface");
            assert!(matches(path, &source), "{path}");
            assert!(!matches(path, &(source + "\n# changed\n")), "{path}");
        }
    }
}
