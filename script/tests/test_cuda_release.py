"""Tests for deterministic CUDA runtime materialization and validation."""

from __future__ import annotations

import hashlib
import io
import json
import shutil
import subprocess
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from script import package_release as PACKAGE
from script import prepare_cuda_runtime as PREPARE
from script import validate_cuda_runtime as VALIDATE


SCRIPT_DIR = Path(__file__).resolve().parent.parent
REPOSITORY_ROOT = SCRIPT_DIR.parent


class PrepareCudaRuntimeTests(unittest.TestCase):
    def test_repository_spec_is_structurally_valid(self) -> None:
        spec = PREPARE.load_spec(PREPARE.DEFAULT_SPEC)
        self.assertEqual(spec["artifact"]["target"], "x86_64-unknown-linux-gnu-cuda12")

    def test_materializes_only_declared_members_offline(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cache = root / "cache"
            cache.mkdir()
            archive = cache / "fixture.whl"
            with zipfile.ZipFile(archive, "w") as wheel:
                wheel.writestr("pkg/lib/libfixture.so", b"native")
                wheel.writestr("pkg/License.txt", b"license")
                wheel.writestr("pkg/unexpected.txt", b"do not extract")
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            spec = self.spec(digest)

            manifest_path = PREPARE.materialize(spec, cache, root / "runtime", True)

            self.assertEqual((root / "runtime/lib/libfixture.so").read_bytes(), b"native")
            self.assertEqual((root / "runtime/licenses/fixture.txt").read_bytes(), b"license")
            self.assertFalse((root / "runtime/pkg/unexpected.txt").exists())
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            self.assertEqual([record["path"] for record in manifest["files"]], [
                "lib/libfixture.so",
                "licenses/fixture.txt",
            ])

    def test_rejects_destination_traversal(self) -> None:
        spec = self.spec("0" * 64)
        spec["sources"][0]["files"][0]["destination"] = "../libfixture.so"
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "spec.json"
            path.write_text(json.dumps(spec), encoding="utf-8")
            with self.assertRaisesRegex(PREPARE.CudaRuntimeError, "normalized relative path"):
                PREPARE.load_spec(path)

    def test_archive_member_closes_zip_when_declared_member_is_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / "fixture.whl"
            with zipfile.ZipFile(archive, "w") as wheel:
                wheel.mkdir("directory/")
            container = zipfile.ZipFile(archive)
            with mock.patch.object(PREPARE.zipfile, "ZipFile", return_value=container):
                with self.assertRaisesRegex(PREPARE.CudaRuntimeError, "not a regular file"):
                    PREPARE.archive_member(archive, "zip", "directory/")
            self.assertIsNone(container.fp)

    def test_archive_member_closes_tar_when_declared_member_is_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / "fixture.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                info = tarfile.TarInfo("directory")
                info.type = tarfile.DIRTYPE
                tar.addfile(info)
            container = tarfile.open(archive, "r:gz")
            with mock.patch.object(PREPARE.tarfile, "open", return_value=container):
                with self.assertRaisesRegex(PREPARE.CudaRuntimeError, "not a regular file"):
                    PREPARE.archive_member(archive, "tar.gz", "directory")
            self.assertTrue(container.fileobj.closed)

    def test_closing_reader_always_closes_container(self) -> None:
        reader = mock.Mock()
        reader.close.side_effect = OSError("reader close failed")
        container = mock.Mock()
        closing_reader = PREPARE._ClosingReader(reader, container)
        with self.assertRaisesRegex(OSError, "reader close failed"):
            closing_reader.__exit__()
        container.close.assert_called_once_with()

    @staticmethod
    def spec(digest: str) -> dict[str, object]:
        return {
            "schema_version": 1,
            "artifact": {"target": "x86_64-unknown-linux-gnu-cuda12"},
            "compatibility": {},
            "system_libraries": ["libc.so.6"],
            "required_loaded_libraries": ["libfixture.so"],
            "prohibited_library_patterns": ["tensorrt"],
            "sources": [
                {
                    "id": "fixture",
                    "version": "1",
                    "filename": "fixture.whl",
                    "url": "https://invalid.example/fixture.whl",
                    "sha256": digest,
                    "archive": "zip",
                    "files": [
                        {"source": "pkg/lib/libfixture.so", "destination": "lib/libfixture.so"},
                        {"source": "pkg/License.txt", "destination": "licenses/fixture.txt"},
                    ],
                }
            ],
        }


class ValidateCudaRuntimeTests(unittest.TestCase):
    def test_parse_needed(self) -> None:
        output = """
 0x0000000000000001 (NEEDED) Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED) Shared library: [libm.so.6]
"""
        self.assertEqual(VALIDATE.parse_needed(output), {"libc.so.6", "libm.so.6"})

    def test_accelerator_runtime_classification_keeps_driver_host_owned(self) -> None:
        self.assertTrue(VALIDATE.is_accelerator_runtime_library("libcudnn.so.9"))
        self.assertTrue(VALIDATE.is_accelerator_runtime_library("libonnxruntime_providers_cuda.so"))
        self.assertFalse(VALIDATE.is_accelerator_runtime_library("libcuda.so.1"))
        self.assertFalse(VALIDATE.is_accelerator_runtime_library("libc.so.6"))

    def test_rejects_nested_runtime_libraries(self) -> None:
        manifest = {
            "files": [
                {
                    "path": "lib/nested/libfixture.so",
                    "size": 1,
                    "sha256": "0" * 64,
                }
            ]
        }
        with self.assertRaisesRegex(VALIDATE.ValidationError, "directly under lib"):
            VALIDATE.declared_files(manifest)

    def test_validates_exact_inventory_and_hashes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "lib").mkdir()
            (root / "licenses").mkdir()
            (root / "notices").mkdir()
            library = root / "lib/@libfixture.so"
            shutil.copy2("/bin/true", library)
            manifest = {
                "schema_version": 1,
                "system_libraries": sorted(VALIDATE.needed_libraries(library)),
                "prohibited_library_patterns": ["tensorrt"],
                "files": [
                    {
                        "path": "lib/@libfixture.so",
                        "size": library.stat().st_size,
                        "sha256": VALIDATE.sha256_file(library),
                    }
                ],
            }
            self.assertEqual(VALIDATE.validate_files(root, manifest), {"@libfixture.so"})

            (root / "notices/unexpected.txt").write_text("unexpected", encoding="utf-8")
            with self.assertRaisesRegex(VALIDATE.ValidationError, "inventory mismatch"):
                VALIDATE.validate_files(root, manifest)


class PackageReleaseTests(unittest.TestCase):
    def test_archive_permissions_are_normalized(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "localhold-v1-test"
            (stage / "bin").mkdir(parents=True)
            (stage / "docs").mkdir()
            executable = stage / "bin/hold"
            regular = stage / "docs/guide.md"
            executable.write_bytes(b"fixture binary")
            regular.write_text("guide\n", encoding="utf-8")
            stage.chmod(0o700)
            (stage / "bin").chmod(0o700)
            (stage / "docs").chmod(0o700)
            executable.chmod(0o700)
            regular.chmod(0o600)

            first_tar = io.BytesIO()
            PACKAGE.write_tar(stage, first_tar, 1_700_000_000)
            first_zip = root / "first.zip"
            PACKAGE.write_zip(stage, first_zip, 1_700_000_000)

            stage.chmod(0o755)
            (stage / "bin").chmod(0o777)
            (stage / "docs").chmod(0o755)
            executable.chmod(0o755)
            regular.chmod(0o666)
            second_tar = io.BytesIO()
            PACKAGE.write_tar(stage, second_tar, 1_700_000_000)
            second_zip = root / "second.zip"
            PACKAGE.write_zip(stage, second_zip, 1_700_000_000)

            self.assertEqual(first_tar.getvalue(), second_tar.getvalue())
            self.assertEqual(first_zip.read_bytes(), second_zip.read_bytes())
            with tarfile.open(fileobj=io.BytesIO(first_tar.getvalue()), mode="r:") as archive:
                modes = {member.name: member.mode for member in archive.getmembers()}
            self.assertEqual(modes["localhold-v1-test"], 0o755)
            self.assertEqual(modes["localhold-v1-test/bin/hold"], 0o755)
            self.assertEqual(modes["localhold-v1-test/docs/guide.md"], 0o644)
            with zipfile.ZipFile(first_zip) as archive:
                modes = {info.filename: (info.external_attr >> 16) & 0o777 for info in archive.infolist()}
            self.assertEqual(modes["localhold-v1-test/"], 0o755)
            self.assertEqual(modes["localhold-v1-test/bin/hold"], 0o755)
            self.assertEqual(modes["localhold-v1-test/docs/guide.md"], 0o644)

    def test_streamed_tar_zst_preserves_failed_command_details(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "not-created.tar.zst"
            expected = [
                "zstd",
                "-19",
                "--threads=1",
                "--no-progress",
                "--quiet",
                "--force",
                "-o",
                str(destination),
            ]
            compressor = mock.Mock(args=expected)
            compressor.stdin = mock.Mock()
            compressor.wait.return_value = 23
            with (
                mock.patch.object(PACKAGE, "write_tar"),
                mock.patch.object(PACKAGE.subprocess, "Popen", return_value=compressor) as popen,
                self.assertRaises(subprocess.CalledProcessError) as raised,
            ):
                PACKAGE.write_tar_zst(Path(temporary) / "stage", destination, 1_700_000_000)

            popen.assert_called_once_with(expected, stdin=subprocess.PIPE)
            self.assertEqual(raised.exception.returncode, 23)
            self.assertEqual(raised.exception.cmd, expected)

    def test_streamed_tar_zst_is_reproducible(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "localhold-v1-test"
            (stage / "bin").mkdir(parents=True)
            binary = stage / "bin/hold"
            binary.write_bytes(b"fixture binary")
            binary.chmod(0o755)
            first = root / "first.tar.zst"
            second = root / "second.tar.zst"

            PACKAGE.write_tar_zst(stage, first, 1_700_000_000)
            PACKAGE.write_tar_zst(stage, second, 1_700_000_000)

            self.assertEqual(first.read_bytes(), second.read_bytes())
            with first.open("rb") as archive:
                subprocess.run(["zstd", "--test", "-"], check=True, stdin=archive, capture_output=True)


class GpuReleaseWorkflowTests(unittest.TestCase):
    def test_missing_dependency_assertion_uses_doctor_summary(self) -> None:
        workflow = (REPOSITORY_ROOT / ".github/workflows/gpu-release-gate.yml").read_text(
            encoding="utf-8"
        )
        diagnostic = 'contains("bundled CUDA 12 runtime")'

        self.assertIn(f".summary | {diagnostic}", workflow)
        self.assertNotIn(f".message | {diagnostic}", workflow)


if __name__ == "__main__":
    unittest.main()
