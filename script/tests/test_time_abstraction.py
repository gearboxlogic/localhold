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
        source_path: str = "src/example.rs",
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "script").mkdir()
            if source is not None:
                path = root / source_path
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source, encoding="utf-8")
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

    def test_scans_an_unconditional_module_named_tests(self) -> None:
        result = self._run_check(
            "mod tests {\n"
            "    pub fn production() { std::thread::sleep(std::time::Duration::ZERO); }\n"
            "}\n"
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("direct time access bypasses Clock", result.stderr)

    def test_test_cfg_remains_active_across_other_attributes(self) -> None:
        result = self._run_check(
            "#[cfg(test)]\n"
            "#[expect(dead_code, reason = \"test helper\")]\n"
            "mod tests {\n"
            "    fn helper() { std::thread::sleep(std::time::Duration::ZERO); }\n"
            "}\n"
        )

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_test_cfg_accepts_a_module_opening_split_across_lines(self) -> None:
        result = self._run_check(
            "#[cfg(test)]\n"
            "mod tests\n"
            "{\n"
            "    fn helper() { std::thread::sleep(std::time::Duration::ZERO); }\n"
            "}\n"
            "fn production() {}\n"
        )

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_ignores_time_patterns_inside_production_literals_and_comments(self) -> None:
        result = self._run_check(
            "const NORMAL: &str = \"Utc::now( and std::thread::sleep(\";\n"
            "const RAW: &str = r###\"SystemTime::now( and tokio::time::timeout(\"###;\n"
            "// Instant::now(\n"
            "/* tokio::time::sleep(\n"
            "   std::thread::sleep( */\n"
            "fn production() {}\n"
        )

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_all_cfg_requires_test_before_skipping_module(self) -> None:
        skipped = self._run_check(
            "#[cfg(all(feature = \"reranker\", test))]\n"
            "mod tests {\n"
            "    fn helper() { std::thread::sleep(std::time::Duration::ZERO); }\n"
            "}\n"
        )
        scanned = self._run_check(
            "#[cfg(any(feature = \"testing\", test))]\n"
            "mod tests {\n"
            "    pub fn production() { std::thread::sleep(std::time::Duration::ZERO); }\n"
            "}\n"
        )

        self.assertEqual(skipped.returncode, 0, skipped.stderr)
        self.assertNotEqual(scanned.returncode, 0)
        self.assertIn("direct time access bypasses Clock", scanned.stderr)

    def test_multiline_all_cfg_skips_only_a_test_module(self) -> None:
        skipped = self._run_check(
            "#[cfg(all(\n"
            "    feature = \"reranker\",\n"
            "    test,\n"
            "))]\n"
            "mod tests {\n"
            "    fn helper() { std::thread::sleep(std::time::Duration::ZERO); }\n"
            "}\n"
        )
        scanned = self._run_check(
            "#[cfg(any(\n"
            "    feature = \"testing\",\n"
            "    test,\n"
            "))]\n"
            "mod tests {\n"
            "    pub fn production() { std::thread::sleep(std::time::Duration::ZERO); }\n"
            "}\n"
        )

        self.assertEqual(skipped.returncode, 0, skipped.stderr)
        self.assertNotEqual(scanned.returncode, 0)
        self.assertIn("direct time access bypasses Clock", scanned.stderr)

    def test_rejects_an_unterminated_multiline_cfg_attribute(self) -> None:
        result = self._run_check("#[cfg(all(\n    feature = \"testing\",\n    test,\n")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unterminated cfg attribute", result.stderr)

    def test_tracks_multiline_test_function_braces(self) -> None:
        result = self._run_check(
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    #[test]\n"
            "    fn first() {\n"
            "        std::time::SystemTime::now();\n"
            "    }\n"
            "    #[test]\n"
            "    fn second() {\n"
            "        std::thread::sleep(std::time::Duration::ZERO);\n"
            "    }\n"
            "}\n"
            "fn production() {}\n"
        )

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_ignores_braces_inside_test_literals_and_comments(self) -> None:
        result = self._run_check(
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    /* nested { comment /* } */ remains ignored } */\n"
            "    #[test]\n"
            "    fn literals() {\n"
            "        let _ = \"}\"; // }\n"
            "        let _ = r###\"{ raw }\"###;\n"
            "        let _ = '}';\n"
            "        std::time::SystemTime::now();\n"
            "    }\n"
            "}\n"
            "fn production() {}\n"
        )

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_an_unclosed_inline_test_module(self) -> None:
        result = self._run_check("#[cfg(test)]\nmod tests {\n    fn unfinished() {}\n")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not identify the inline test-module boundary", result.stderr)

    def test_ignores_test_module_markers_inside_multiline_literals_and_comments(self) -> None:
        result = self._run_check(
            "const DOCUMENTATION: &str = r###\"\n"
            "mod tests {\n"
            "\"###;\n"
            "/*\n"
            "mod tests {\n"
            "*/\n"
            "fn production() {}\n"
        )

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_discovers_rust_sources_in_nested_directories(self) -> None:
        result = self._run_check(
            "fn production() { std::time::SystemTime::now(); }\n",
            source_path="src/nested/example.rs",
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("direct time access bypasses Clock", result.stderr)

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
