"""The fixed D0 adapter must propagate timeout and preserve failed evidence."""

import argparse
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import run


class TimeoutTests(unittest.TestCase):
    def invoke(self, failure):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            args = argparse.Namespace(
                binary=Path(sys.executable), output=output, label="test",
                cases=["normal", "pressure"], repeat=2, timeout=1,
                warmup_per_agent=None, measured_per_agent=None,
                require_ready_admission=True,
            )
            with mock.patch.object(
                run.subprocess, "run", side_effect=failure
            ) as process, contextlib.redirect_stderr(io.StringIO()):
                code = run.run(args)
            manifest = json.loads((output / "test-manifest.json").read_text())
            streams = sorted(path.name for path in output.glob("*.std*"))
            return code, process.call_count, manifest, streams

    def test_timeout_stops_remaining_repeats_and_cases(self):
        code, calls, manifest, streams = self.invoke(subprocess.TimeoutExpired("fixture", 1))
        self.assertEqual((code, calls), (124, 1))
        self.assertEqual(manifest["failure_kind"], "timeout")
        self.assertEqual(manifest["runs"][0]["status"], "failed")
        self.assertNotIn("metrics", manifest["runs"][0])
        self.assertEqual(len(streams), 2)

    def test_ordinary_failures_still_preserve_each_independent_attempt(self):
        code, calls, manifest, streams = self.invoke(
            lambda *args, **kwargs: subprocess.CompletedProcess(args[0], 1)
        )
        self.assertEqual((code, calls), (1, 4))
        self.assertNotIn("failure_kind", manifest)
        self.assertTrue(all(entry["status"] == "failed" for entry in manifest["runs"]))
        self.assertEqual(len(streams), 8)


if __name__ == "__main__":
    unittest.main()
