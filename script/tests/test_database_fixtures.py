"""Tests for historical database fixture validation."""

import copy
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import script.database_fixtures as database_fixtures
from script.database_fixtures import (
    FIXTURE_ROOT,
    FixtureError,
    expand_fixture,
    sha256,
    validate_manifest,
)


class DatabaseFixtureTests(unittest.TestCase):
    def _copied_manifest(self, directory: str) -> tuple[Path, dict[str, object]]:
        fixture_root = Path(directory) / "database-upgrades"
        shutil.copytree(FIXTURE_ROOT, fixture_root)
        manifest_path = fixture_root / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        return manifest_path, manifest

    @staticmethod
    def _write_manifest(path: Path, manifest: dict[str, object]) -> None:
        path.write_text(json.dumps(manifest), encoding="utf-8")

    def _future_release(self, manifest: dict[str, object]) -> dict[str, object]:
        releases = manifest["releases"]
        assert isinstance(releases, list)
        future = copy.deepcopy(releases[-1])
        assert isinstance(future, dict)
        future["tag"] = "v0.3.0"
        commit = database_fixtures._git_commit("HEAD^", database_fixtures.REPO_ROOT)
        self.assertIsNotNone(commit)
        future["commit"] = commit
        for backend in ("sqlite", "postgres"):
            entry = future[backend]
            assert isinstance(entry, dict)
            source = entry["source"]
            assert isinstance(source, str)
            source_bytes = database_fixtures._git_bytes("HEAD", source, database_fixtures.REPO_ROOT)
            self.assertIsNotNone(source_bytes)
            entry["source_sha256"] = sha256(source_bytes or b"")
        return future

    def _ancestor_with_different_source(self, source: str, expected: str) -> str:
        result = subprocess.run(
            ["git", "rev-list", "HEAD", "--", source],
            cwd=database_fixtures.REPO_ROOT,
            check=True,
            capture_output=True,
            text=True,
        )
        for commit in result.stdout.splitlines():
            source_bytes = database_fixtures._git_bytes(commit, source, database_fixtures.REPO_ROOT)
            if source_bytes is not None and sha256(source_bytes) != expected:
                return commit
        self.fail(f"Git history has no ancestor with different source bytes for {source}")

    def test_repository_manifest_and_checksums_are_valid(self) -> None:
        validate_manifest("v0.2.0")

    def test_requested_release_must_have_fixture_coverage(self) -> None:
        with self.assertRaisesRegex(FixtureError, "missing for release v9.9.9"):
            validate_manifest("v9.9.9")

    def test_inventory_rejects_missing_published_release(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_path, manifest = self._copied_manifest(directory)
            releases = manifest["releases"]
            assert isinstance(releases, list)
            manifest["releases"] = [release for release in releases if release["tag"] != "v0.1.0-beta.2"]
            self._write_manifest(manifest_path, manifest)
            with self.assertRaisesRegex(FixtureError, "missing published releases: v0.1.0-beta.2"):
                validate_manifest(manifest_path=manifest_path)

    def test_inventory_rejects_untrusted_extra_release(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_path, manifest = self._copied_manifest(directory)
            releases = manifest["releases"]
            assert isinstance(releases, list)
            extra = copy.deepcopy(releases[-1])
            extra["tag"] = "v9.9.9"
            releases.append(extra)
            self._write_manifest(manifest_path, manifest)
            with self.assertRaisesRegex(FixtureError, "outside trusted published inventory: v9.9.9"):
                validate_manifest(manifest_path=manifest_path)

    def test_v020_effective_checksum_covers_included_fixture_content(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_path, manifest = self._copied_manifest(directory)
            fixture_root = manifest_path.parent
            included = fixture_root / "v0.1.0-beta.2-beta.3.sqlite.sql"
            included.write_text(included.read_text(encoding="utf-8") + "\n-- tampered\n", encoding="utf-8")
            releases = manifest["releases"]
            assert isinstance(releases, list)
            tampered_base_hash = sha256(expand_fixture(included, fixture_root))
            for release in releases[:2]:
                release["sqlite"]["fixture_sha256"] = tampered_base_hash
            self._write_manifest(manifest_path, manifest)
            with self.assertRaisesRegex(FixtureError, "v0.2.0 sqlite fixture checksum mismatch"):
                validate_manifest(manifest_path=manifest_path)

    def test_fixture_include_cycle_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "a.sql").write_text("-- fixture-include: b.sql\n", encoding="utf-8")
            (root / "b.sql").write_text("-- fixture-include: a.sql\n", encoding="utf-8")
            with self.assertRaisesRegex(FixtureError, "include cycle"):
                expand_fixture(root / "a.sql", root)

    def test_fixture_include_traversal_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "a.sql").write_text("-- fixture-include: ../outside.sql\n", encoding="utf-8")
            with self.assertRaisesRegex(FixtureError, "must be a basename"):
                expand_fixture(root / "a.sql", root)

    def test_missing_historical_tag_fails_closed(self) -> None:
        real_git_commit = database_fixtures._git_commit

        def missing_beta2(ref: str, repo_root: Path) -> str | None:
            if ref == "v0.1.0-beta.2":
                return None
            return real_git_commit(ref, repo_root)

        with mock.patch("script.database_fixtures._git_commit", side_effect=missing_beta2):
            with self.assertRaisesRegex(FixtureError, "historical tag is unavailable"):
                validate_manifest("v0.2.0")

    def test_pretag_release_uses_exact_head_and_ancestor_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_path, manifest = self._copied_manifest(directory)
            future = self._future_release(manifest)
            releases = manifest["releases"]
            assert isinstance(releases, list)
            releases.append(future)
            self._write_manifest(manifest_path, manifest)
            inventory = database_fixtures.PUBLISHED_DATABASE_RELEASES | {"v0.3.0"}
            with mock.patch("script.database_fixtures.PUBLISHED_DATABASE_RELEASES", inventory):
                validate_manifest("v0.3.0", manifest_path=manifest_path)

    def test_pretag_release_rejects_existing_nonancestor_commit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_path, manifest = self._copied_manifest(directory)
            future = self._future_release(manifest)
            releases = manifest["releases"]
            assert isinstance(releases, list)
            releases.append(future)
            self._write_manifest(manifest_path, manifest)
            inventory = database_fixtures.PUBLISHED_DATABASE_RELEASES | {"v0.3.0"}
            real_is_ancestor = database_fixtures._git_is_ancestor
            head_commit = database_fixtures._git_commit("HEAD", database_fixtures.REPO_ROOT)
            future_commit = future["commit"]

            def reject_future(ancestor: str, descendant: str, repo_root: Path) -> bool:
                if ancestor == future_commit and descendant == head_commit:
                    return False
                return real_is_ancestor(ancestor, descendant, repo_root)

            with (
                mock.patch("script.database_fixtures.PUBLISHED_DATABASE_RELEASES", inventory),
                mock.patch("script.database_fixtures._git_is_ancestor", side_effect=reject_future),
            ):
                with self.assertRaisesRegex(FixtureError, "is not an ancestor of HEAD"):
                    validate_manifest("v0.3.0", manifest_path=manifest_path)

    def test_pretag_release_rejects_spoofed_commit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_path, manifest = self._copied_manifest(directory)
            future = self._future_release(manifest)
            future["commit"] = "0" * 40
            releases = manifest["releases"]
            assert isinstance(releases, list)
            releases.append(future)
            self._write_manifest(manifest_path, manifest)
            inventory = database_fixtures.PUBLISHED_DATABASE_RELEASES | {"v0.3.0"}
            with mock.patch("script.database_fixtures.PUBLISHED_DATABASE_RELEASES", inventory):
                with self.assertRaisesRegex(FixtureError, "is not an ancestor of HEAD"):
                    validate_manifest("v0.3.0", manifest_path=manifest_path)

    def test_pretag_release_rejects_ancestor_whose_declared_source_differs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_path, manifest = self._copied_manifest(directory)
            future = self._future_release(manifest)
            sqlite_entry = future["sqlite"]
            assert isinstance(sqlite_entry, dict)
            expected = sqlite_entry["source_sha256"]
            source = sqlite_entry["source"]
            assert isinstance(expected, str)
            assert isinstance(source, str)
            future["commit"] = self._ancestor_with_different_source(source, expected)
            releases = manifest["releases"]
            assert isinstance(releases, list)
            releases.append(future)
            self._write_manifest(manifest_path, manifest)
            inventory = database_fixtures.PUBLISHED_DATABASE_RELEASES | {"v0.3.0"}
            with mock.patch("script.database_fixtures.PUBLISHED_DATABASE_RELEASES", inventory):
                with self.assertRaisesRegex(FixtureError, "declared provenance commit source checksum mismatch"):
                    validate_manifest("v0.3.0", manifest_path=manifest_path)

    def test_pretag_release_rejects_source_hash_not_from_exact_head(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_path, manifest = self._copied_manifest(directory)
            future = self._future_release(manifest)
            sqlite_entry = future["sqlite"]
            assert isinstance(sqlite_entry, dict)
            head_hash = sqlite_entry["source_sha256"]
            source = sqlite_entry["source"]
            assert isinstance(head_hash, str)
            assert isinstance(source, str)
            commit = self._ancestor_with_different_source(source, head_hash)
            future["commit"] = commit
            for backend in ("sqlite", "postgres"):
                entry = future[backend]
                assert isinstance(entry, dict)
                backend_source = entry["source"]
                assert isinstance(backend_source, str)
                source_bytes = database_fixtures._git_bytes(commit, backend_source, database_fixtures.REPO_ROOT)
                self.assertIsNotNone(source_bytes)
                entry["source_sha256"] = sha256(source_bytes or b"")
            releases = manifest["releases"]
            assert isinstance(releases, list)
            releases.append(future)
            self._write_manifest(manifest_path, manifest)
            inventory = database_fixtures.PUBLISHED_DATABASE_RELEASES | {"v0.3.0"}
            with mock.patch("script.database_fixtures.PUBLISHED_DATABASE_RELEASES", inventory):
                with self.assertRaisesRegex(FixtureError, "v0.3.0 sqlite release source checksum mismatch"):
                    validate_manifest("v0.3.0", manifest_path=manifest_path)

    def test_fixture_checksum_cannot_be_manifest_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_path, manifest = self._copied_manifest(directory)
            releases = manifest["releases"]
            assert isinstance(releases, list)
            releases[0]["sqlite"]["fixture_sha256"] = "0" * 64
            self._write_manifest(manifest_path, manifest)
            with self.assertRaisesRegex(FixtureError, "fixture checksum mismatch"):
                validate_manifest(manifest_path=manifest_path)


if __name__ == "__main__":
    unittest.main()
