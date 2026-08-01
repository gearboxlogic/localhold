#!/usr/bin/env python3
"""Run LocalHold's Python release-tooling tests from an auditable file."""

import sys
import unittest
from pathlib import Path


repository_root = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(repository_root))
suite = unittest.defaultTestLoader.discover(str(repository_root / "script/tests"))
if suite.countTestCases() == 0:
    raise SystemExit("no Python release-tooling tests were discovered")
result = unittest.TextTestRunner(verbosity=2).run(suite)
raise SystemExit(not result.wasSuccessful())
