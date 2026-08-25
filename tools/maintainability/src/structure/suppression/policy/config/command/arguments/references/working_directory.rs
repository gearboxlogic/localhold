pub(super) fn is_reviewed_release_restoration(path: &str, source_is_reviewed: bool, source: &str) -> bool {
    const CHECKSUMS_AND_NOTES: &str =
        "cd dist\nsha256sum -- ./*.tar.zst ./*.zip > SHA256SUMS\ncd ..\npython3 script/release.py notes \"$GITHUB_REF_NAME\" --output release-notes.md\n";
    path == ".github/workflows/release.yml" && source_is_reviewed && source == CHECKSUMS_AND_NOTES
}
