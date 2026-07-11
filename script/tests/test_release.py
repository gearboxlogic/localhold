"""Tests for release metadata validation helpers."""

import unittest

from script.release import references_tag


class ReferencesTagTests(unittest.TestCase):
    def test_matches_complete_tag(self) -> None:
        self.assertTrue(references_tag("git checkout v0.1.0-beta.1", "v0.1.0-beta.1"))

    def test_stable_tag_does_not_match_prerelease(self) -> None:
        self.assertFalse(references_tag("git checkout v0.1.0-beta.1", "v0.1.0"))

    def test_tag_does_not_match_inside_larger_token(self) -> None:
        self.assertFalse(references_tag("release-v0.1.0-notes", "v0.1.0"))


if __name__ == "__main__":
    unittest.main()
