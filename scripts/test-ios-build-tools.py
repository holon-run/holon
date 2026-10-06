#!/usr/bin/env python3
"""Run build/archive and UI command contract regressions without starting Xcode."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import call, patch

from ios_simulator_text_size import (
    MAXIMUM_TEXT_SIZE, initialize_simulator_text_size, simulator_text_size,
)


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


class SimulatorTextSizeContracts(unittest.TestCase):
    @staticmethod
    def result(stdout=""):
        return subprocess.CompletedProcess([], 0, stdout=stdout, stderr="")

    def lifecycle(self, selected=MAXIMUM_TEXT_SIZE, restored="large"):
        return [self.result("large\n"), self.result(), self.result(selected + "\n"),
                self.result(), self.result(restored + "\n")]

    def test_initialize_dedicated_baseline_with_readback(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=[self.result(), self.result(), self.result("large\n")]) as run:
            initialize_simulator_text_size(UUID)
            prefix = ["xcrun", "simctl", "ui", UUID, "content_size"]
            self.assertEqual(run.call_args_list, [
                call(["xcrun", "simctl", "bootstatus", UUID, "-b"], check=True),
                call(prefix + ["large"], check=True, capture_output=True, text=True),
                call(prefix, check=True, capture_output=True, text=True),
            ])

    def test_failed_boot_prevents_baseline_configuration(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=subprocess.CalledProcessError(149, ["xcrun"])) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                initialize_simulator_text_size(UUID)
            run.assert_called_once_with(
                ["xcrun", "simctl", "bootstatus", UUID, "-b"], check=True,
            )

    def test_unverified_baseline_fails(self):
        for actual in ("medium", "unknown", "unsupported", ""):
            with self.subTest(actual=actual):
                with patch("ios_simulator_text_size.subprocess.run",
                           side_effect=[self.result(), self.result(), self.result(actual)]) as run:
                    with self.assertRaises(RuntimeError):
                        initialize_simulator_text_size(UUID)
                    self.assertEqual(run.call_count, 3)

    def test_failed_baseline_configuration_is_not_success(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=[self.result(), subprocess.CalledProcessError(1, ["xcrun"])]) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                initialize_simulator_text_size(UUID)
            self.assertEqual(run.call_count, 2)

    def test_set_readback_and_restore(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=self.lifecycle()) as run:
            with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE) as category:
                self.assertEqual(category, MAXIMUM_TEXT_SIZE)
            prefix = ["xcrun", "simctl", "ui", UUID, "content_size"]
            self.assertEqual(run.call_args_list, [
                call(prefix, check=True, capture_output=True, text=True),
                call(prefix + [MAXIMUM_TEXT_SIZE], check=True, capture_output=True, text=True),
                call(prefix, check=True, capture_output=True, text=True),
                call(prefix + ["large"], check=True, capture_output=True, text=True),
                call(prefix, check=True, capture_output=True, text=True),
            ])

    def test_restore_preserves_xcode_failure(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=self.lifecycle()) as run:
            with self.assertRaisesRegex(RuntimeError, "Xcode failed"):
                with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE):
                    raise RuntimeError("Xcode failed")
            self.assertEqual(run.call_count, 5)
            self.assertEqual(run.call_args_list[-2].args[0][-1], "large")

    def test_reject_unrestorable_original_without_mutation(self):
        for original in ("", "unknown", "unsupported"):
            with self.subTest(original=original):
                with patch("ios_simulator_text_size.subprocess.run",
                           return_value=self.result(original)) as run:
                    with self.assertRaisesRegex(RuntimeError, "可恢复"):
                        with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE):
                            self.fail("Unverified configuration must not run XCTest")
                    self.assertEqual(run.call_count, 1)

    def test_mismatched_readback_fails_before_xcode_and_restores(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=self.lifecycle(selected="large")) as run:
            with self.assertRaisesRegex(RuntimeError, "核验失败"):
                with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE):
                    self.fail("Mismatched configuration must not run XCTest")
            self.assertEqual(run.call_count, 5)
            self.assertEqual(run.call_args_list[-2].args[0][-1], "large")

    def test_failed_configuration_restores(self):
        failure = subprocess.CalledProcessError(1, ["xcrun"])
        responses = [self.result("large"), failure, self.result(), self.result("large")]
        with patch("ios_simulator_text_size.subprocess.run", side_effect=responses) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE):
                    self.fail("Failed configuration must not run XCTest")
            self.assertEqual(run.call_args_list[-2].args[0][-1], "large")

    def test_failed_restore_is_not_success(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=self.lifecycle(restored="medium")):
            with self.assertRaisesRegex(RuntimeError, "核验失败"):
                with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE):
                    pass

    def test_failed_initial_read_does_not_mutate(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=subprocess.CalledProcessError(1, ["xcrun"])) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE):
                    self.fail("Failed initial read must not run XCTest")
            self.assertEqual(run.call_count, 1)


if __name__ == "__main__":
    unittest.main()
