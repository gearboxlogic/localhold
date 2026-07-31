#!/usr/bin/env python3
"""Run LocalHold's Python release-tooling tests from an auditable file."""

import sys
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
suite = unittest.defaultTestLoader.discover("script/tests")
result = unittest.TextTestRunner(verbosity=2).run(suite)
raise SystemExit(not result.wasSuccessful())
