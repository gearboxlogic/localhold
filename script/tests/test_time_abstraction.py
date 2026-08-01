"""Tests for the time-abstraction source gate."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


CHECK = Path(__file__).resolve().parents[1] / "check-time-abstraction.sh"


class TimeAbstractionTests(unittest.TestCase):
    def _run_check(
        self,
        source: str | None,
        with_failing_ripgrep: bool = False,
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "script").mkdir()
            if source is not None:
                (root / "src/example.rs").write_text(source, encoding="utf-8")
            shutil.copy2(CHECK, root / "script/check-time-abstraction.sh")
            environment = os.environ.copy()
            if with_failing_ripgrep:
                fake_bin = root / "fake-bin"
                fake_bin.mkdir()
                fake_rg = fake_bin / "rg"
                fake_rg.write_text("#!/bin/sh\nexit 127\n", encoding="utf-8")
                fake_rg.chmod(0o755)
                environment["PATH"] = f"{fake_bin}{os.pathsep}{environment['PATH']}"
            return subprocess.run(
                ["script/check-time-abstraction.sh"],
                cwd=root,
                check=False,
                capture_output=True,
                env=environment,
                text=True,
            )

    def test_scans_production_code_after_an_inline_test_module(self) -> None:
        result = self._run_check(
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    #[test]\n"
            "    fn uses_runtime_time() { std::time::SystemTime::now(); }\n"
            "}\n"
            "fn production() { std::time::SystemTime::now(); }\n"
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("direct time access bypasses Clock", result.stderr)

    def test_ignores_inline_tests_without_discarding_surrounding_code(self) -> None:
        result = self._run_check(
            "fn before() {}\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    #[test]\n"
            "    fn uses_runtime_time() { std::time::SystemTime::now(); }\n"
            "}\n"
            "fn after() {}\n"
        )

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_an_unclosed_inline_test_module(self) -> None:
        result = self._run_check("#[cfg(test)]\nmod tests {\n    fn unfinished() {}\n")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not identify the inline test-module boundary", result.stderr)

    def test_rejects_workspace_without_rust_sources(self) -> None:
        result = self._run_check(None)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("found no Rust sources", result.stderr)

    def test_does_not_require_ripgrep(self) -> None:
        result = self._run_check(
            "fn production() { std::time::SystemTime::now(); }\n",
            with_failing_ripgrep=True,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("direct time access bypasses Clock", result.stderr)


if __name__ == "__main__":
    unittest.main()
