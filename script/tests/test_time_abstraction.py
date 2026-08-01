"""Tests for the time-abstraction source gate."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


CHECK = Path(__file__).resolve().parents[1] / "check-time-abstraction.sh"


class TimeAbstractionTests(unittest.TestCase):
    def _run_check(self, source: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "script").mkdir()
            (root / "src/example.rs").write_text(source, encoding="utf-8")
            shutil.copy2(CHECK, root / "script/check-time-abstraction.sh")
            return subprocess.run(
                ["script/check-time-abstraction.sh"],
                cwd=root,
                check=False,
                capture_output=True,
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


if __name__ == "__main__":
    unittest.main()
