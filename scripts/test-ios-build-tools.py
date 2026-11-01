#!/usr/bin/env python3
"""Run build/archive and UI command contract regressions without starting Xcode."""
import ast
import base64
from contextlib import nullcontext, redirect_stdout
import hashlib
import http.client
import http.server
from io import StringIO
import json
import os
import pathlib
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sqlite3
import struct
import tempfile
import threading
import time
import unittest
import uuid
from unittest.mock import Mock, call, patch
from types import SimpleNamespace
import urllib.error
import urllib.request

from ios_simulator_text_size import (
    _RUNTIME_RETRY_ATTEMPTS, MAXIMUM_TEXT_SIZE, initialize_simulator_text_size,
    runtime_text_size_control, set_simulator_text_size, simulator_text_size,
    simulator_appearance,
)


ROOT = Path(__file__).resolve().parent.parent
UUID = "12345678-1234-1234-1234-123456789abc"


def process_alive(pid):
    """Report whether a process id is still running."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def fixture_helpers():
    # Load only definitions/imports, never the fixture's daemon/Xcode entrypoint.
    source = ROOT / "scripts/ios_ui_fixture.py"
    tree = ast.parse(source.read_text(), filename=str(source))
    namespace = {}
    exec(compile(ast.Module(body=[node for node in tree.body
        if isinstance(node, (ast.Import, ast.ImportFrom, ast.FunctionDef, ast.ClassDef))],
        type_ignores=[]), str(source), "exec"), namespace)
    return namespace


def mocked_subprocess(run):
    """A subprocess stand-in that answers both run() and Popen().

    The UI sweep starts xcodebuild through Popen so a silent stall can be
    killed. The harness only needs the child's exit code, so Popen returns a
    process-like object whose wait() reports the same code run() would.
    """
    def popen(command, env=None, **keywords):
        completed = run(command, env=env, **keywords)
        return Mock(wait=Mock(return_value=completed.returncode))

    return Mock(run=run, Popen=popen)


def ui_case_sweep_statements(tree):
    """The per-case sweep: run_ui_case plus its bounded-retry driver."""
    driver = next(node for node in ast.walk(tree)
                  if isinstance(node, ast.For)
                  and isinstance(node.target, ast.Tuple)
                  and any(isinstance(target, ast.Name) and target.id == "case_bundle"
                          for target in node.target.elts))
    driver_owner = None
    for node in ast.walk(tree):
        body = getattr(node, "body", None)
        if isinstance(body, list) and driver in body:
            driver_owner = node
            break
    if driver_owner is None:
        raise AssertionError("The UI sweep driver must live in a statement block")
    # The driver may sit inside a try block whose finally block carries the
    # sweep's reporting. The rebuild below keeps only the driver, so any
    # other shape must fail loudly instead of dropping statements silently.
    wrapper = driver_owner if isinstance(driver_owner, ast.Try) else None
    if wrapper is not None:
        if len(wrapper.body) != 1 or wrapper.body[0] is not driver:
            raise AssertionError(
                "The sweep's try body must hold only the case loop; extra "
                "statements would be dropped by the extracted code")
        if wrapper.handlers or wrapper.orelse:
            raise AssertionError(
                "The sweep's try must have no except/else clauses; they "
                "would be dropped by the extracted code")
        owner = next((node for node in ast.walk(tree)
                      if isinstance(getattr(node, "body", None), list)
                      and wrapper in node.body), None)
        if owner is None:
            raise AssertionError("The sweep's try wrapper must live in a statement block")
        scope = owner.body
    else:
        scope = driver_owner.body
    # The helpers and the case body live next to the driver, in the
    # enclosing block. Every helper the sweep calls must travel with it,
    # or the extracted code references names that were never defined.
    helpers = [statement for statement in scope
               if isinstance(statement, ast.FunctionDef)
               and statement.name != "run_ui_case"]
    run_case = next((statement for statement in scope
                     if isinstance(statement, ast.FunctionDef)
                     and statement.name == "run_ui_case"), None)
    if run_case is None:
        raise AssertionError("run_ui_case must be defined next to the sweep driver")
    # The driver's own setup statements travel with it too.
    setup = [statement for statement in scope
             if isinstance(statement, ast.Assign)
             and any(isinstance(target, ast.Name)
                     and target.id in {"max_attempts", "ui_retries", "ui_telemetry"}
                     for target in statement.targets)]
    selected = helpers + [run_case] + setup
    if wrapper is None:
        selected += scope[scope.index(driver):scope.index(driver) + 1]
        return selected
    # Rebuild the try/finally around the driver so the reporting still
    # runs when a case fails.
    try_statement = ast.Try(body=[driver], handlers=[], orelse=[],
                            finalbody=wrapper.finalbody)
    ast.fix_missing_locations(try_statement)
    return selected + [try_statement]


class AppMetadataContracts(unittest.TestCase):
    def test_app_declares_only_exempt_encryption(self):
        info = plistlib.loads((ROOT / "apps/ios/Info.plist").read_bytes())
        self.assertIs(info["ITSAppUsesNonExemptEncryption"], False)


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


class UIFixtureSetupContracts(unittest.TestCase):
    def run_loss_scenarios(self, invalid_receipts=None):
        # Execute the real per-case loop; only external XCTest/services are mocked.
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        sweep = ui_case_sweep_statements(tree)
        release = threading.Event()
        release.set()  # A preceding workflow may already have released its response.
        receipts = []
        methods = ["testLostResponseAndProcessRecovery", "testDirectAgentShareWorkflow"]

        def run(command, env=None, **keywords):
            if command[0] != "xcodebuild":
                # A simulator probe, not a test run.
                return Mock(returncode=1, stdout="")
            method = next(argument.rsplit("/", 1)[-1] for argument in command
                          if argument.startswith("-only-testing:"))
            self.assertFalse(release.is_set(), method + " must begin with response loss armed")
            request = method + "-request"
            message = method + "-message"
            current = [(request, {"message_id": message, "disposition": "accepted"}),
                       (request, {"message_id": message, "disposition": "duplicate"})]
            if invalid_receipts is not None and method == methods[-1]:
                current = invalid_receipts(current)
            receipts.extend(current)
            release.set()  # The real UI explicitly releases before retrying.
            return Mock(returncode=0)

        namespace = dict(fixture_helpers(),
            cases=[(method, "large") for method in methods],
            bundles=[Path(method + ".xcresult") for method in methods],
            release_lost_response=release, lost_receipts=receipts,
            lost_lock=threading.Lock(), lost_response_acceptance=True,
            simulator=UUID, repo=str(ROOT), destination="fixture", derived="fixture",
            test_env={}, local=Mock(return_value={"ticket": "fixture"}),
            simulator_text_size=lambda *args: nullcontext(),
            simulator_appearance=lambda *args: nullcontext(),
            runtime_text_size_control=lambda *args: nullcontext(("fixture", "fixture")),
            subprocess=mocked_subprocess(run), fixture_evidence=None,
        )
        with patch("builtins.print") as output:
            exec(compile(ast.Module(body=sweep, type_ignores=[]), str(source), "exec"), namespace)
        return receipts, output.call_args_list

    def test_loss_scenarios_rearm_and_verify_independent_requests(self):
        receipts, output = self.run_loss_scenarios()
        self.assertEqual(len(receipts), 4, "Keep both workflows' receipt evidence")
        self.assertEqual(len({request for request, _ in receipts}), 2)
        # The sweep also prints its telemetry; count only the acceptance lines.
        acceptance = [call for call in output
                      if "Lost-response acceptance" in str(call)]
        self.assertEqual(len(acceptance), 2, "Verify App and Share independently")

    def test_flaky_ui_case_retries_once_with_clean_state(self):
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        sweep = ui_case_sweep_statements(tree)
        method = "testAuthenticatedNativeWorkflow"

        def scenario(failures, environment=None):
            runs, commands = [], []
            directory = tempfile.TemporaryDirectory(prefix="holon-ios-ui-retry-")
            self.addCleanup(directory.cleanup)
            bundle = Path(directory.name) / "case.xcresult"

            def run(command, env=None, **keywords):
                commands.append(list(command))
                if command[0] == "xcodebuild":
                    runs.append(command)
                    bundle.mkdir(exist_ok=True)  # xcodebuild materializes the bundle
                    return Mock(returncode=65 if len(runs) <= failures else 0)
                if "groups" in command:
                    # The App Group container holds the persisted sharing
                    # consent; a retry must clear it before the next attempt.
                    return Mock(returncode=0, stdout="")
                return Mock(returncode=0)  # simctl bootstatus health check

            namespace = dict(fixture_helpers(),
                cases=[(method, "large")], bundles=[bundle],
                release_lost_response=threading.Event(), lost_receipts=[],
                lost_lock=threading.Lock(), lost_response_acceptance=False,
                simulator=UUID, repo=str(ROOT), destination="fixture", derived="fixture",
                test_env={}, local=Mock(return_value={"ticket": "fixture"}),
                fixture_evidence=None,
                simulator_text_size=lambda *args: nullcontext(),
                simulator_appearance=lambda *args: nullcontext(),
                runtime_text_size_control=lambda *args: nullcontext(("fixture", "fixture")),
                subprocess=mocked_subprocess(run))
            with patch.dict(os.environ, environment or {}, clear=True), \
                    patch("builtins.print") as output:
                error = None
                try:
                    exec(compile(ast.Module(body=sweep, type_ignores=[]),
                                 str(source), "exec"), namespace)
                except RuntimeError as failure:
                    error = failure
            return runs, commands, bundle, error, output.call_args_list

        runs, commands, bundle, error, output = scenario(failures=1)
        self.assertIsNone(error)
        self.assertEqual(len(runs), 2, "A flaky attempt retries exactly once")
        self.assertTrue(any("bootstatus" in command and UUID in command
                            for command in commands), "Retry restores simulator boot state")
        self.assertTrue(any("groups" in command and UUID in command
                            for command in commands),
                        "Retry clears the persisted sharing consent")
        self.assertTrue((bundle.parent / "case-attempt1-failed.xcresult").exists(),
                        "Keep the failed attempt's diagnostics")
        self.assertTrue(bundle.exists(), "The passing attempt owns the canonical bundle")
        messages = " ".join(str(call) for call in output)
        self.assertIn("::warning title=iOS UI retry::", messages)
        self.assertIn(method, messages)

        runs, commands, bundle, error, output = scenario(failures=2)
        self.assertIsInstance(error, RuntimeError)
        self.assertIn("UI case " + method, str(error))
        self.assertEqual(len(runs), 2, "Two real failures still fail the job")

        runs, commands, bundle, error, output = scenario(
            failures=2, environment={"IOS_UI_RETRY": "0"})
        self.assertIsInstance(error, RuntimeError)
        self.assertEqual(len(runs), 1, "IOS_UI_RETRY=0 keeps fail-fast semantics")

    def test_ui_wait_scale_forwarded_to_test_runner(self):
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        for node in ast.walk(tree):
            body = getattr(node, "body", None)
            if not isinstance(body, list):
                continue
            start = next((index for index, statement in enumerate(body)
                          if isinstance(statement, ast.Assign)
                          and any(isinstance(target, ast.Name) and target.id == "test_env"
                                  for target in statement.targets)), None)
            if start is None:
                continue
            stop = next(index for index in range(start + 1, len(body))
                        if isinstance(body[index], ast.Expr)
                        and isinstance(body[index].value, ast.Call)
                        and isinstance(body[index].value.func, ast.Attribute)
                        and body[index].value.func.attr == "update")
            wait_scale = ast.Module(body=body[start:stop], type_ignores=[])
            break
        else:
            self.fail("UI fixture wait-scale block not found")

        def forwarded(environment):
            namespace = dict(os=os)
            with patch.dict(os.environ, environment, clear=True), patch("builtins.print"):
                exec(compile(wait_scale, str(source), "exec"), namespace)
            return namespace["test_env"]

        self.assertEqual(forwarded({})["TEST_RUNNER_HOLON_UI_WAIT_SCALE"], "1")
        self.assertEqual(forwarded({"HOLON_UI_WAIT_SCALE": " 2.5 "})["TEST_RUNNER_HOLON_UI_WAIT_SCALE"], "2.5")
        with self.assertRaisesRegex(RuntimeError, "HOLON_UI_WAIT_SCALE"):
            forwarded({"HOLON_UI_WAIT_SCALE": "fast"})
        with self.assertRaisesRegex(RuntimeError, "HOLON_UI_WAIT_SCALE"):
            forwarded({"HOLON_UI_WAIT_SCALE": "9"})

    def test_retry_clears_persisted_sharing_consent(self):
        source = ROOT / "scripts/ios_ui_fixture.py"
        text = source.read_text()
        tree = ast.parse(text, filename=str(source))
        helper = next(node for node in ast.walk(tree)
                      if isinstance(node, ast.FunctionDef)
                      and node.name == "reset_app_group_consent")
        namespace = {"pathlib": pathlib, "subprocess": Mock()}
        # Rebuild the helper as a real function so its `return` statements
        # keep their meaning.
        module = ast.Module(body=[ast.FunctionDef(
            name=helper.name, args=helper.args, body=helper.body,
            decorator_list=[], returns=None, type_comment=None,
            type_params=[])], type_ignores=[])
        ast.fix_missing_locations(module)
        with patch("builtins.print"):
            exec(compile(module, str(source), "exec"), namespace)
        reset = namespace["reset_app_group_consent"]

        with tempfile.TemporaryDirectory(prefix="holon-ios-ui-group-") as directory:
            group = Path(directory)
            preferences = group / "Library" / "Preferences"
            preferences.mkdir(parents=True)
            stale = preferences / "group.run.holon.ios.plist"
            stale.write_bytes(b"stale consent")

            def container(*args, **kwargs):
                return subprocess.CompletedProcess(
                    args, 0, stdout=f"group.run.holon.ios\t{group}\n", stderr="")

            namespace["subprocess"].run = container
            reset("device", "run.holon.ios", "group.run.holon.ios")
            self.assertFalse(stale.exists(),
                             "The App Group consent must be cleared so a retry re-runs the gate")

            # A second attempt must tolerate an already-clean group.
            reset("device", "run.holon.ios", "group.run.holon.ios")
            self.assertFalse(stale.exists())

            # An uninstalled app has no group container; that is not an error.
            def missing(*args, **kwargs):
                return subprocess.CompletedProcess(args, 1, stdout="", stderr="missing")

            namespace["subprocess"].run = missing
            reset("device", "run.holon.ios", "group.run.holon.ios")

        # The retry driver must reset consent before every retried attempt.
        self.assertIn("reset_app_group_consent(", text)
        driver = text[text.index("max_attempts = 1 if os.environ.get"):]
        driver = driver[:driver.index("started = time.monotonic()")]
        self.assertIn("reset_app_group_consent(", driver,
                      "Every retried attempt must reset the persisted consent")

    def test_ui_sweep_records_per_case_telemetry(self):
        """A failing case must be nameable without reading the xcodebuild log."""
        source = ROOT / "scripts/ios_ui_fixture.py"
        text = source.read_text()
        self.assertIn("ui-sweep-telemetry.json", text,
                      "The sweep must write a machine-readable outcome record")
        self.assertIn('"outcome": "passed"', text)
        self.assertIn('"outcome": "failed"', text)
        self.assertIn("UI sweep telemetry:", text,
                      "The CI log must summarize the sweep outcome")
        tree = ast.parse(text, filename=str(source))
        sweep = ui_case_sweep_statements(tree)
        module = ast.Module(body=sweep, type_ignores=[])
        ast.fix_missing_locations(module)
        namespace = dict(fixture_helpers(),
            cases=[("testAuthenticatedNativeWorkflow", "large")],
            bundles=[Path("/tmp/holon-ui-telemetry-case.xcresult")],
            release_lost_response=threading.Event(), lost_receipts=[],
            lost_lock=threading.Lock(), lost_response_acceptance=False,
            simulator=UUID, repo=str(ROOT), destination="fixture", derived="fixture",
            test_env={"TEST_RUNNER_HOLON_UI_WAIT_SCALE": "2"}, local=Mock(return_value={"ticket": "fixture"}),
            fixture_evidence=None,
            simulator_text_size=lambda *args: nullcontext(),
            simulator_appearance=lambda *args: nullcontext(),
            runtime_text_size_control=lambda *args: nullcontext(("fixture", "fixture")),
            subprocess=mocked_subprocess(lambda *args, **keywords: Mock(returncode=0, stdout="")))
        with patch.dict(os.environ, {"IOS_UI_RETRY": "0"}, clear=True), \
                patch("builtins.print") as output:
            try:
                exec(compile(module, str(source), "exec"), namespace)
            except RuntimeError:
                pass  # The mocked case fails; the telemetry must still be recorded.
        printed = "\n".join(str(call) for call in output.call_args_list)
        self.assertIn("UI sweep telemetry:", printed)
        self.assertIn("testAuthenticatedNativeWorkflow", printed)

    def test_app_side_reset_failure_reaches_the_host(self):
        """A reset that failed in the app process must still fail the case.

        The app runs in its own process, so an assertion there cannot fail
        the test. The failure is published in the App Group and read by the
        host, which turns a dirty-state run into a red case instead of a
        false pass.
        """
        source = ROOT / "scripts/ios_ui_fixture.py"
        text = source.read_text()
        self.assertIn("app_state_reset_failure", text,
                      "The host must read the app's published reset failure")
        self.assertIn("testing.state.reset.failure", text,
                      "The published failure must use one agreed key")
        app = (ROOT / "apps/ios/Sources/TestingStateReset.swift").read_text()
        self.assertIn("testing.state.reset.failure", app,
                      "The app must publish the failure under the agreed key")
        self.assertIn("publish(failures)", app)

        # The failure must be a case failure, so the bounded retry applies.
        tree = ast.parse(text, filename=str(source))
        run_case = next(node for node in ast.walk(tree)
                        if isinstance(node, ast.FunctionDef) and node.name == "run_ui_case")
        self.assertTrue(any("app_state_reset_failure" in ast.dump(statement)
                            for statement in ast.walk(run_case)),
                        "The case must check the published reset failure")

    def test_app_side_state_reset_uses_one_launch_argument(self):
        """The app and the UI test must ask for the reset the same way."""
        app_reset = (ROOT / "apps/ios/Sources/TestingStateReset.swift").read_text()
        test_reset = (ROOT / "apps/ios/UITests/UIWaitScale.swift").read_text()
        arguments = set(re.findall(r'"(-Holon[A-Za-z]*UITests)"', app_reset))
        self.assertEqual(len(arguments), 1,
                         "The app must name exactly one reset launch argument")
        argument = arguments.pop()
        self.assertIn(f'"{argument}"', test_reset,
                      "The UI test must request the reset the app implements")
        # The app must actually act on the argument, not merely declare it.
        self.assertIn("CommandLine.arguments.contains(launchArgument)", app_reset)
        self.assertIn("resetIfRequested()", app_reset)
        # The app owns the reset, so it must clear the App Group consent too.
        self.assertIn("revokeAll()", app_reset)

    def test_swift_ui_waits_all_use_the_scaled_budget(self):
        """Every UI wait budget must be stretched by the same factor.

        The fixture forwards one factor; a Swift wait that bypasses it would
        keep the unscaled budget and still fail on a loaded CI runner.
        """
        sources = sorted((ROOT / "apps/ios/UITests").glob("*.swift"))
        self.assertTrue(sources, "UI test sources must exist")
        raw_waits = []
        for source in sources:
            for number, line in enumerate(source.read_text().splitlines(), start=1):
                if "UIWaitScale" in line:
                    continue
                for match in re.finditer(
                        r"waitForExistence\(timeout: ([0-9]+)\)"
                        r"|XCTWaiter\.(?:wait|fulfillment)\([^)]*?timeout: ([0-9]+)\)", line):
                    raw_waits.append(f"{source.name}:{number}: {line.strip()}")
        self.assertEqual(raw_waits, [],
                         "Every UI wait must use the scaled budget: " + "; ".join(raw_waits))

        scale = (ROOT / "apps/ios/UITests/UIWaitScale.swift").read_text()
        self.assertIn("HOLON_UI_WAIT_SCALE", scale,
                      "The Swift side must read the factor the fixture forwards")
        self.assertIn("timeout * factor", scale,
                      "The factor must multiply the wait budget")

    def test_automatic_read_retry_pins_marker_baseline(self):
        source = ROOT / "scripts/ios_ui_fixture.py"
        text = source.read_text()
        # The confirming query must be bounded by the per-attempt baseline.
        self.assertIn("AND brief.created_event_seq > ?", text)
        tree = ast.parse(text, filename=str(source))
        run_case = next(node for node in ast.walk(tree)
                        if isinstance(node, ast.FunctionDef) and node.name == "run_ui_case")
        branch = next(statement for statement in run_case.body
                      if isinstance(statement, ast.If)
                      and isinstance(statement.test, ast.Compare)
                      and getattr(statement.test.comparators[0], "value", None)
                          == "testVisibleBriefAutomaticallyUpdatesHomeUnreadCount")
        with tempfile.TemporaryDirectory(prefix="holon-ios-ui-baseline-") as directory:
            state = Path(directory) / "holon" / "state"
            state.mkdir(parents=True)
            connection = sqlite3.connect(state / "runtime.sqlite")
            connection.execute("CREATE TABLE briefs (agent_id TEXT, created_event_seq INTEGER, preview TEXT)")
            connection.executemany("INSERT INTO briefs VALUES (?, ?, ?)", [
                ("holon-tester", 4, "IOS_AUTOMATIC_READ_FOLLOWUP_BRIEF attempt one"),
                ("holon-tester", 7, "IOS_AUTOMATIC_READ_FOLLOWUP_BRIEF attempt two"),
                ("holon-tester", 9, "unrelated preview"),
                ("other-agent", 12, "IOS_AUTOMATIC_READ_FOLLOWUP_BRIEF other agent")])
            connection.commit()
            connection.close()
            seeds = Mock()
            namespace = dict(sqlite3=sqlite3, root=Path(directory), agent="holon-tester",
                             automatic_read_seed_active=threading.Event(),
                             history_seed_active=threading.Event(),
                             seed_fixture_history=seeds, local=Mock())
            with patch("builtins.print"):
                exec(compile(ast.Module(body=branch.body, type_ignores=[]), str(source), "exec"), namespace)
            self.assertEqual(namespace["automatic_read_baseline_seq"], 7,
                             "Baseline is the newest followup marker before this attempt")
            seeds.assert_called_once()

    def test_loss_scenario_rejects_invalid_retry_receipts(self):
        invalid = {
            "missing retry": lambda rows: rows[:1],
            "changed UUID": lambda rows: [rows[0], ("different", rows[1][1])],
            "duplicate message": lambda rows: [rows[0], (rows[1][0], {
                "message_id": "different", "disposition": "duplicate"})],
            "accepted twice": lambda rows: [rows[0], (rows[1][0], {
                "message_id": rows[0][1]["message_id"], "disposition": "accepted"})],
        }
        for label, transform in invalid.items():
            with self.subTest(label=label), self.assertRaises(RuntimeError):
                self.run_loss_scenarios(transform)

    def run_setup(self, initialize, install, share=True, report=False, commands=None):
        # Execute the fixture's real setup block without starting its daemon/XCTest.
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        for node in ast.walk(tree):
            body = getattr(node, "body", None)
            if not isinstance(body, list):
                continue
            start = next((index for index, statement in enumerate(body)
                          if isinstance(statement, ast.Assign)
                          and any(isinstance(target, ast.Name) and target.id == "cases"
                                  for target in statement.targets)), None)
            if start is not None:
                # The setup ends where the sweep begins: the driver loop, or
                # the try block that wraps it.
                stop = next(index for index in range(start, len(body))
                            if isinstance(body[index], (ast.For, ast.Try)))
                setup = ast.Module(body=body[start:stop], type_ignores=[])
                break
        else:
            self.fail("UI fixture setup block not found")
        with tempfile.TemporaryDirectory(prefix="holon-ios-ui-setup-") as directory:
            namespace = dict(os=os, pathlib=pathlib, simulator=UUID, repo=str(ROOT),
                             root=Path(directory), bundle=Path(directory) / "UI.xcresult",
                             MAXIMUM_TEXT_SIZE=MAXIMUM_TEXT_SIZE, rich_acceptance=report,
                             report_acceptance=report,
                             lost_response_acceptance=False, history_acceptance=False,
                             task_result_acceptance=False,
                             share_acceptance=share, initialize_simulator_text_size=initialize,
                             subprocess=commands if commands is not None else Mock(),
                             time=Mock())
            with patch.dict(os.environ, {}, clear=True), patch("ios_share_probe.install", install):
                exec(compile(setup, str(source), "exec"), namespace)
            return namespace["cases"]

    def test_report_acceptance_selects_real_report_cases(self):
        initialize, install = Mock(), Mock()
        cases = self.run_setup(initialize, install, share=False, report=True)
        methods = {method for method, _ in cases}
        self.assertTrue({
            "testContentReportFullResponseIncludesTail",
            "testContentReportConfirmationCancelDoesNotPersist",
            "testContentReportInvalidExplanationCannotShowAccepted",
            "testContentReportAcceptedReceiptCannotSubmitTwice",
        }.issubset(methods))
        initialize.assert_called_once_with(UUID)
        install.assert_not_called()

    def test_cold_simulator_is_initialized_before_share_install(self):
        events = []

        def initialize(simulator):
            self.assertEqual(simulator, UUID)
            events.append("ready")

        def install(simulator, repo, directory):
            self.assertEqual(events, ["ready"], "Cannot install on a Shutdown simulator")
            self.assertEqual(simulator, UUID)
            self.assertEqual(repo, str(ROOT))
            self.assertTrue(directory.is_dir())
            events.append("installed")

        self.run_setup(initialize, install)
        self.assertEqual(events, ["ready", "installed"])

    def test_stale_app_is_uninstalled_before_share_install(self):
        events = []

        def initialize(simulator):
            events.append("ready")

        def install(simulator, repo, directory):
            events.append("installed")

        commands = Mock()
        commands.run.side_effect = lambda *arguments, **keywords: (
            events.append(tuple(arguments[0])), Mock(returncode=1))[1]
        self.run_setup(initialize, install, commands=commands)
        self.assertEqual(events, [
            "ready",
            ("xcrun", "simctl", "get_app_container", UUID, "run.holon.ios", "groups"),
            ("xcrun", "simctl", "get_app_container", UUID, "run.holon.ios", "data"),
            ("xcrun", "simctl", "uninstall", UUID, "run.holon.ios"),
            ("xcrun", "simctl", "get_app_container", UUID, "run.holon.ios", "data"),
            "installed",
        ], "A reused simulator must not restore stale app data into any case")

    def test_sweep_resets_consent_before_uninstalling_the_app(self):
        """The group container is only reachable while the app is installed.

        The consent lives in the App Group container, which outlives the app
        install. Resetting after the uninstall would find no container and
        leave the previous run's approval in place.
        """
        source = ROOT / "scripts/ios_ui_fixture.py"
        text = source.read_text()
        tree = ast.parse(text)
        def call_sites(tree, function_name, literal=None):
            """Line of every call to `function_name`, optionally requiring
            `literal` among the call's constant arguments. Anchoring on AST
            call nodes keeps comments and the definition out of the sites."""
            sites = []
            for node in ast.walk(tree):
                if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                        and node.func.id == function_name):
                    continue
                if literal is None or any(isinstance(argument, ast.Constant)
                                          and argument.value == literal
                                          for argument in node.args):
                    sites.append(node.lineno)
            return sites

        reset_calls = call_sites(tree, "reset_app_group_consent", "run.holon.ios")
        uninstall_calls = call_sites(tree, "ensure_uninstalled", "run.holon.ios")
        self.assertTrue(reset_calls, "The sweep must reset the sharing consent")
        self.assertTrue(uninstall_calls, "The sweep must uninstall the app")
        self.assertLess(min(reset_calls), min(uninstall_calls),
                        "The consent reset must run while the app is still installed")
        # The browser case's own clean install needs the same order.
        browser = text[:text.index('if method == "testRuntimeBrowserWorkflow":')].count("\n") + 1
        browser_reset = [line for line in reset_calls if line > browser]
        browser_uninstall = [line for line in uninstall_calls if line > browser]
        self.assertTrue(browser_reset and browser_uninstall,
                        "The browser case must reset consent and uninstall the app")
        self.assertLess(min(browser_reset), min(browser_uninstall),
                        "The browser case must reset consent before its uninstall")

    def test_failed_initialization_prevents_share_install(self):
        install = Mock()
        initialize = Mock(side_effect=subprocess.CalledProcessError(149, ["xcrun"]))
        with self.assertRaises(subprocess.CalledProcessError):
            self.run_setup(initialize, install)
        initialize.assert_called_once_with(UUID)
        install.assert_not_called()

    def test_non_share_setup_still_initializes_without_install(self):
        initialize, install = Mock(), Mock()
        self.run_setup(initialize, install, share=False)
        initialize.assert_called_once_with(UUID)
        install.assert_not_called()

    def test_offline_reading_and_sign_out_are_default_cases_before_network_deletion(self):
        cases = self.run_setup(Mock(), Mock(), share=False)
        methods = [method for method, _ in cases]
        cold = methods.index("testOfflineReadingCacheColdLaunchAndRetry")
        automatic = methods.index("testVisibleBriefAutomaticallyUpdatesHomeUnreadCount")
        sign_out = methods.index("testOfflineReadingCacheSignOutBoundary")
        self.assertLess(methods.index("testAuthenticatedNativeWorkflow"), cold)
        self.assertLess(cold, automatic)
        self.assertLess(automatic, sign_out)
        self.assertLess(cold, sign_out)
        self.assertLess(sign_out, methods.index("testNetworkManagementWorkflow"))
        self.assertEqual(cases[cold][1], "large")
        self.assertEqual(cases[sign_out][1], "large")

    def test_real_offline_case_loop_seeds_after_onboarding_and_exports_real_brief_oracle(self):
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        sweep = ui_case_sweep_statements(tree)
        methods = ["testAuthenticatedNativeWorkflow", "testOfflineReadingCacheColdLaunchAndRetry",
                   "testOfflineReadingCacheSignOutBoundary"]
        turns = [{"turn_id": "initial-turn", "key": {"turn_index": 0}, "brief_ids": ["initial-brief"]}]
        seed_active = threading.Event()
        tickets, runs, seeded = [], [], []

        def local(method, path, payload=None):
            if path == "/auth/pairing/issue":
                self.assertFalse(seed_active.is_set(), "Issue the single-use ticket after history preparation")
                tickets.append(str(len(tickets)))
                return {"ticket": tickets[-1]}
            if method == "POST" and path.endswith("/enqueue"):
                self.assertTrue(seed_active.is_set())
                seeded.append(payload["text"])
                index = len(turns)
                turns.append({"turn_id": "turn-" + str(index), "key": {"turn_index": index},
                              "brief_ids": ["brief-" + str(index)], "inputs": [payload]})
                return {}
            if path.endswith("/conversation") or path.endswith("/conversation?limit=60"):
                return {"turns": list(reversed(turns))}
            if "/briefs/" in path:
                self.assertTrue(any(path.endswith("/briefs/" + brief)
                                    for turn in turns for brief in turn["brief_ids"]))
                return {"text": "# Real result\n\nActual fixture body cache oracle."}
            self.fail("Unexpected fixture call: " + path)

        def run(command, env=None, **keywords):
            if command[0] != "xcodebuild":
                # A simulator probe, not a test run.
                return Mock(returncode=1, stdout="")
            method = next(argument.rsplit("/", 1)[-1] for argument in command
                          if argument.startswith("-only-testing:"))
            runs.append(method)
            self.assertEqual(env["TEST_RUNNER_HOLON_UI_PAIRING_CODE"], tickets[-1])
            if method == methods[0]:
                self.assertEqual(seeded, [], "Do not evict the earlier onboarding smoke's original latest window")
            else:
                self.assertEqual(seeded, [f"IOS_OFFLINE_HISTORY_{number:03}" for number in range(35)])
                self.assertEqual(env["TEST_RUNNER_HOLON_UI_HISTORY_OLDER_TOP"], "turn-1")
                self.assertEqual(env["TEST_RUNNER_HOLON_UI_OFFLINE_BODY_MARKER"], "Actual fixture body cache oracle.")
            return Mock(returncode=0)

        namespace = dict(fixture_helpers(), cases=[(method, "large") for method in methods],
            bundles=[Path(method + ".xcresult") for method in methods], history_acceptance=False,
            history_seed_active=seed_active, agent="holon-tester", local=local,
            lost_response_acceptance=False, fixture_evidence=None,
            simulator=UUID, repo=str(ROOT), destination="fixture", derived="fixture", test_env={},
            simulator_text_size=lambda *args: nullcontext(),
            simulator_appearance=lambda *args: nullcontext(),
            runtime_text_size_control=lambda *args: nullcontext(("fixture", "fixture")),
            subprocess=mocked_subprocess(run))
        exec(compile(ast.Module(body=sweep, type_ignores=[]), str(source), "exec"), namespace)
        self.assertEqual(runs, methods)
        self.assertEqual(len(tickets), 3, "Each independently selectable authenticated case gets a fresh ticket")
        self.assertFalse(seed_active.is_set())

    def test_offline_history_oracle_survives_preceding_brief_less_turns(self):
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        seed = next(node for node in ast.walk(tree)
                    if isinstance(node, ast.If)
                    and any(isinstance(value, ast.Constant)
                            and value.value == "testOfflineReadingCacheColdLaunchAndRetry"
                            for value in ast.walk(node.test))
                    and any(isinstance(value, ast.Call) and isinstance(value.func, ast.Name)
                            and value.func.id == "seed_fixture_history"
                            for statement in node.body for value in ast.walk(statement)))
        oracle = next(node for node in ast.walk(tree)
                      if isinstance(node, ast.If)
                      and any(isinstance(value, ast.Constant)
                              and value.value == "testConversationHistoryWindowPosition"
                              for value in ast.walk(node.test)))
        code = compile(ast.Module(body=[seed, oracle], type_ignores=[]), str(source), "exec")
        for history_acceptance in (False, True):
            with self.subTest(history_acceptance=history_acceptance):
                # Prior cases can leave cancelled/control turns with no brief.
                turns = [{"turn_id": f"turn-{index}", "key": {"turn_index": index},
                          "brief_ids": [], "inputs": []} for index in range(70)]
                seeded = []
                active = threading.Event()

                def local(method, path, payload=None):
                    if method == "POST" and path.endswith("/enqueue"):
                        self.assertTrue(active.is_set())
                        seeded.append(payload["text"])
                        index = len(turns)
                        turns.append({"turn_id": f"turn-{index}", "key": {"turn_index": index},
                                      "brief_ids": [f"brief-{index}"], "inputs": [payload]})
                        return {}
                    if path.endswith("/conversation") or path.endswith("/conversation?limit=60"):
                        limit = 60 if path.endswith("?limit=60") else 30
                        return {"turns": list(reversed(turns[-limit:]))}
                    if "/briefs/" in path:
                        return {"text": "# Real result\n\nActual fixture body cache oracle."}
                    self.fail("Unexpected fixture call: " + path)

                environment = {}
                namespace = dict(fixture_helpers(), local=local, agent="holon-tester",
                                 history_seed_active=active, history_acceptance=history_acceptance,
                                 method="testOfflineReadingCacheColdLaunchAndRetry", test_env=environment)
                exec(code, namespace)
                self.assertEqual(seeded, [f"IOS_OFFLINE_HISTORY_{number:03}" for number in range(35)])
                self.assertEqual(environment["TEST_RUNNER_HOLON_UI_HISTORY_OLDER_TOP"], "turn-70")
                self.assertEqual(environment["TEST_RUNNER_HOLON_UI_HISTORY_NEWER_TOP"], "turn-85")
                self.assertEqual(environment["TEST_RUNNER_HOLON_UI_OFFLINE_BODY_MARKER"],
                                 "Actual fixture body cache oracle.")
                self.assertFalse(active.is_set())

    def test_offline_control_is_exported_only_to_runner_with_proxy_endpoint(self):
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        block = next(node for node in ast.walk(tree)
                     if isinstance(node, ast.If) and isinstance(node.test, ast.Name)
                     and node.test.id == "proxy"
                     and any(isinstance(child, ast.Name) and child.id == "runner_inputs"
                             for child in ast.walk(node)))
        export = next(node for node in ast.walk(tree)
                      if isinstance(node, ast.For) and isinstance(node.target, ast.Tuple)
                      and any(isinstance(child, ast.Constant)
                              and child.value == "TEST_RUNNER_HOLON_UI_" for child in ast.walk(node)))
        namespace = dict(proxy=Mock(server_port=12345), runner_inputs={},
                         proxy_control_token="fixture-runner-only-control",
                         lost_response_acceptance=False, test_env={})
        exec(compile(ast.Module(body=[block, export], type_ignores=[]), str(source), "exec"), namespace)
        self.assertEqual(namespace["test_env"], {
            "TEST_RUNNER_HOLON_UI_ENDPOINT": "http://127.0.0.1:12345/api",
            "TEST_RUNNER_HOLON_UI_NODE_CONTROL_URL": "http://127.0.0.1:12345/fixture/node-availability",
            "TEST_RUNNER_HOLON_UI_NODE_CONTROL_TOKEN": "fixture-runner-only-control",
        })


class UIFixtureStallDetectionContracts(unittest.TestCase):
    """A silent xcodebuild stall must fail fast and say so.

    A macOS runner can hang inside SwiftDriver without emitting any output.
    The sweep used to wait for the job's timeout-minutes backstop, so the
    log ended mid-compile with no explanation.
    """

    def setUp(self):
        self.helpers = fixture_helpers()
        source = ROOT / "scripts/ios_ui_fixture.py"
        self.source = str(source)
        self.text = source.read_text()

    class SilentProcess:
        """A child that never writes and only dies when it is killed."""

        returncode = None

        def __init__(self, killed):
            self.killed = killed
            self.pid = None

        def poll(self):
            return 137 if self.killed else None

        def wait(self, timeout=None):
            # The watcher kills the child; the wait returns once it dies.
            # The deadline keeps a broken kill from hanging the suite: the
            # test must fail, not stall until the CI timeout.
            deadline = time.monotonic() + 30
            while not self.killed:
                if time.monotonic() > deadline:
                    raise AssertionError(
                        "the stalled child was never killed")
                if timeout is not None and time.monotonic() >= deadline - timeout:
                    raise subprocess.TimeoutExpired("xcodebuild", timeout)
                threading.Event().wait(0.05)
            return 137

        def kill(self):
            self.killed.append(True)

    def test_monitored_runner_kills_a_silent_child(self):
        """A child that never writes must be killed and reported."""
        helpers = self.helpers
        killed = []

        popen = Mock(return_value=self.SilentProcess(killed))
        module = Mock(Popen=popen, STDOUT=subprocess.STDOUT)
        with patch("builtins.print") as output:
            with self.assertRaises(helpers["UIStallTimeout"]):
                helpers["monitor_xcodebuild"](
                    ["xcodebuild", "build"], {}, 1, subprocess_module=module,
                    heartbeat_interval=1)
        self.assertTrue(killed, "A stalled xcodebuild must be killed")
        printed = "\n".join(str(call) for call in output.call_args_list)
        self.assertIn("iOS UI stall", printed)
        self.assertIn("no output", printed)

    def test_a_quiet_but_live_child_reports_a_heartbeat(self):
        """A child that is slow but alive must be visible in the log."""
        helpers = self.helpers

        class QuietProcess:
            returncode = None
            pid = None
            finished = False

            def wait(self, timeout=None):
                # Quiet but progressing: it returns before any stall is due.
                threading.Event().wait(1.2)
                self.finished = True
                return 0

            def poll(self):
                return 0 if self.finished else None

            def kill(self):
                raise AssertionError("a live child must not be killed")

        module = Mock(Popen=Mock(return_value=QuietProcess()),
                      STDOUT=subprocess.STDOUT)
        with patch("builtins.print") as output:
            helpers["monitor_xcodebuild"](
                ["xcodebuild", "build"], {}, 30, subprocess_module=module,
                heartbeat_interval=1)
        printed = "\n".join(str(call) for call in output.call_args_list)
        self.assertIn("iOS UI heartbeat", printed)
        self.assertIn("budget 30s", printed)

    def test_a_watcher_that_dies_fails_closed(self):
        """The watchdog must not leave an unbounded wait behind it."""
        helpers = self.helpers
        killed = []

        class BrokenWatcherProcess:
            returncode = None
            pid = None

            def __init__(self):
                self.killed = False

            def wait(self, timeout=None):
                # Mirror subprocess.Popen.wait(timeout=...): block until the
                # child exits or the timeout expires. The child only exits
                # when the watchdog kills it, so a wait that returns
                # without a kill means the watchdog failed to stop it.
                deadline = time.monotonic() + (20 if timeout is None else timeout)
                while not self.killed and time.monotonic() < deadline:
                    threading.Event().wait(0.01)
                if not self.killed:
                    raise subprocess.TimeoutExpired("xcodebuild", timeout or 20)
                return 137

            def poll(self):
                return 137 if self.killed else None

            def kill(self):
                self.killed = True
                # Record which thread stopped the child. The watchdog
                # must be the one that does it: the main thread also
                # stops a child whose watchdog died, so the outcome
                # alone would not prove the watchdog acted.
                killed.append(threading.current_thread().name)
                return None

        module = Mock(Popen=Mock(return_value=BrokenWatcherProcess()),
                      STDOUT=subprocess.STDOUT)
        # A failing growth probe breaks the watcher the way an unexpected
        # runtime error would.
        with patch("pathlib.Path.stat",
                   side_effect=RuntimeError("probe failed")):
            with self.assertRaises(helpers["UIStallTimeout"]):
                helpers["monitor_xcodebuild"](
                    ["xcodebuild", "build"], {}, 3600,
                    subprocess_module=module, heartbeat_interval=1)
        # The kill must come from the watchdog itself: the main thread
        # also stops a child whose watchdog died, so a passing assertion
        # on the outcome alone would not prove the watchdog killed it.
        self.assertTrue(killed, "A dead watcher must still kill the child")
        self.assertTrue(all(name != "MainThread" for name in killed),
                        "the watchdog must kill the child, not the main thread")

    def test_a_failing_report_does_not_break_the_watchdog(self):
        """A broken stdout must not disable the stall watchdog."""
        helpers = self.helpers
        killed = []

        class SilentProcess:
            returncode = None
            pid = None

            def poll(self):
                return 137 if killed else None

            def wait(self, timeout=None):
                deadline = time.monotonic() + 20
                while not killed and time.monotonic() < deadline:
                    if timeout is not None:
                        raise subprocess.TimeoutExpired("xcodebuild", timeout)
                    threading.Event().wait(0.05)
                return 137

            def kill(self):
                killed.append(True)

        module = Mock(Popen=Mock(return_value=SilentProcess()),
                      STDOUT=subprocess.STDOUT)
        # A print that raises is what a closing runner pipe looks like.
        with patch("builtins.print", side_effect=BrokenPipeError("closed")):
            with self.assertRaises(helpers["UIStallTimeout"]):
                helpers["monitor_xcodebuild"](
                    ["xcodebuild", "build"], {}, 1,
                    subprocess_module=module, heartbeat_interval=1)
        self.assertTrue(killed, "A stalled xcodebuild must be killed")

    def test_stall_is_a_case_failure_so_the_bounded_retry_applies(self):
        """A stall must be retryable, not a hard sweep abort."""
        helpers = self.helpers
        self.assertTrue(issubclass(helpers["UIStallTimeout"],
                                   helpers["UICaseFailure"]))

    def test_ui_step_has_a_step_level_timeout(self):
        """The workflow step must carry its own bound."""
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        marker = "- name: Test isolated simulator UI"
        self.assertEqual(workflow.count(marker), 1,
                         "The UI step must be declared exactly once")
        step = workflow[workflow.index(marker):]
        end = step.find("\n      - name:", 1)
        step = step[:end] if end != -1 else step
        # The bound belongs to this step, so it must appear before the next
        # step starts and carry the intended value.
        self.assertRegex(step, r"timeout-minutes:\s*90\b",
                         "The UI step must carry a 90 minute bound")

    def test_ui_step_bound_clears_the_slowest_observed_sweep(self):
        """The step bound must not kill a sweep that may still be healthy.

        Measured on main, a successful UI step took up to 63 minutes and
        the slowest observed sweep — a run that was cancelled, not
        failed — reached 67 minutes. Case-level retries then inflated a
        healthy sweep past 70 minutes (two retried cases added 13), on
        pace for ~80. A bound below that tail turns a run that might
        still be healthy into a timeout failure.
        """
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        marker = "- name: Test isolated simulator UI"
        step = workflow[workflow.index(marker):]
        end = step.find("\n      - name:", 1)
        step = step[:end] if end != -1 else step
        match = re.search(r"timeout-minutes:\s*(\d+)\b", step)
        self.assertIsNotNone(match, "The UI step must carry a bound")
        bound = int(match.group(1))
        slowest_observed_minutes = 80
        self.assertGreater(bound, slowest_observed_minutes,
                           "The UI step bound must clear the slowest "
                           "observed sweep measured on main")

    def test_the_ios_job_bound_covers_the_ui_step_bound(self):
        """The job bound must leave room for the UI step to report."""
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        job_match = re.search(r"\n  ios-app:\n(?:.*\n)*?    timeout-minutes:\s*(\d+)\b",
                               workflow)
        self.assertIsNotNone(job_match, "The iOS job must carry a bound")
        job_bound = int(job_match.group(1))
        step_marker = "- name: Test isolated simulator UI"
        step = workflow[workflow.index(step_marker):]
        end = step.find("\n      - name:", 1)
        step = step[:end] if end != -1 else step
        step_match = re.search(r"timeout-minutes:\s*(\d+)\b", step)
        self.assertIsNotNone(step_match)
        step_bound = int(step_match.group(1))
        # The other steps of the job take up to 25 minutes on a contended
        # runner, so the job bound has to cover them as well.
        other_steps_minutes = 25
        self.assertGreaterEqual(
            job_bound, step_bound + other_steps_minutes,
            "The job bound must cover the UI step bound and the other steps")

    def test_idle_budget_covers_the_test_execution_allowance(self):
        """A healthy but slow case must not be killed as a stall."""
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        sweep = ui_case_sweep_statements(tree)
        calls = [
            node for node in ast.walk(ast.Module(body=sweep, type_ignores=[]))
            if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name)
            and node.func.id == "monitor_xcodebuild"
        ]
        self.assertEqual(len(calls), 1,
                         "The sweep must run xcodebuild through the monitor")
        # A substring check would also match a comment, and would break on
        # a harmless reformat. The budget must be a max() of a floor and
        # the execution allowance plus a margin.
        budgets = [
            node for node in ast.walk(ast.Module(body=sweep, type_ignores=[]))
            if isinstance(node, ast.Assign)
            and any(isinstance(target, ast.Name)
                    and target.id == "idle_budget"
                    for target in node.targets)
        ]
        self.assertEqual(len(budgets), 1,
                         "The sweep must compute exactly one idle budget")
        value = budgets[0].value
        self.assertIsInstance(value, ast.Call,
                              "The idle budget must be a max() call")
        self.assertIsInstance(value.func, ast.Name)
        self.assertEqual(value.func.id, "max",
                         "The idle budget must be the larger of a floor and "
                         "the execution allowance plus a margin")
        self.assertEqual(len(value.args), 2,
                         "The idle budget must compare exactly two bounds")
        def is_floor(arg):
            return (isinstance(arg, ast.Constant)
                    and isinstance(arg.value, int))

        def is_cover(arg):
            return isinstance(arg, ast.BinOp) and isinstance(arg.op, ast.Add)

        floors = [arg for arg in value.args if is_floor(arg)]
        covers = [arg for arg in value.args if is_cover(arg)]
        # The two bounds are a numeric floor and an allowance plus a
        # margin. Which one comes first is an implementation detail, so
        # they are matched semantically rather than by position.
        self.assertEqual(len(floors), 1,
                         "The idle budget needs one numeric floor bound")
        self.assertGreaterEqual(floors[0].value, 900,
                                "The idle budget floor must leave headroom "
                                "for a healthy compile")
        self.assertEqual(len(covers), 1,
                         "The idle budget needs one allowance-plus-margin "
                         "bound")
        cover = covers[0]
        sides = [cover.left, cover.right]
        allowances = [side for side in sides
                      if isinstance(side, ast.Name)
                      and side.id == "execution_allowance"]
        margins = [side for side in sides
                   if isinstance(side, ast.Constant)
                   and side.value == 300]
        self.assertEqual(len(allowances), 1,
                         "The idle budget must cover the execution allowance")
        self.assertEqual(len(margins), 1,
                         "The idle budget must add a margin to the allowance")

    def test_the_stall_kill_reaches_the_whole_process_group(self):
        """A stalled build must not leave its descendants behind.

        xcodebuild drives SwiftDriver and the test runner as descendants.
        Killing only xcodebuild leaves those alive, and they keep the
        simulator busy for the next attempt. This test uses real
        processes, because the point of the fix is the real kill
        semantics that a stand-in cannot show.
        """
        helpers = self.helpers
        marker = pathlib.Path(tempfile.mkdtemp(prefix="holon-ios-ui-group-"))
        grandchild_marker = marker / "grandchild.pid"
        script = f"sleep 300 & echo $! > {grandchild_marker}; wait"
        try:
            with self.assertRaises(helpers["UIStallTimeout"]):
                helpers["monitor_xcodebuild"](
                    ["/bin/sh", "-c", script], {}, 1,
                    subprocess_module=subprocess, heartbeat_interval=1)
            # The group kill must reach the backgrounded descendant too.
            deadline = time.monotonic() + 10
            while not grandchild_marker.exists() and time.monotonic() < deadline:
                threading.Event().wait(0.05)
            self.assertTrue(grandchild_marker.exists(),
                            "the descendant must have been started")
            pid = int(grandchild_marker.read_text().strip())
            deadline = time.monotonic() + 10
            while process_alive(pid) and time.monotonic() < deadline:
                threading.Event().wait(0.1)
            self.assertFalse(process_alive(pid),
                             "the descendant of a stalled build must be "
                             "killed with its process group")
        finally:
            shutil.rmtree(marker, ignore_errors=True)

    def test_a_watcher_that_cannot_start_still_kills_the_child(self):
        """A watcher start() failure must not skip the child cleanup.

        join() on a never-started thread raises RuntimeError. If the
        cleanup order let that propagate, the orphan kill and the log
        replay would be skipped, and the join error would mask the
        original start failure. The kill and the replay must not depend
        on the watcher's own liveness.
        """
        helpers = self.helpers
        killed = []

        class NeverStartingThread:
            def __init__(self, target=None, daemon=None):
                pass

            def start(self):
                raise RuntimeError("can't start new thread")

            def join(self, timeout=None):
                # Real semantics: joining a never-started thread raises.
                raise RuntimeError("cannot join thread before it is started")

        class ThreadlessModule:
            Event = staticmethod(threading.Event)
            Thread = NeverStartingThread

        def popen(command, env=None, stdout=None, stderr=None, **keywords):
            stdout.write("started\n")
            stdout.flush()
            return self.SilentProcess(killed)

        standin = SimpleNamespace(Popen=popen, STDOUT=subprocess.STDOUT)
        original_threading = helpers["threading"]
        original_sys = helpers["sys"]
        helpers["threading"] = ThreadlessModule
        helpers["sys"] = SimpleNamespace(stdout=StringIO())
        try:
            with self.assertRaisesRegex(RuntimeError, "can't start new thread"):
                helpers["monitor_xcodebuild"](
                    ["/bin/sh", "-c", "echo started"], {}, 1,
                    subprocess_module=standin)
            self.assertTrue(killed, "the child must be killed even when "
                                    "the watcher never started")
            self.assertIn("started", helpers["sys"].stdout.getvalue(),
                          "the log replay must not be skipped")
        finally:
            helpers["threading"] = original_threading
            helpers["sys"] = original_sys

    def test_a_healthy_build_longer_than_the_grace_period_survives(self):
        """A healthy build must not be killed by the watchdog's grace period.

        The grace period bounds how long a *dead* watchdog may stay
        silent. It is not a cap on how long a build may run: the iOS UI
        sweep legitimately runs for many minutes while the watchdog
        happily watches it. A grace that armed against a live watchdog
        would kill every healthy build.
        """
        helpers = self.helpers

        class HealthyProcess:
            returncode = None
            pid = None

            def __init__(self):
                self.killed = False

            def wait(self, timeout=None):
                # A healthy child that outlives the grace period and then
                # finishes on its own. subprocess.Popen.wait(timeout=)
                # raises TimeoutExpired while the child is still running.
                deadline = time.monotonic() + (timeout or 0)
                while time.monotonic() < deadline:
                    threading.Event().wait(0.01)
                raise subprocess.TimeoutExpired("xcodebuild", timeout)

            def poll(self):
                return 0

            def kill(self):
                self.killed = True
                return None

        process = HealthyProcess()
        state = {"watcher_failed": None, "wait_timed_out": None}

        class LiveWatcher:
            """A watcher that is alive and reports nothing."""

            def is_alive(self):
                return True

        # The child returns once the grace period has passed. A grace
        # that armed against a live watcher would have killed it by now.
        def bounded_wait(timeout=None):
            threading.Event().wait(2.5)
            return 0

        process.wait = bounded_wait
        returncode = helpers["_wait_for_child"](
            process, state, poll_interval=0.2, watcher_failure_grace=1.0,
            watcher=LiveWatcher())
        self.assertEqual(returncode, 0,
                         "A healthy build must not be killed by the grace "
                         "period")
        self.assertFalse(process.killed,
                         "A healthy build must not be killed by the grace "
                         "period")
        self.assertIsNone(state["wait_timed_out"])

    def test_a_dead_watchdog_cannot_leave_an_unbounded_wait(self):
        """The main thread must not block forever on a dead watchdog.

        The watcher can die before the child handle is published, so the
        kill it attempted had nothing to kill. The main thread has to
        notice that the watchdog is gone instead of waiting on the child
        without a bound.
        """
        helpers = self.helpers

        class UnkillableProcess:
            returncode = None
            pid = None

            def wait(self, timeout=None):
                # A child that never exits. The wait must stay bounded, so
                # the stand-in honours the timeout like subprocess does.
                if timeout is not None:
                    raise subprocess.TimeoutExpired("xcodebuild", timeout)
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    threading.Event().wait(0.05)
                raise AssertionError("the wait must not run unbounded")

            def poll(self):
                return None

            def kill(self):
                return None

        module = Mock(Popen=Mock(return_value=UnkillableProcess()),
                      STDOUT=subprocess.STDOUT)
        # A failing growth probe breaks the watcher the way an unexpected
        # runtime error would.
        with patch("pathlib.Path.stat",
                   side_effect=RuntimeError("probe failed")):
            with self.assertRaises(helpers["UIStallTimeout"]):
                helpers["monitor_xcodebuild"](
                    ["xcodebuild", "build"], {}, 3600,
                    subprocess_module=module, heartbeat_interval=1)

    def test_a_reported_watchdog_failure_is_killed_by_the_main_thread(self):
        """The main thread must kill the child once the watchdog is gone.

        The watchdog kills the child on its way out, but that kill is
        best effort. If it did not land, the main thread must stop the
        child itself instead of waiting on a child nobody watches.
        """
        helpers = self.helpers
        killed_by = []

        class SurvivesWatchdogKill:
            """A live child whose watchdog already reported its death.

            The child only exits once killed, and ``wait()`` mirrors
            ``subprocess.Popen.wait(timeout=)``: the timeout is real
            waiting. An immediate raise would let a broken main loop
            spin without the grace clock ever advancing, so a regression
            would hang the suite instead of failing it.
            """

            returncode = None
            pid = None

            def __init__(self):
                self.killed = False
                self.polls = 0

            def wait(self, timeout=None):
                self.polls += 1
                if self.killed:
                    return 137
                if timeout is None:
                    raise AssertionError("the wait must stay bounded")
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    threading.Event().wait(0.01)
                if self.polls > 60:
                    # Nobody killed the child and no deadline fired; the
                    # wait is looping forever. Fail instead of hanging.
                    raise AssertionError(
                        "a reported watchdog failure must end the wait")
                raise subprocess.TimeoutExpired("xcodebuild", timeout)

            def poll(self):
                return 137 if self.killed else None

            def kill(self):
                self.killed = True
                killed_by.append(threading.current_thread().name)
                return None

        dead_watcher = threading.Thread(target=lambda: None)
        dead_watcher.start()
        dead_watcher.join()

        process = SurvivesWatchdogKill()
        state = {"watcher_failed": RuntimeError("watchdog gone"),
                 "wait_timed_out": None, "wait_unsupported": False}
        started = time.monotonic()
        returncode = helpers["_wait_for_child"](
            process, state, poll_interval=0.1, watcher_failure_grace=0.5,
            watcher=dead_watcher)
        elapsed = time.monotonic() - started
        self.assertEqual(returncode, 137)
        self.assertEqual(killed_by, ["MainThread"],
                         "the main thread must kill a child whose watchdog "
                         "died")
        self.assertIsNone(state["wait_timed_out"])
        self.assertLess(elapsed, 0.5,
                        "a reported failure must not wait out the grace "
                        "period before killing")

    def test_a_silent_dead_watchdog_is_killed_by_the_main_thread(self):
        """A watchdog that dies without reporting must not stall the wait.

        The watcher records its death in watcher_failed, but a thread can
        terminate without running that handler. The main thread must
        notice a watcher that is no longer alive and kill the child after
        the grace period, instead of waiting on a child nobody watches.
        """
        helpers = self.helpers
        killed_by = []

        class UnwatchedProcess:
            returncode = None
            pid = None

            def __init__(self):
                self.killed = False
                self.polls = 0

            def wait(self, timeout=None):
                self.polls += 1
                if self.killed:
                    return 137
                if timeout is None:
                    raise AssertionError("the wait must stay bounded")
                # Mirror subprocess.Popen.wait(timeout=): the timeout is
                # real waiting, so the grace clock can actually elapse.
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    threading.Event().wait(0.01)
                if self.polls > 60:
                    # A dead watchdog that never kills the child leaves
                    # the wait looping forever; fail instead of hanging.
                    raise AssertionError(
                        "a dead watchdog must end the wait")
                raise subprocess.TimeoutExpired("xcodebuild", timeout)

            def poll(self):
                return 137 if self.killed else None

            def kill(self):
                self.killed = True
                killed_by.append(threading.current_thread().name)
                return None

        dead_watcher = threading.Thread(target=lambda: None)
        dead_watcher.start()
        dead_watcher.join()

        process = UnwatchedProcess()
        state = {"watcher_failed": None, "wait_timed_out": None,
                 "wait_unsupported": False}
        started = time.monotonic()
        returncode = helpers["_wait_for_child"](
            process, state, poll_interval=0.1, watcher_failure_grace=0.5,
            watcher=dead_watcher)
        elapsed = time.monotonic() - started
        self.assertEqual(returncode, 137)
        self.assertEqual(killed_by, ["MainThread"],
                         "the main thread must kill the child once the "
                         "watcher is dead")
        self.assertEqual(state["wait_timed_out"], 0.5)
        self.assertGreaterEqual(elapsed, 0.5,
                                "the grace must actually elapse before the "
                                "kill")

    def test_an_unsupported_bounded_wait_fails_closed(self):
        """A wait() without timeout support must fail closed, not hang.

        The watchdog exists to prevent unbounded waits. If the child's
        wait() cannot take a timeout at all, retrying it without a bound
        would reintroduce exactly that, so the child is killed and the
        case fails.
        """
        helpers = self.helpers

        class TimeoutlessProcess:
            returncode = None
            pid = None

            def __init__(self):
                self.killed = False

            def wait(self, timeout=None):
                raise TypeError(
                    "wait() got an unexpected keyword argument 'timeout'")

            def poll(self):
                return None

            def kill(self):
                self.killed = True
                return None

        process = TimeoutlessProcess()
        module = Mock(Popen=Mock(return_value=process),
                      STDOUT=subprocess.STDOUT)
        with self.assertRaises(helpers["UIStallTimeout"]) as raised:
            helpers["monitor_xcodebuild"](
                ["xcodebuild", "build"], {}, 3600,
                subprocess_module=module, heartbeat_interval=1)
        self.assertIn("unbounded", str(raised.exception))
        self.assertTrue(process.killed)

    def test_a_foreign_type_error_from_wait_is_not_swallowed(self):
        """A TypeError unrelated to the timeout kwarg must surface.

        The timeout-kwarg fallback is matched on the error message, so a
        TypeError raised inside a real wait() must propagate rather than
        be misread as an unsupported wait.
        """
        helpers = self.helpers

        class BrokenWaitProcess:
            returncode = None
            pid = None

            def wait(self, timeout=None):
                raise TypeError("waiter exploded")

            def poll(self):
                return None

            def kill(self):
                return None

        state = {"watcher_failed": None, "wait_timed_out": None,
                 "wait_unsupported": False}
        with self.assertRaises(TypeError):
            helpers["_wait_for_child"](
                BrokenWaitProcess(), state, poll_interval=0.1)
        self.assertFalse(state["wait_unsupported"])

    def test_sweep_runs_xcodebuild_through_the_monitored_runner(self):
        """The sweep must not call xcodebuild through an unbounded run()."""
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        sweep = ui_case_sweep_statements(tree)
        module = ast.Module(body=sweep, type_ignores=[])
        ast.fix_missing_locations(module)
        calls = []
        for node in ast.walk(module):
            if not (isinstance(node, ast.Call)
                    and isinstance(node.func, ast.Attribute)
                    and node.func.attr == "run"
                    and isinstance(node.func.value, ast.Name)
                    and node.func.value.id == "subprocess"):
                continue
            command = node.args[0] if node.args else None
            if isinstance(command, ast.Name) and command.id == "command":
                # The xcodebuild invocation is routed through the monitored
                # runner; it must not reach subprocess.run unbounded.
                calls.append(node)
        self.assertEqual(calls, [],
                         "The UI case must not call subprocess.run directly")
        source_text = source.read_text()
        self.assertIn("monitor_xcodebuild(", source_text,
                      "The UI case must go through the monitored runner")



class UIFixtureEvidenceContracts(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="holon-ios-evidence-contract-")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name).resolve()
        self.helpers = fixture_helpers()
        source = ROOT / "scripts/ios_ui_fixture.py"
        self.tree = ast.parse(source.read_text(), filename=str(source))
        self.source = str(source)

    def execute(self, nodes, namespace):
        exec(compile(ast.Module(body=nodes, type_ignores=[]), self.source, "exec"), namespace)

    def test_default_cleans_up_and_opt_in_retains_private_material_on_error(self):
        for retain in (False, True):
            for fail in (False, True):
                target = self.directory / f"retained-{retain}-{fail}"
                environment = {"IOS_UI_FIXTURE_PATH": str(target)} if retain else {}
                with self.subTest(retain=retain, fail=fail), patch.dict(os.environ, environment, clear=True):
                    original_umask = os.umask(0o022)
                    try:
                        with patch("builtins.print") as output:
                            try:
                                with self.helpers["fixture_directory"](ROOT) as root:
                                    (root / "daemon-material").write_text("isolated daemon data")
                                    self.assertEqual(root.stat().st_mode & 0o777, 0o700)
                                    if retain:
                                        self.assertEqual((root / "daemon-material").stat().st_mode & 0o777, 0o600)
                                    if fail:
                                        raise RuntimeError("fixture failed")
                            except RuntimeError:
                                self.assertTrue(fail)
                        self.assertEqual(root.exists(), retain)
                        if retain:
                            self.assertEqual((root / "daemon-material").read_text(), "isolated daemon data")
                            self.assertIn(str(root), str(output.call_args_list))
                        else:
                            output.assert_not_called()
                        self.assertEqual(os.umask(0o022), 0o022, "Restore the caller's umask")
                    finally:
                        os.umask(original_umask)

    def test_rejects_collisions_and_dangerous_paths_without_touching_them(self):
        repo = self.directory / "repo"
        repo.mkdir()
        existing = self.directory / "existing"
        existing.mkdir(mode=0o755)
        sentinel = existing / "sentinel"
        sentinel.write_text("untouched")
        alias = self.directory / "repo-alias"
        alias.symlink_to(repo, target_is_directory=True)
        dangling = self.directory / "dangling"
        dangling.symlink_to(self.directory / "absent")
        paths = ["", "/", "relative", str(self.directory / "absent" / ".." / "escape"),
                 str(repo / "inside"), str(alias / "inside"), str(existing),
                 str(sentinel), str(dangling), str(self.directory / "missing" / "child")]
        for path in paths:
            with self.subTest(path=path), patch.dict(os.environ, {"IOS_UI_FIXTURE_PATH": path}, clear=True):
                with self.assertRaises((RuntimeError, OSError)):
                    with self.helpers["fixture_directory"](repo):
                        self.fail("Unsafe retention path accepted")
        self.assertEqual(sentinel.read_text(), "untouched")
        self.assertEqual(existing.stat().st_mode & 0o777, 0o755)
        self.assertFalse((repo / "inside").exists())
        self.assertFalse((self.directory / "escape").exists())

    def test_retention_rejects_all_roots_of_a_linked_git_worktree(self):
        canonical = self.directory / "canonical checkout"
        linked = self.directory / "linked checkout"
        sibling = self.directory / "sibling checkout"
        subprocess.run(["git", "init", "-q", str(canonical)], check=True)
        (canonical / "tracked").write_text("fixture")
        subprocess.run(["git", "-C", str(canonical), "add", "tracked"], check=True)
        subprocess.run(["git", "-C", str(canonical), "-c", "user.name=Fixture",
                        "-c", "user.email=fixture@example.test", "commit", "-qm", "fixture"], check=True)
        for worktree in (linked, sibling):
            subprocess.run(["git", "-C", str(canonical), "worktree", "add", "-q",
                            "--detach", str(worktree), "HEAD"], check=True)
        alias = self.directory / "canonical-alias"
        alias.symlink_to(canonical, target_is_directory=True)
        for number, parent in enumerate((canonical, linked, sibling, alias)):
            target = parent / f"private-evidence-{number}"
            with self.subTest(parent=parent), patch.dict(os.environ, {"IOS_UI_FIXTURE_PATH": str(target)}):
                with self.assertRaises(RuntimeError):
                    with self.helpers["fixture_directory"](linked):
                        self.fail("A related Git checkout must not retain raw databases")
                self.assertFalse(target.exists())
        target = self.directory / "safe-evidence"
        with patch.dict(os.environ, {"IOS_UI_FIXTURE_PATH": str(target)}), patch("builtins.print"):
            with self.helpers["fixture_directory"](linked) as retained:
                self.assertEqual(retained, target)
        self.assertEqual(target.stat().st_mode & 0o777, 0o700)

    def test_real_case_loop_exports_sqlite_rows_and_zero_zero_zero_one_deltas(self):
        evidence = self.directory / "evidence"
        home = self.directory / "holon"
        (home / "state").mkdir(parents=True)
        database = home / "state" / "runtime.sqlite"
        row = dict(
            report_id="report_fixture", reporter_principal="isolated-test-principal",
            agent_id="holon-tester", turn_id="turn-fixture", message_id="message-fixture",
            category="spam_or_other", description="IOS_CONTENT_REPORT_ACCEPTANCE",
            content_snapshot="IOS_RICH_ASSISTANT: actual stored test response",
            content_snapshot_hash=hashlib.sha256(b"IOS_RICH_ASSISTANT: actual stored test response").hexdigest(),
            snapshot_truncated=0, source_message_created_at="2026-10-10T00:00:00Z",
            source_origin_json='{"kind":"model"}', source_authority_class="model_output",
            status="received", client_request_id=UUID,
            created_at="2026-10-10T00:01:00Z", updated_at="2026-10-10T00:01:00Z")
        with sqlite3.connect(database) as connection:
            connection.execute("CREATE TABLE content_reports (" + ", ".join(
                name + (" INTEGER" if name == "snapshot_truncated" else " TEXT")
                for name in row) + ")")
            connection.execute("CREATE TABLE auth_records (token TEXT)")
            connection.execute("INSERT INTO auth_records VALUES ('do-not-export-auth-token')")
        methods = [
            "testContentReportFullResponseIncludesTail",
            "testContentReportConfirmationCancelDoesNotPersist",
            "testContentReportInvalidExplanationCannotShowAccepted",
            "testContentReportAcceptedReceiptCannotSubmitTwice",
        ]

        def run(command, env=None, **keywords):
            if "-only-testing:HolonUITests/HolonUITests/" + methods[-1] in command:
                with sqlite3.connect(database) as connection:
                    connection.execute("INSERT INTO content_reports VALUES (" +
                                       ",".join("?" for _ in row) + ")", tuple(row.values()))
            return Mock(returncode=0)

        sweep = ui_case_sweep_statements(self.tree)
        namespace = dict(self.helpers, cases=[(method, "large") for method in methods],
            bundles=[Path(method + ".xcresult") for method in methods], home=home,
            fixture_evidence=evidence, lost_response_acceptance=False,
            simulator=UUID, repo=str(ROOT), agent="holon-tester", rich_turn="turn-fixture",
            destination="fixture", derived="fixture", test_env={},
            local=Mock(return_value={"ticket": "do-not-export-pairing-ticket"}),
            simulator_text_size=lambda *args: nullcontext(),
            simulator_appearance=lambda *args: nullcontext(),
            runtime_text_size_control=lambda *args: nullcontext(("fixture", "do-not-export-control-token")),
            subprocess=mocked_subprocess(run))
        with patch("builtins.print"):
            self.execute(sweep, namespace)
        deltas = []
        for method in methods:
            snapshots = []
            for phase in ("before", "after"):
                value = json.loads((evidence / f"content-reports-{method}-{phase}.json").read_text())
                exported = evidence / value["sqlite"]["path"]
                self.assertEqual(value["sqlite"]["sha256"], hashlib.sha256(exported.read_bytes()).hexdigest())
                self.assertIn("content_reports-only", value["source"])
                self.assertEqual(value["columns"], list(row))
                with sqlite3.connect(exported) as connection:
                    self.assertEqual(connection.execute(
                        "SELECT name FROM sqlite_master WHERE type='table'").fetchall(), [("content_reports",)])
                    self.assertEqual(connection.execute("SELECT * FROM content_reports").fetchall(),
                                     [tuple(item[column] for column in row) for item in value["rows"]])
                self.assertEqual(value["row_count"], len(value["rows"]))
                self.assertEqual(exported.stat().st_mode & 0o777, 0o600)
                if value["rows"]:
                    self.assertEqual(value["rows"], [row])
                snapshots.append(value["row_count"])
            deltas.append(snapshots[1] - snapshots[0])
        self.assertEqual(deltas, [0, 0, 0, 1])
        self.assertTrue(database.exists(), "Keep the original daemon database separate from table-only exports")
        with sqlite3.connect(database) as connection:
            self.assertEqual(connection.execute("SELECT token FROM auth_records").fetchone()[0],
                             "do-not-export-auth-token")
        for path in evidence.iterdir():
            self.assertNotIn(b"do-not-export", path.read_bytes())

        # An XCTest failure still leaves both factual snapshots, not an alleged pass.
        failed_evidence = self.directory / "failed-evidence"
        namespace.update(fixture_evidence=failed_evidence, cases=[(methods[0], "large")],
                         bundles=[Path("failed.xcresult")], subprocess=mocked_subprocess(Mock(return_value=Mock(returncode=65))))
        with self.assertRaises(RuntimeError):
            self.execute(sweep, namespace)
        self.assertEqual(len(list(failed_evidence.glob("*.sqlite"))), 2)

    def test_conversation_inputs_merges_turn_windows_not_message_identities(self):
        completed = {"turn_id": "turn-completed", "inputs": [{"message_id": "message-completed"}]}
        historical = {"turn_id": "turn-live", "inputs": [{"message_id": "message-live"}]}
        active = {"turn_id": "turn-live", "inputs": historical["inputs"] + [{"message_id": "message-added"}]}
        pending = {"message_id": "message-pending"}
        merge = self.helpers["conversation_inputs"]
        for history, live in [([completed, historical], [active]), ([completed, active], []),
                              ([completed], [active]), ([], [completed, active])]:
            with self.subTest(history=history, active=live):
                page = {"turns": history, "active_turns": live, "pending_inputs": [pending]}
                self.assertEqual(merge(page), completed["inputs"] + active["inputs"] + [pending])
        duplicate = historical["inputs"][0]
        # Only repeated turn membership is merged. Real repeated inputs must
        # remain visible to the strict per-message evidence validator.
        for page in [
            {"turns": [{"turn_id": "turn-live", "inputs": [duplicate, duplicate]}]},
            {"turns": [historical], "active_turns": [{"turn_id": "turn-other", "inputs": [duplicate]}]},
            {"turns": [historical], "pending_inputs": [duplicate]},
        ]:
            with self.subTest(duplicate=page):
                self.assertEqual(merge(page), [duplicate, duplicate])

    def test_proxy_keeps_raw_identity_chains_and_direct_share_material_without_headers(self):
        evidence = self.directory / "evidence"
        workspace = self.directory / "workspace"
        workspace.mkdir()
        # A passive, valid PNG produced by the same chunk format as the fixture.
        def chunk(kind, data):
            return struct.pack("!I", len(data)) + kind + data + struct.pack("!I", self.helpers["zlib"].crc32(kind + data))
        image = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack("!2I5B", 16, 16, 8, 2, 0, 0, 0))
                 + chunk(b"IDAT", self.helpers["zlib"].compress((b"\0" + bytes([40, 100, 190]) * 16) * 16))
                 + chunk(b"IEND", b""))
        inbox = workspace / "media" / "inbox"
        inbox.mkdir(parents=True)
        (inbox / "request-image.png").write_bytes(image)
        (inbox / "request-file.txt").write_bytes(b"IOS_SHARED_FILE_BYTES")
        inputs, receipts, messages = {}, {}, {}

        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                if self.path.endswith("/prompt"):
                    request_id = body["client_request_id"]
                    duplicate = request_id in receipts
                    receipts.setdefault(request_id, "message-" + request_id)
                    text = body["text"]
                    for item in body.get("attachments", []):
                        is_image = item["kind"] == "image"
                        path = inbox / ("request-image.png" if is_image else "request-file.txt")
                        if text and not text.endswith("\n"):
                            text += "\n"
                        label = item.get("name", "image 1" if is_image else "file 1")
                        text += f"\n{'!' if is_image else ''}[{label}]({path.as_posix().replace(' ', '%20')})"
                    inputs.setdefault(request_id, {"message_id": receipts[request_id], "preview": text})
                    messages.setdefault(receipts[request_id], {"id": receipts[request_id],
                        "agent_id": "holon-tester", "body": {"type": "text", "text": text}})
                    data = json.dumps({"ok": True, "agent_id": "holon-tester",
                        "message_id": receipts[request_id],
                        "disposition": "duplicate" if duplicate else "accepted"}).encode()
                else:
                    data = b'{"ticket":"do-not-record-pairing-ticket"}'
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        upstream = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
        worker = threading.Thread(target=upstream.serve_forever, kwargs={"poll_interval": .05})
        worker.start()
        self.addCleanup(worker.join)
        self.addCleanup(upstream.server_close)
        self.addCleanup(upstream.shutdown)
        release = threading.Event()
        namespace = dict(self.helpers, port=upstream.server_port, fixture_evidence=evidence,
            fixture_case="testLostResponseAndProcessRecovery", prompt_evidence=[],
            lost_receipts=[], lost_lock=threading.Lock(), release_lost_response=release,
            lost_response_acceptance=True, share_acceptance=True, proxy_control_token="do-not-record-control-token",
            proxy_available=threading.Event(), proxy_connections=set(), proxy_state_lock=threading.Lock())
        namespace["proxy_available"].set()
        proxy_class = next(node for node in ast.walk(self.tree)
                           if isinstance(node, ast.ClassDef) and node.name == "LostResponseProxy")
        self.execute([proxy_class], namespace)
        proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), namespace["LostResponseProxy"])
        worker = threading.Thread(target=proxy.serve_forever, kwargs={"poll_interval": .05})
        worker.start()
        self.addCleanup(worker.join)
        self.addCleanup(proxy.server_close)
        self.addCleanup(proxy.shutdown)

        def send(request, path="/api/agents/holon-tester/prompt"):
            body = json.dumps(request, ensure_ascii=False, indent=1).encode()
            connection = http.client.HTTPConnection("127.0.0.1", proxy.server_port, timeout=5)
            try:
                connection.request("POST", path, body, {
                    "Content-Type": "application/json", "Authorization": "Bearer do-not-record-session-token",
                    "Cookie": "do-not-record-cookie"})
                response = connection.getresponse()
                self.assertEqual(response.status, 200)
                try:
                    response.read()
                except http.client.IncompleteRead:
                    self.assertFalse(release.is_set())
            finally:
                connection.close()
            return body

        with patch("builtins.print"):
            originals = []
            for number, text in enumerate(["IOS_LOST_RESPONSE_SEND\nOriginal App body",
                                          "IOS_SHARED_TEXT\nLiteral **operator** input"]):
                namespace["fixture_case"] = ["testLostResponseAndProcessRecovery", "testDirectAgentShareWorkflow"][number]
                release.clear()
                request = {"text": text, "client_request_id": UUID if not number else str(uuid.UUID(int=1)), "attachments": []}
                originals.extend([send(request)])
                release.set()
                originals.extend([send(request)])
            for number, (kind, text, attachments) in enumerate([
                ("url", "https://example.test/holon-share", []),
                ("image", "", [{"kind": "image", "media_type": "image/png", "data_base64": base64.b64encode(image).decode()}]),
                ("file", "", [{"kind": "file", "name": "shared-note.txt", "media_type": "text/plain",
                              "data_base64": base64.b64encode(b"IOS_SHARED_FILE_BYTES").decode()}]),
            ], 2):
                originals.append(send({"text": text, "attachments": attachments, "client_request_id": str(uuid.UUID(int=number))}))
            send({"pairing_ticket": "do-not-record-pairing-ticket"}, "/api/auth/pairing/issue")
        records = namespace["prompt_evidence"]
        self.assertEqual(len(records), 7)
        for index, (record, body) in enumerate(zip(records, originals), 1):
            self.assertEqual((evidence / record["request_body"]["path"]).read_bytes(), body)
            for field in ("request_body", "receipt_body"):
                path = evidence / record[field]["path"]
                self.assertEqual(record[field]["sha256"], hashlib.sha256(path.read_bytes()).hexdigest())
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads((evidence / f"prompt-{index:04}.json").read_text()), record)
            self.assertEqual(record["receipt"]["agent_id"], "holon-tester")
        for first, retry in [(records[0], records[1]), (records[2], records[3])]:
            self.assertEqual(first["request"], retry["request"])
            self.assertEqual(first["receipt"]["message_id"], retry["receipt"]["message_id"])
            self.assertEqual([first["receipt"]["disposition"], retry["receipt"]["disposition"]], ["accepted", "duplicate"])
            self.assertEqual([first["delivered"], retry["delivered"]], [False, True])
        self.assertEqual({record["kind"] for record in records},
                         {"app-lost-response", "share-text", "share-url", "share-image", "share-file"})
        # Execute the real post-run daemon material check/export, not a test-only exporter.
        share_block = next(node for node in ast.walk(self.tree)
                           if isinstance(node, ast.If) and isinstance(node.test, ast.BoolOp)
                           and isinstance(node.test.values[0], ast.Name)
                           and node.test.values[0].id == "share_acceptance"
                           and any(isinstance(item, ast.Compare) for item in node.test.values))
        def local(method, path, payload=None):
            self.assertEqual(method, "GET")
            if "/messages/" in path:
                return messages[path.rsplit("/", 1)[-1]]
            # The real history SQL includes running turns also returned in the
            # active window. Native receipt delivery need not wait for completion.
            turn = {"turn_id": "turn-sharing", "inputs": list(inputs.values())}
            return {"turns": [turn], "active_turns": [turn]}

        namespace.update(mode="--populated", root=self.directory, workspace=workspace, agent="holon-tester",
                         local=local)
        with patch("builtins.print"):
            self.execute([share_block], namespace)
        shared = json.loads((evidence / "direct-share.json").read_text())
        self.assertEqual(len(shared["inputs"]), 4)
        self.assertEqual(len(shared["files"]), 2)
        self.assertEqual(len(shared["chains"]), 4)
        self.assertEqual(len({chain["client_request_id"] for chain in shared["chains"]}), 4)
        self.assertEqual(len({chain["message_id"] for chain in shared["chains"]}), 4)
        for chain in shared["chains"]:
            self.assertEqual(chain["message"]["id"], chain["input"]["message_id"])
            expected = 1 if chain["kind"] in ("share-image", "share-file") else 0
            self.assertEqual(len(chain["attachments"]), expected)
        for item in shared["files"]:
            self.assertEqual(item["sha256"], hashlib.sha256((self.directory / item["path"]).read_bytes()).hexdigest())
        # Exercise the real acceptance block with invalid identity/material chains.
        for fault in ("collapsed-message", "reused-uuid", "wrong-text", "swapped-attachment",
                      "missing-input", "wrong-agent", "changed-retry", "invalid-uuid", "duplicate-input",
                      "cross-turn-duplicate", "pending-duplicate"):
            damaged = json.loads(json.dumps(records))
            damaged_inputs = list(json.loads(json.dumps(inputs)).values())
            damaged_messages = json.loads(json.dumps(messages))
            share_records = [record for record in damaged if record["kind"].startswith("share-")]
            text_id = share_records[0]["receipt"]["message_id"]
            url_record = next(record for record in share_records if record["kind"] == "share-url")
            image_id = next(record["receipt"]["message_id"] for record in share_records if record["kind"] == "share-image")
            if fault == "collapsed-message":
                for record in share_records:
                    record["receipt"]["message_id"] = text_id
                damaged_inputs = [value for value in damaged_inputs if value["message_id"] == text_id]
                damaged_inputs[0]["preview"] += "\nhttps://example.test/holon-share shared-note"
            elif fault == "reused-uuid":
                url_record["request"]["client_request_id"] = share_records[0]["request"]["client_request_id"]
            elif fault == "wrong-text":
                damaged_messages[text_id]["body"]["text"] = "unrelated input"
            elif fault == "swapped-attachment":
                damaged_messages[image_id]["body"]["text"] = f"\n![shared]({inbox}/request-file.txt)"
            elif fault == "missing-input":
                damaged_inputs = [value for value in damaged_inputs if value["message_id"] != image_id]
            elif fault == "wrong-agent":
                url_record["receipt"]["agent_id"] = "another-agent"
            elif fault == "changed-retry":
                share_records[1]["request"]["text"] += "\nchanged on retry"
            elif fault == "invalid-uuid":
                url_record["request"]["client_request_id"] = "not-a-uuid"
            elif fault == "duplicate-input":
                damaged_inputs.append(next(value.copy() for value in damaged_inputs
                                           if value["message_id"] == text_id))

            def damaged_local(method, path, payload=None):
                if "/messages/" in path:
                    return damaged_messages[path.rsplit("/", 1)[-1]]
                turn = {"turn_id": "turn-sharing", "inputs": damaged_inputs}
                page = {"turns": [turn], "active_turns": [turn]}
                duplicate = next(value.copy() for value in damaged_inputs if value["message_id"] == text_id)
                if fault == "cross-turn-duplicate":
                    page["active_turns"].append({"turn_id": "turn-other", "inputs": [duplicate]})
                elif fault == "pending-duplicate":
                    page["pending_inputs"] = [duplicate]
                return page

            destination = self.directory / ("invalid-" + fault)
            namespace.update(prompt_evidence=damaged, local=damaged_local, fixture_evidence=destination)
            with self.subTest(fault=fault), patch("builtins.print"):
                with self.assertRaises(RuntimeError):
                    self.execute([share_block], namespace)
                self.assertFalse((destination / "direct-share.json").exists())
        for path in evidence.iterdir():
            self.assertNotIn(b"do-not-record", path.read_bytes())
            self.assertNotIn(b"Authorization", path.read_bytes())
        with self.assertRaises(RuntimeError):
            self.helpers["record_fixture_prompt"](evidence, 99, None, "/prompt",
                b'{"text":"IOS_SHARED_TEXT","session_token":"do-not-record"}',
                b'{"message_id":"message-fixture"}', 200, True)
        self.assertFalse((evidence / "prompt-0099-request.json").exists())

    def test_real_finally_stops_services_and_daemon_even_if_task_stop_fails(self):
        cleanup = next(node.finalbody for node in ast.walk(self.tree)
                       if isinstance(node, ast.Try) and node.finalbody
                       and isinstance(node.finalbody[0], ast.Try)
                       and any(isinstance(item, ast.Call) and isinstance(item.func, ast.Attribute)
                               and item.func.attr == "terminate"
                               for item in ast.walk(ast.Module(body=node.finalbody, type_ignores=[]))))
        for fail in (False, True):
            daemon = Mock()
            daemon.wait.side_effect = [subprocess.TimeoutExpired("isolated daemon", 8), None]
            release, proxy, provider = threading.Event(), Mock(), Mock()
            namespace = dict(task_id="fixture-task", agent="holon-tester", daemon=daemon,
                             release_held_run=release, proxy=proxy, provider=provider,
                             subprocess=subprocess, local=Mock(side_effect=RuntimeError("stop failed") if fail else None))
            if fail:
                with self.assertRaisesRegex(RuntimeError, "stop failed"):
                    self.execute(cleanup, namespace)
            else:
                self.execute(cleanup, namespace)
            self.assertTrue(release.is_set())
            for server in (proxy, provider):
                server.shutdown.assert_called_once()
                server.server_close.assert_called_once()
            daemon.terminate.assert_called_once()
            daemon.kill.assert_called_once()
            self.assertEqual(daemon.wait.call_args_list, [call(timeout=8), call()])


class UIFixtureProxyAvailabilityContracts(unittest.TestCase):
    def setUp(self):
        self.requests = []
        self.release_stream = threading.Event()
        self.addCleanup(self.release_stream.set)
        requests, release_stream = self.requests, self.release_stream

        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                requests.append((self.command, self.path))
                self.send_response(200)
                if self.path == "/api/events":
                    self.send_header("Content-Type", "text/event-stream")
                    self.end_headers()
                    self.wfile.write(b"data: initial-stream-event\n\n")
                    self.wfile.flush()
                    release_stream.wait(timeout=5)
                else:
                    data = b'{"agents":["isolated-agent"]}'
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)

            def do_POST(self):
                requests.append((self.command, self.path))
                data = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        upstream = self.start_server(Upstream)
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        proxy_class = next(node for node in ast.walk(tree)
                           if isinstance(node, ast.ClassDef) and node.name == "LostResponseProxy")
        self.namespace = dict(fixture_helpers(), port=upstream.server_port, fixture_evidence=None,
            fixture_case=None, prompt_evidence=[], lost_receipts=[], lost_lock=threading.Lock(),
            release_lost_response=threading.Event(), lost_response_acceptance=False,
            share_acceptance=False, proxy_control_token="fixture-runner-only-control",
            proxy_available=threading.Event(), proxy_connections=set(), proxy_state_lock=threading.Lock())
        self.namespace["proxy_available"].set()
        exec(compile(ast.Module(body=[proxy_class], type_ignores=[]), str(source), "exec"), self.namespace)
        self.proxy = self.start_server(self.namespace["LostResponseProxy"])
        self.upstream = upstream

    def start_server(self, handler):
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        server.daemon_threads = True
        worker = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": .05})
        worker.start()
        self.addCleanup(worker.join, 5)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return server

    def request(self, method, path, body=None, token=None, server=None):
        connection = http.client.HTTPConnection("127.0.0.1", (server or self.proxy).server_port, timeout=3)
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        try:
            connection.request(method, path, body=body, headers=headers)
            response = connection.getresponse()
            return response.status, response.read()
        finally:
            connection.close()

    def toggle(self, available):
        status, data = self.request("POST", "/fixture/node-availability",
            json.dumps({"available": available}).encode(), token="fixture-runner-only-control")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(data), {"available": available})

    def test_unavailability_blocks_reads_and_writes_without_stopping_upstream(self):
        status, data = self.request("GET", "/api/agents")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(data), {"agents": ["isolated-agent"]})
        self.toggle(False)
        before = list(self.requests)
        self.assertEqual(self.request("GET", "/api/agents")[0], 503)
        self.assertEqual(self.request("POST", "/api/agents/isolated-agent/prompt", b"{}")[0], 503)
        self.assertEqual(self.requests, before, "Unavailable requests cannot reach the isolated node")
        self.assertEqual(self.request("GET", "/api/agents", server=self.upstream)[0], 200,
                         "Only the fixture proxy is disabled, not its upstream daemon")
        self.toggle(True)
        status, data = self.request("POST", "/api/echo", b'{"text":"normal-pairing-session"}')
        self.assertEqual(status, 200)
        self.assertEqual(data, b'{"text":"normal-pairing-session"}')
        self.assertEqual(self.request("GET", "/api/agents")[0], 200)
        self.assertFalse(any(path.startswith("/fixture/") for _, path in self.requests))

    def test_control_requires_runner_token_and_strict_boolean_payload(self):
        for token in (None, "ordinary-app-session-token"):
            with self.subTest(token=token):
                status, _ = self.request("POST", "/fixture/node-availability",
                    b'{"available":false}', token=token)
                self.assertEqual(status, 403)
                self.assertTrue(self.namespace["proxy_available"].is_set())
        for payload in (b"{}", b'{"available":0}', b'{"available":"false"}',
                        b'{"available":false,"extra":1}', b"[]", b"invalid", b"", b"x" * 129):
            with self.subTest(payload=payload):
                status, _ = self.request("POST", "/fixture/node-availability",
                    payload, token="fixture-runner-only-control")
                self.assertEqual(status, 400)
                self.assertTrue(self.namespace["proxy_available"].is_set())
        self.assertEqual(self.requests, [])
        self.toggle(False)
        self.assertEqual(self.request("POST", "/fixture/release-lost-response",
                                     token="ordinary-app-session-token")[0], 403)
        self.assertFalse(self.namespace["release_lost_response"].is_set())
        self.assertEqual(self.request("POST", "/fixture/release-lost-response",
                                     token="fixture-runner-only-control")[0], 204)
        self.assertTrue(self.namespace["release_lost_response"].is_set(),
                        "Keep the existing lost-response/share control protocol")

    def test_unavailability_disconnects_already_established_quiet_sse(self):
        connection = http.client.HTTPConnection("127.0.0.1", self.proxy.server_port, timeout=3)
        self.addCleanup(connection.close)
        connection.request("GET", "/api/events")
        response = connection.getresponse()
        self.assertEqual(response.status, 200)
        self.assertEqual(response.readline(), b"data: initial-stream-event\n")
        self.assertEqual(response.readline(), b"\n")
        self.toggle(False)
        self.assertEqual(response.read(), b"", "Quiet SSE must close immediately, not after a heartbeat timeout")
        self.assertFalse(self.release_stream.is_set(), "The upstream stream was still quiet when disconnected")
        self.assertEqual(self.request("GET", "/api/events")[0], 503)
        self.toggle(True)
        self.assertEqual(self.request("GET", "/api/agents")[0], 200)


class UIFixtureProviderContracts(unittest.TestCase):
    def test_history_window_oracle_matches_reader_snapshot_limit(self):
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        block = next(node for node in ast.walk(tree)
                     if isinstance(node, ast.If) and isinstance(node.test, ast.Compare)
                     and isinstance(node.test.left, ast.Name) and node.test.left.id == "method"
                     and any(isinstance(value, ast.Constant)
                             and value.value == "testConversationHistoryWindowPosition"
                             for value in ast.walk(node.test)))
        code = compile(ast.Module(body=[block], type_ignores=[]), str(source), "exec")
        for count, older, newer in [(25, 0, 5), (33, 0, 13), (60, 25, 40)]:
            with self.subTest(turns=count):
                turns = [{"turn_id": f"turn-{index}", "key": {"turn_index": index}}
                         for index in range(count)]

                def snapshot(method, path):
                    self.assertEqual(method, "GET")
                    # The daemon defaults to 30; ReadingTransport explicitly reads 60.
                    limit = 60 if path.endswith("?limit=60") else 30
                    return {"turns": list(reversed(turns[-limit:]))}

                local = Mock(side_effect=snapshot)
                environment = {}
                exec(code, dict(method="testConversationHistoryWindowPosition",
                                agent="holon-tester", local=local, test_env=environment))
                self.assertEqual(environment["TEST_RUNNER_HOLON_UI_HISTORY_OLDER_TOP"],
                                 f"turn-{older}")
                self.assertEqual(environment["TEST_RUNNER_HOLON_UI_HISTORY_NEWER_TOP"],
                                 f"turn-{newer}")
                local.assert_called_once_with("GET", "/agents/holon-tester/conversation?limit=60")

    def test_failed_history_seed_clears_scope(self):
        helpers = fixture_helpers()
        history_seed_active = threading.Event()

        def fail_seed(*args):
            self.assertTrue(history_seed_active.is_set())
            raise RuntimeError("seed stopped")

        with self.assertRaisesRegex(RuntimeError, "seed stopped"):
            helpers["seed_fixture_history"](fail_seed, "holon-tester", history_seed_active)
        self.assertFalse(history_seed_active.is_set())

    def test_history_seed_scope_preserves_normal_and_streaming_markdown(self):
        source = ROOT / "scripts/ios_ui_fixture.py"
        tree = ast.parse(source.read_text(), filename=str(source))
        provider_class = next(node for node in ast.walk(tree)
                              if isinstance(node, ast.ClassDef) and node.name == "FakeProvider")
        history_seed_active = threading.Event()
        automatic_read_seed_active = threading.Event()
        with tempfile.TemporaryDirectory(prefix="holon-ios-ui-provider-") as directory:
            namespace = dict(http=http, json=json, root=Path(directory),
                             rich_activity_acceptance=False, report_acceptance=True,
                             history_acceptance=True, history_seed_active=history_seed_active,
                             automatic_read_seed_active=automatic_read_seed_active)
            exec(compile(ast.Module(body=[provider_class], type_ignores=[]),
                         str(source), "exec"), namespace)
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), namespace["FakeProvider"])
            namespace["provider"] = server
            worker = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": .05})
            worker.start()
            try:
                for streaming in (False, True):
                    for seeding in ("normal", "history", "automatic", "followup", "normal"):
                        if seeding != "normal":
                            history_seed_active.set()
                        else:
                            history_seed_active.clear()
                        namespace["FakeProvider"].automatic_read_followup_active = seeding == "followup"
                        if seeding in ("automatic", "followup"):
                            automatic_read_seed_active.set()
                        else:
                            automatic_read_seed_active.clear()
                        request = urllib.request.Request(
                            f"http://127.0.0.1:{server.server_port}/v1/chat/completions",
                            data=json.dumps({"model": "fixture-model", "stream": streaming,
                                             "messages": [
                                                 {"role": "user", "content": "IOS_HISTORY_024"},
                                                 {"role": "user", "content": "Produce the reading brief"},
                                             ]}).encode(),
                            headers={"Authorization": "Bearer isolated-test-only",
                                     "Content-Type": "application/json"})
                        with urllib.request.urlopen(request, timeout=5) as response:
                            body = response.read().decode()
                        with self.subTest(streaming=streaming, seeding=seeding):
                            if seeding in ("automatic", "followup"):
                                marker = ("IOS_AUTOMATIC_READ_FOLLOWUP_BRIEF" if seeding == "followup"
                                          else "IOS_AUTOMATIC_READ_BRIEF")
                                self.assertIn(marker, body)
                                self.assertNotIn("IOS_HISTORY_BRIEF", body)
                                self.assertNotIn("IOS_POPULATED_BRIEF", body)
                            elif seeding == "history":
                                self.assertIn("IOS_HISTORY_BRIEF", body)
                                self.assertNotIn("IOS_POPULATED_BRIEF", body)
                                self.assertIn("History fixture result.", body)
                                self.assertNotIn("Open fixture file", body)
                            else:
                                self.assertIn("IOS_POPULATED_BRIEF", body)
                                self.assertNotIn("IOS_HISTORY_BRIEF", body)
                                self.assertIn("Open fixture file", body)
                                self.assertNotIn("History fixture result.", body)
            finally:
                server.shutdown()
                server.server_close()
                worker.join(5)
            self.assertFalse(worker.is_alive())


class SimulatorAppearanceContracts(unittest.TestCase):
    @staticmethod
    def result(stdout=""):
        return subprocess.CompletedProcess([], 0, stdout=stdout, stderr="")

    def test_slow_case_configuration_remains_verified_and_restorable(self):
        responses = iter([self.result("light"), self.result(), self.result("dark"),
                          self.result(), self.result("light")])

        def slow_ui(command, **kwargs):
            if kwargs["timeout"] < 30:
                raise subprocess.TimeoutExpired(command, kwargs["timeout"])
            return next(responses)

        with patch("ios_simulator_text_size.subprocess.run", side_effect=slow_ui) as run:
            with simulator_appearance(UUID, "dark") as appearance:
                self.assertEqual(appearance, "dark")
            self.assertEqual([item.kwargs["timeout"] for item in run.call_args_list],
                             [120] * 5)

    def test_readback_and_restore_even_when_xcode_fails(self):
        responses = [self.result("light"), self.result(), self.result("dark"),
                     self.result(), self.result("light")]
        with patch("ios_simulator_text_size.subprocess.run", side_effect=responses) as run:
            with self.assertRaisesRegex(RuntimeError, "Xcode failed"):
                with simulator_appearance(UUID, "dark") as appearance:
                    self.assertEqual(appearance, "dark")
                    raise RuntimeError("Xcode failed")
            prefix = ["xcrun", "simctl", "ui", UUID, "appearance"]
            self.assertEqual([item.args[0] for item in run.call_args_list],
                             [prefix, prefix + ["dark"], prefix, prefix + ["light"], prefix])

    def test_unreadable_original_does_not_mutate(self):
        with patch("ios_simulator_text_size.subprocess.run", return_value=self.result("unknown")) as run:
            with self.assertRaisesRegex(RuntimeError, "可恢复"):
                with simulator_appearance(UUID, "dark"):
                    self.fail("Unverified appearance must not run XCTest")
            self.assertEqual(run.call_count, 1)

    def test_mismatched_readback_fails_and_restores(self):
        responses = [self.result("light"), self.result(), self.result("light"),
                     self.result(), self.result("light")]
        with patch("ios_simulator_text_size.subprocess.run", side_effect=responses) as run:
            with self.assertRaisesRegex(RuntimeError, "核验失败"):
                with simulator_appearance(UUID, "dark"):
                    self.fail("Mismatched appearance must not run XCTest")
            self.assertEqual(run.call_args_list[-2].args[0][-1], "light")

    def test_failed_restore_is_not_success(self):
        responses = [self.result("light"), self.result(), self.result("dark"),
                     self.result(), self.result("dark")]
        with patch("ios_simulator_text_size.subprocess.run", side_effect=responses):
            with self.assertRaisesRegex(RuntimeError, "核验失败"):
                with simulator_appearance(UUID, "dark"):
                    pass


class SimulatorTextSizeContracts(unittest.TestCase):
    @staticmethod
    def result(stdout=""):
        return subprocess.CompletedProcess([], 0, stdout=stdout, stderr="")

    def lifecycle(self, selected=MAXIMUM_TEXT_SIZE, restored="large"):
        return [self.result("large\n"), self.result(), self.result(selected + "\n"),
                self.result(), self.result(restored + "\n")]

    def test_slow_case_configuration_remains_verified_and_restorable(self):
        responses = iter(self.lifecycle())

        def slow_ui(command, **kwargs):
            if kwargs["timeout"] < 30:
                raise subprocess.TimeoutExpired(command, kwargs["timeout"])
            return next(responses)

        with patch("ios_simulator_text_size.subprocess.run", side_effect=slow_ui) as run:
            with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE) as category:
                self.assertEqual(category, MAXIMUM_TEXT_SIZE)
            self.assertEqual([item.kwargs["timeout"] for item in run.call_args_list],
                             [120] * 5)

    def test_initialize_dedicated_baseline_with_readback(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=[self.result(), self.result("medium\n"),
                                self.result(), self.result("large\n")]) as run:
            initialize_simulator_text_size(UUID)
            prefix = ["xcrun", "simctl", "ui", UUID, "content_size"]
            self.assertEqual(run.call_args_list, [
                call(["xcrun", "simctl", "bootstatus", UUID, "-b"],
                     check=True, timeout=180),
                call(prefix, check=True, capture_output=True, text=True, timeout=120),
                call(prefix + ["large"], check=True, capture_output=True, text=True, timeout=120),
                call(prefix, check=True, capture_output=True, text=True, timeout=120),
            ])

    def test_failed_boot_prevents_baseline_configuration(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=subprocess.CalledProcessError(149, ["xcrun"])) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                initialize_simulator_text_size(UUID)
            run.assert_called_once_with(
                ["xcrun", "simctl", "bootstatus", UUID, "-b"], check=True, timeout=180,
            )

    def test_unready_ui_service_prevents_baseline_configuration(self):
        for error in (
            subprocess.TimeoutExpired(["xcrun"], 120),
            subprocess.CalledProcessError(1, ["xcrun"]),
        ):
            with self.subTest(error=type(error).__name__):
                with patch("ios_simulator_text_size.subprocess.run",
                           side_effect=[self.result(), error]) as run:
                    with self.assertRaises(type(error)):
                        initialize_simulator_text_size(UUID)
                    self.assertEqual(run.call_count, 2)

    def test_unsupported_ui_service_prevents_baseline_configuration(self):
        for actual in ("unknown", "unsupported", ""):
            with self.subTest(actual=actual):
                with patch("ios_simulator_text_size.subprocess.run",
                           side_effect=[self.result(), self.result(actual)]) as run:
                    with self.assertRaises(RuntimeError):
                        initialize_simulator_text_size(UUID)
                    self.assertEqual(run.call_count, 2)

    def test_unverified_baseline_fails(self):
        for actual in ("medium", "unknown", "unsupported", ""):
            with self.subTest(actual=actual):
                with patch("ios_simulator_text_size.subprocess.run",
                           side_effect=[self.result(), self.result("large\n"),
                                        self.result(), self.result(actual)]) as run:
                    with self.assertRaises(RuntimeError):
                        initialize_simulator_text_size(UUID)
                    self.assertEqual(run.call_count, 4)

    def test_failed_baseline_configuration_is_not_success(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=[self.result(), self.result("large\n"),
                                subprocess.CalledProcessError(1, ["xcrun"])]) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                initialize_simulator_text_size(UUID)
            self.assertEqual(run.call_count, 3)

    def test_set_readback_and_restore(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=self.lifecycle()) as run:
            with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE) as category:
                self.assertEqual(category, MAXIMUM_TEXT_SIZE)
            prefix = ["xcrun", "simctl", "ui", UUID, "content_size"]
            self.assertEqual(run.call_args_list, [
                call(prefix, check=True, capture_output=True, text=True, timeout=120),
                call(prefix + [MAXIMUM_TEXT_SIZE], check=True, capture_output=True, text=True, timeout=120),
                call(prefix, check=True, capture_output=True, text=True, timeout=120),
                call(prefix + ["large"], check=True, capture_output=True, text=True, timeout=120),
                call(prefix, check=True, capture_output=True, text=True, timeout=120),
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
        for error in (subprocess.CalledProcessError(1, ["xcrun"]),
                      subprocess.TimeoutExpired(["xcrun"], 120)):
            with self.subTest(error=type(error).__name__):
                with patch("ios_simulator_text_size.subprocess.run", side_effect=error) as run:
                    with self.assertRaises(type(error)):
                        with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE):
                            self.fail("Failed initial read must not run XCTest")
                    self.assertEqual(run.call_count, 1)
                    self.assertEqual(run.call_args.kwargs["timeout"], 120)

    def test_unsupported_category_never_mutates(self):
        with patch("ios_simulator_text_size.subprocess.run") as run:
            with self.assertRaises(ValueError):
                set_simulator_text_size(UUID, "unsupported")
            run.assert_not_called()

    @staticmethod
    def request_control(control, payload, authenticated=True):
        url, token = control
        headers = {"Content-Type": "application/json"}
        if authenticated:
            headers["Authorization"] = "Bearer " + token
        request = urllib.request.Request(url, data=json.dumps(payload).encode(), headers=headers)
        return urllib.request.urlopen(request, timeout=5)

    def test_runtime_control_acknowledges_only_verified_system_size_and_closes(self):
        with patch("ios_simulator_text_size.subprocess.run",
                   side_effect=[self.result(), self.result(MAXIMUM_TEXT_SIZE)]) as run:
            with runtime_text_size_control(UUID) as control:
                self.assertTrue(control[0].startswith("http://127.0.0.1:"))
                with self.request_control(control, {"category": MAXIMUM_TEXT_SIZE}) as response:
                    self.assertEqual(json.load(response), {"category": MAXIMUM_TEXT_SIZE})
                self.assertEqual(run.call_args_list[0].args[0][-1], MAXIMUM_TEXT_SIZE)
                self.assertEqual(run.call_args_list[1].args[0][-1], "content_size")
                self.assertEqual([item.kwargs["timeout"] for item in run.call_args_list],
                                 [15, 15])
            with self.assertRaises(urllib.error.URLError):
                self.request_control(control, {"category": "large"})

    def test_runtime_control_rejects_unauthenticated_and_invalid_requests(self):
        with patch("ios_simulator_text_size.subprocess.run") as run:
            with runtime_text_size_control(UUID) as control:
                for payload, authenticated, status in [
                    ({"category": "large"}, False, 403),
                    ({"category": "medium"}, True, 400),
                    ({"category": MAXIMUM_TEXT_SIZE, "simulator": "other"}, True, 400),
                    ({"category": []}, True, 400),
                    ([], True, 400),
                ]:
                    with self.subTest(payload=payload):
                        with self.assertRaises(urllib.error.HTTPError) as failure:
                            self.request_control(control, payload, authenticated)
                        self.assertEqual(failure.exception.code, status)
                run.assert_not_called()

    def test_runtime_control_command_failure_or_wrong_readback_cannot_acknowledge(self):
        for responses, expected_calls in [
            ([subprocess.CalledProcessError(1, ["xcrun"])] * _RUNTIME_RETRY_ATTEMPTS,
             _RUNTIME_RETRY_ATTEMPTS),
            ([self.result(), self.result("large")], 2),
            ([subprocess.TimeoutExpired(["xcrun"], 15)] * _RUNTIME_RETRY_ATTEMPTS,
             _RUNTIME_RETRY_ATTEMPTS),
        ]:
            with self.subTest(responses=responses):
                with patch("ios_simulator_text_size.subprocess.run",
                           side_effect=responses) as run, \
                        patch("ios_simulator_text_size.time.sleep"):
                    with runtime_text_size_control(UUID) as control:
                        with self.assertRaises(urllib.error.HTTPError) as failure:
                            self.request_control(control, {"category": MAXIMUM_TEXT_SIZE})
                        self.assertEqual(failure.exception.code, 500)
                    self.assertEqual(run.call_count, expected_calls)

    def test_runtime_set_retries_transient_subprocess_errors(self):
        responses = [subprocess.CalledProcessError(1, ["xcrun"]), self.result(),
                     self.result("large")]
        with patch("ios_simulator_text_size.subprocess.run", side_effect=responses) as run, \
                patch("ios_simulator_text_size.time.sleep") as sleep:
            self.assertEqual(
                set_simulator_text_size(UUID, "large", attempts=_RUNTIME_RETRY_ATTEMPTS),
                "large")
            self.assertEqual(run.call_count, 3)
            self.assertEqual(sleep.call_args_list, [call(1.0)])

    def test_runtime_transient_set_failure_recovers_with_backoff(self):
        responses = [subprocess.TimeoutExpired(["xcrun"], 15), self.result(),
                     self.result(MAXIMUM_TEXT_SIZE)]
        with patch("ios_simulator_text_size.subprocess.run", side_effect=responses) as run, \
                patch("ios_simulator_text_size.time.sleep") as sleep:
            with runtime_text_size_control(UUID) as control:
                with self.request_control(control, {"category": MAXIMUM_TEXT_SIZE}) as response:
                    self.assertEqual(json.load(response), {"category": MAXIMUM_TEXT_SIZE})
            self.assertEqual(run.call_count, 3)
            self.assertEqual(sleep.call_args_list, [call(1.0)])

    def test_runtime_transient_read_failure_recovers_with_backoff(self):
        responses = [self.result(), subprocess.TimeoutExpired(["xcrun"], 15),
                     self.result(MAXIMUM_TEXT_SIZE)]
        with patch("ios_simulator_text_size.subprocess.run", side_effect=responses) as run, \
                patch("ios_simulator_text_size.time.sleep") as sleep:
            with runtime_text_size_control(UUID) as control:
                with self.request_control(control, {"category": MAXIMUM_TEXT_SIZE}) as response:
                    self.assertEqual(json.load(response), {"category": MAXIMUM_TEXT_SIZE})
            self.assertEqual(run.call_count, 3)
            self.assertEqual(sleep.call_args_list, [call(1.0)])

    def test_runtime_retry_exhaustion_is_bounded_logged_and_backed_off(self):
        responses = [subprocess.TimeoutExpired(["xcrun"], 15)] * _RUNTIME_RETRY_ATTEMPTS
        with patch("ios_simulator_text_size.subprocess.run", side_effect=responses) as run, \
                patch("ios_simulator_text_size.time.sleep") as sleep, \
                redirect_stdout(StringIO()) as logs:
            with runtime_text_size_control(UUID) as control:
                with self.assertRaises(urllib.error.HTTPError) as failure:
                    self.request_control(control, {"category": MAXIMUM_TEXT_SIZE})
                self.assertEqual(failure.exception.code, 500)
            self.assertEqual(run.call_count, _RUNTIME_RETRY_ATTEMPTS)
            self.assertEqual([item.args[0] for item in sleep.call_args_list], [1.0, 4.0])
            self.assertEqual(logs.getvalue().count("后重试"), _RUNTIME_RETRY_ATTEMPTS - 1)

    def test_runtime_change_and_ui_failure_restore_exact_original_size(self):
        responses = [self.result("extra-large"), self.result(), self.result(MAXIMUM_TEXT_SIZE),
                     self.result(), self.result("large"), self.result(), self.result("extra-large")]
        with patch("ios_simulator_text_size.subprocess.run", side_effect=responses) as run:
            with self.assertRaisesRegex(RuntimeError, "XCTest failed"):
                with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE):
                    with runtime_text_size_control(UUID) as control:
                        with self.request_control(control, {"category": "large"}) as response:
                            self.assertEqual(json.load(response), {"category": "large"})
                        raise RuntimeError("XCTest failed")
            self.assertEqual(run.call_count, 7)
            self.assertEqual(run.call_args_list[-2].args[0][-1], "extra-large")

    def test_runtime_control_drains_active_request_before_restoring(self):
        started, release, closing, restored = (threading.Event() for _ in range(4))
        failure = []

        def set_size(simulator, category, phase="运行时切换", *, timeout=15, attempts=1):
            self.assertEqual(simulator, UUID)
            self.assertEqual(timeout, 15 if phase == "运行时切换" else 120)
            self.assertEqual(
                attempts, _RUNTIME_RETRY_ATTEMPTS if phase == "运行时切换" else 1)
            if phase == "运行时切换":
                started.set()
                if not release.wait(5):
                    raise RuntimeError("Test did not release the in-flight request")
            elif phase == "恢复":
                self.assertTrue(release.is_set())
                restored.set()
            return category

        def request(control):
            try:
                with self.request_control(control, {"category": "large"}) as response:
                    self.assertEqual(json.load(response), {"category": "large"})
            except Exception as error:
                failure.append(error)

        def close(service):
            try:
                closing.set()
                service.__exit__(None, None, None)
            except Exception as error:
                failure.append(error)

        with patch("ios_simulator_text_size._read_text_size", return_value="extra-large"), \
                patch("ios_simulator_text_size.set_simulator_text_size", side_effect=set_size):
            with simulator_text_size(UUID, MAXIMUM_TEXT_SIZE):
                service = runtime_text_size_control(UUID)
                client = threading.Thread(target=request, args=(service.__enter__(),))
                client.start()
                self.assertTrue(started.wait(5))
                shutdown = threading.Thread(target=close, args=(service,))
                shutdown.start()
                try:
                    self.assertTrue(closing.wait(5))
                    shutdown.join(0.1)
                    self.assertTrue(shutdown.is_alive(), "Shutdown must drain the active request")
                    self.assertFalse(restored.is_set())
                finally:
                    release.set()
                    client.join(5)
                    shutdown.join(5)
                self.assertFalse(client.is_alive())
                self.assertFalse(shutdown.is_alive())
                self.assertEqual(failure, [])
            self.assertTrue(restored.is_set())


if __name__ == "__main__":
    unittest.main()
