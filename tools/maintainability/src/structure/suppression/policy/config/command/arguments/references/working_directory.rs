use sha2::{Digest, Sha256};

pub(super) fn is_reviewed_relative_input_root(path: &str, source_is_reviewed: bool, source: &str) -> bool {
    const CHECKSUMS_AND_NOTES: &str =
        "cd dist\nsha256sum -- ./*.tar.zst ./*.zip > SHA256SUMS\ncd ..\npython3 script/release.py notes \"$GITHUB_REF_NAME\" --output release-notes.md\n";
    const PUBLICATION_HYGIENE_SHA256: &str = "75d0448a0b57b1ea47e5cee73d8893206a52316c20a27a49e63873472a60b680";
    path == "script/check-publication-hygiene.sh" && format!("{:x}", Sha256::digest(source.as_bytes())) == PUBLICATION_HYGIENE_SHA256
        || source_is_reviewed && path == ".github/workflows/release.yml" && source == CHECKSUMS_AND_NOTES
}
