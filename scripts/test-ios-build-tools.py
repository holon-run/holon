#!/usr/bin/env python3
"""Run build/archive command contract regressions without starting Xcode."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
UUID = "12345678-1234-1234-1234-123456789abc"


class BuildToolContracts(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="holon-ios-build-tools-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.log = self.directory / "args.json"
        tool = self.directory / "xcodebuild"
        tool.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "with open(os.environ['TEST_ARGS'], 'w') as f: json.dump(sys.argv[1:], f)\n"
            "sys.exit(int(os.environ.get('TEST_EXIT', '0')))\n"
        )
        tool.chmod(0o755)
        self.env = {
            key: value for key, value in os.environ.items()
            if not key.startswith("IOS_")
        }
        self.env.update(
            PATH=str(self.directory) + os.pathsep + os.environ["PATH"],
            TEST_ARGS=str(self.log),
            IOS_SIMULATOR_ID=UUID,
            IOS_DERIVED_DATA_PATH=str(self.directory / "cold build"),
            IOS_RESULT_BUNDLE_PATH=str(self.directory / "test result.xcresult"),
        )

    def run_tool(self, script, *args, expected=0):
        result = subprocess.run(
            ["bash", str(ROOT / "scripts" / script), *args],
            env=self.env, capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, expected, result.stderr)
        return json.loads(self.log.read_text()) if self.log.exists() else None

    def test_build_and_test_forward_paths_and_action(self):
        for action in ("build", "test"):
            args = self.run_tool("test-ios-app.sh", action)
            self.assertEqual(args[-1], action)
            self.assertEqual(args[args.index("-derivedDataPath") + 1],
                             self.env["IOS_DERIVED_DATA_PATH"])
            self.assertEqual(args[args.index("-resultBundlePath") + 1],
                             self.env["IOS_RESULT_BUNDLE_PATH"])
            self.assertIn("platform=iOS Simulator,id=" + UUID, args)
            if action == "test":
                self.assertIn("-only-testing:HolonTests", args)
            else:
                self.assertFalse(any(arg.startswith("-only-testing:") for arg in args))
            self.assertFalse(Path(self.env["IOS_DERIVED_DATA_PATH"]).exists())

    def test_reject_unknown_action_and_extra_flags(self):
        for args in (("archive",), ("test", "-allowProvisioningUpdates")):
            self.run_tool("test-ios-app.sh", *args, expected=2)
            self.assertFalse(self.log.exists())

    def test_reject_invalid_paths_and_destination(self):
        for key, value in (
            ("IOS_DERIVED_DATA_PATH", "/"),
            ("IOS_DERIVED_DATA_PATH", "relative"),
            ("IOS_RESULT_BUNDLE_PATH", "/"),
            ("IOS_RESULT_BUNDLE_PATH", "relative"),
            ("IOS_RESULT_BUNDLE_PATH", str(self.directory)),
            ("IOS_SIMULATOR_ID", UUID + ",name=other"),
        ):
            original = self.env[key]
            self.env[key] = value
            self.run_tool("test-ios-app.sh", "test", expected=2)
            self.assertFalse(self.log.exists())
            self.env[key] = original

    def test_preserve_xcode_failure(self):
        self.env["TEST_EXIT"] = "65"
        self.run_tool("test-ios-app.sh", "test", expected=65)

    def test_optional_result_bundle_on_system_bash(self):
        del self.env["IOS_RESULT_BUNDLE_PATH"]
        result = subprocess.run(
            ["/bin/bash", str(ROOT / "scripts/test-ios-app.sh"), "build"],
            env=self.env, capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("-resultBundlePath", json.loads(self.log.read_text()))

    def test_archive_requires_explicit_team_and_unused_archive(self):
        self.run_tool("package-ios-archive.sh", expected=2)
        self.env["IOS_DEVELOPMENT_TEAM"] = "ABCDE12345"
        self.env["IOS_ARCHIVE_PATH"] = str(self.directory / "local.xcarchive")
        args = self.run_tool("package-ios-archive.sh")
        self.assertEqual(args[-1], "archive")
        self.assertIn("DEVELOPMENT_TEAM=ABCDE12345", args)
        self.assertNotIn("-allowProvisioningUpdates", args)
        self.assertFalse(any("PRODUCT_BUNDLE_IDENTIFIER" in arg for arg in args))
        self.env["TEST_EXIT"] = "65"
        self.run_tool("package-ios-archive.sh", expected=65)
        self.log.unlink()
        self.run_tool("package-ios-archive.sh", "-allowProvisioningUpdates", expected=2)
        self.assertFalse(self.log.exists())
        Path(self.env["IOS_ARCHIVE_PATH"]).mkdir()
        self.run_tool("package-ios-archive.sh", expected=2)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
