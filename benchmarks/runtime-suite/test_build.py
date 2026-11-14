"""Build provenance must fail closed, even when Cargo exits successfully."""

import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import build


class BuildTests(unittest.TestCase):
    def invoke(self, missing=False, unoptimized=False, changed=False, exit_status=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "build"
            source = {"commit": "fixed", "diff_sha256": "diff", "untracked": {}}
            names = build.BINARY_NAMES[:-1] if missing else build.BINARY_NAMES

            def execute(command, env, record, timeout):
                events = []
                for name in names:
                    executable = root / name
                    executable.write_bytes(b"frozen executable")
                    events.append({
                        "reason": "compiler-artifact", "target": {"name": name},
                        "executable": str(executable),
                        "profile": {"debug_assertions": unoptimized,
                                    "opt_level": "0" if unoptimized else "3"},
                    })
                events.append({"reason": "build-finished", "success": exit_status == 0})
                record.with_suffix(".stdout").write_text(
                    "\n".join(json.dumps(event) for event in events) + "\n"
                )
                record.with_suffix(".stderr").write_text("build diagnostics")
                return exit_status, 1

            with mock.patch("sys.argv", [
                "build.py", "--output", str(output), "--target-dir", str(root / "target")
            ]), mock.patch.object(
                build, "source_identity", side_effect=[source, {} if changed else source]
            ), mock.patch.object(
                build.subprocess, "check_output", side_effect=["/native/toolchain/cargo\n", "rustc test"]
            ), mock.patch.object(build, "execute", side_effect=execute), (
                contextlib.redirect_stdout(io.StringIO())
            ), contextlib.redirect_stderr(io.StringIO()):
                status = build.main()
            return status, json.loads((output / "manifest.json").read_text())

    def test_success_records_exact_binaries_and_native_command(self):
        status, manifest = self.invoke()
        self.assertEqual(status, 0)
        self.assertEqual(manifest["status"], "built")
        self.assertEqual(set(manifest["binaries"]), set(build.BINARY_NAMES))
        self.assertEqual(manifest["command"][0], "/native/toolchain/cargo")
        self.assertEqual(manifest["rustflags"], "-D warnings")
        self.assertEqual(manifest["jobs"], 2)
        self.assertEqual(len(manifest["source_sha256"]), 64)

    def test_missing_executable_fails_despite_cargo_exit_zero(self):
        status, manifest = self.invoke(missing=True)
        self.assertEqual(status, 1)
        self.assertEqual(manifest["status"], "failed")
        self.assertIn("missing", manifest["failure"])

    def test_unoptimized_or_changed_source_cannot_be_frozen(self):
        for kwargs in ({"unoptimized": True}, {"changed": True}):
            with self.subTest(kwargs=kwargs):
                status, manifest = self.invoke(**kwargs)
                self.assertEqual(status, 1)
                self.assertEqual(manifest["status"], "failed")

    def test_cargo_failure_keeps_diagnostics_and_failed_manifest(self):
        status, manifest = self.invoke(exit_status=101)
        self.assertEqual(status, 1)
        self.assertEqual(manifest["exit_status"], 101)
        self.assertIn("Cargo failed", manifest["failure"])


if __name__ == "__main__":
    unittest.main()
