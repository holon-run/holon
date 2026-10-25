#!/usr/bin/env python3
"""Safety and evidence gates for isolated Docker fault acceptance."""
import sys
import json
import subprocess
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from docker_e2e.fallback_storm import (
    FaultRun, evaluate, main, persistent_budget_evidence, request_bound, require_build,
)


def completed():
    return [
        {"type": "turn_terminal", "payload": {
            "kind": "completed", "last_assistant_message": "QA_FALLBACK_COMPLETED"}},
        {"type": "brief_created", "payload": {"kind": "result"}},
    ]


class FaultEvidenceTests(unittest.TestCase):
    def test_control_requires_real_http_and_single_delivery(self):
        self.assertEqual(evaluate("C00", completed(),
                                  [{"side": "a", "status": 200}], [{"side": "a"}]),
                         "verified-w1-isolated")

    def test_empty_is_not_pass(self):
        with self.assertRaises(AssertionError):
            evaluate("C00", completed(), [], [])

    def test_duplicate_delivery_is_not_pass(self):
        with self.assertRaises(AssertionError):
            evaluate("C00", completed() * 2, [{"side": "a", "status": 200}], [{}])

    def test_failure_requires_fault_and_backup_success(self):
        proxy = [{"side": "a"}, {"side": "b"}]
        self.assertEqual(evaluate("N01", completed(), [{"side": "b", "status": 200}],
                                  proxy), "verified-w1-isolated")
        with self.assertRaises(AssertionError):
            evaluate("N01", completed(), [{"side": "a", "status": 200}], proxy)

    def test_parking_cannot_be_inferred_from_silence(self):
        self.assertEqual(evaluate("N02", [], [], [{"side": "a"}] * 3 +
                                  [{"side": "b"}] * 3), "needs-parking-review")

    def test_shared_guard_has_per_epoch_bound(self):
        self.assertEqual(request_bound("H01-429"), 3)
        self.assertEqual(request_bound("N01"), 3)
        self.assertEqual(request_bound("N02", 0), 3)
        self.assertEqual(request_bound("N02", 10), 6)
        with self.assertRaises(AssertionError):
            evaluate("N01", completed(), [{"side": "b", "status": 200}],
                     [{"side": "a"}] * 4)

    def test_failure_brief_with_marker_is_not_completion(self):
        events = [{"type": "turn_terminal", "payload": {
            "kind": "aborted", "last_assistant_message": "QA_FALLBACK_COMPLETED"}},
            {"type": "brief_created", "payload": {
                "kind": "failure", "text": "QA_FALLBACK_COMPLETED"}}]
        with self.assertRaises(AssertionError):
            evaluate("N01", events, [{"side": "b", "status": 200}],
                     [{"side": "a"}, {"side": "b"}])

    def test_control_http_error_is_not_pass(self):
        with self.assertRaises(AssertionError):
            evaluate("C00", completed(), [{"side": "a", "status": 429}], [{"side": "a"}])

    def test_missing_result_brief_is_not_pass(self):
        with self.assertRaises(AssertionError):
            evaluate("C00", completed()[:1], [{"side": "a", "status": 200}], [{"side": "a"}])

    def test_dual_rst_cannot_complete_or_reach_http(self):
        proxy = [{"side": "a"}, {"side": "b"}]
        for events, requests in ((completed(), []), ([], [{"side": "a", "status": 200}])):
            with self.subTest(events=events, requests=requests), self.assertRaises(AssertionError):
                evaluate("N02", events, requests, proxy)

    def test_binary_revision_is_required_and_checked(self):
        require_build("holon 0.54.1 (d841d82f)", "d841d82f0c6c82fdf28fe168752c03657bf63d6b")
        for version, expected in (("holon dev", "d841d82f"),
                                  ("holon 0.54.1 (d841d82f-dirty)", "d841d82f"),
                                  ("holon 0.54.1 (d841d82f)", "339b87c5"),
                                  ("holon 0.54.1 (d841d82f)", "d841")):
            with self.subTest(version=version, expected=expected), self.assertRaises(AssertionError):
                require_build(version, expected)


class PersistentEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.evidence = Path(self.temp.name)
        self.snapshot = {
            "rows": {
                "provider_recovery_episodes": [{
                    "binding_json": json.dumps({"root_message_id": "root"}),
                    "episode_key": "episode", "total_requests": 1, "epoch": 0,
                    "state": "resolved", "not_before": None,
                }],
                "provider_recovery_requests": [{
                    "episode_key": "episode", "request_id": "request-1", "epoch": 0,
                }],
                "provider_queue_parking": [],
            },
            "truncated_tables": [],
        }
        self.events = {"events": [{"type": "turn_started"}], "has_newer": False}
        self.write("prompt.json", {"message_id": "root"})
        self.write("proxy-tcp.json", [{"side": "a"}])

    def write(self, name, value):
        (self.evidence / name).write_text(json.dumps(value))

    def analyze(self):
        self.write("database.json", self.snapshot)
        self.write("events.json", self.events)
        return persistent_budget_evidence(self.evidence)

    def test_canonical_root_and_dispatch_join(self):
        self.assertEqual(self.analyze()["root_and_budget_invariants"], "pass")

    def test_missing_or_duplicate_root_is_not_pass(self):
        episodes = self.snapshot["rows"]["provider_recovery_episodes"]
        for replacement in ([], episodes * 2):
            with self.subTest(replacement=replacement), self.assertRaises(AssertionError):
                self.snapshot["rows"]["provider_recovery_episodes"] = replacement
                self.analyze()

    def test_duplicate_request_id_is_not_pass(self):
        self.snapshot["rows"]["provider_recovery_requests"] *= 2
        with self.assertRaisesRegex(AssertionError, "duplicate"):
            self.analyze()

    def test_epoch_budget_is_not_reset_by_turns(self):
        self.snapshot["rows"]["provider_recovery_requests"] = [
            {"episode_key": "episode", "request_id": f"request-{i}", "epoch": 0}
            for i in range(4)]
        with self.assertRaisesRegex(AssertionError, "epoch budget"):
            self.analyze()

    def test_wire_and_durable_counts_must_agree(self):
        self.write("proxy-tcp.json", [{"side": "a"}] * 2)
        with self.assertRaisesRegex(AssertionError, "wire"):
            self.analyze()

    def test_durable_total_must_agree(self):
        self.snapshot["rows"]["provider_recovery_episodes"][0]["total_requests"] = 2
        with self.assertRaisesRegex(AssertionError, "durable total"):
            self.analyze()

    def test_empty_model_turn_is_not_pass(self):
        self.events["events"].append({"type": "turn_started"})
        with self.assertRaisesRegex(AssertionError, "empty model turn"):
            self.analyze()

    def test_truncated_evidence_is_not_pass(self):
        self.events["has_newer"] = True
        with self.assertRaisesRegex(AssertionError, "event capture"):
            self.analyze()
        self.events["has_newer"] = False
        self.snapshot["truncated_tables"] = ["provider_recovery_requests"]
        with self.assertRaisesRegex(AssertionError, "database capture"):
            self.analyze()


class RunnerLifecycleTests(unittest.TestCase):
    def test_cleanup_attempts_remaining_resources_after_error(self):
        with tempfile.TemporaryDirectory() as temp:
            run = FaultRun(Namespace(evidence_dir=Path(temp)), "C00")
            run.containers = [run.mock, run.daemon]
            run.networks = ["owned-network"]

            def docker(*args, **kwargs):
                if args[0] == "logs":
                    raise subprocess.TimeoutExpired("docker logs", 30)
                return subprocess.CompletedProcess(args, 0, "", "")

            run.docker = Mock(side_effect=docker)
            self.assertEqual(len(run.cleanup()), 2)
            run.docker.assert_any_call("rm", "-f", run.mock, check=False)
            run.docker.assert_any_call("network", "rm", "owned-network", check=False)
            run.docker.assert_any_call("volume", "rm", run.volume, check=False)

    def run_main(self, statuses, *, cleanup_errors=None, prompt=False):
        with tempfile.TemporaryDirectory() as temp:
            evidence = Path(temp) / "case"
            evidence.mkdir()
            if prompt:
                (evidence / "prompt.json").write_text("{}")
            runs = []
            for status in statuses:
                run = Mock(evidence=evidence)
                run.run.return_value = {"status": status}
                run.cleanup.return_value = cleanup_errors or []
                runs.append(run)
            with patch("docker_e2e.fallback_storm.FaultRun", side_effect=runs), \
                 patch("docker_e2e.fallback_storm.command",
                       return_value=subprocess.CompletedProcess([], 0, "sha256:pinned\n", "")), \
                 patch("builtins.print"):
                code = main(["--image", "local", "--python-image", "python-local",
                             "--expected-revision", "d841d82f", "--evidence-dir", temp,
                             *[arg for _ in statuses for arg in ("--case", "C00")]])
            return code, json.loads((Path(temp) / "summary.json").read_text())

    def test_nonpassing_case_is_nonzero_but_does_not_skip_later_cases(self):
        for status in ("needs-parking-review", "failed-or-blocked"):
            with self.subTest(status=status):
                code, results = self.run_main([status, "verified-w1-isolated"])
                self.assertEqual(code, 1)
                self.assertEqual(len(results), 2)

    def test_missing_database_cannot_pass(self):
        code, results = self.run_main(["verified-w1-isolated"], prompt=True)
        self.assertEqual(code, 1)
        self.assertIn("budget_error", results[0])

    def test_cleanup_error_cannot_pass(self):
        code, results = self.run_main(["verified-w1-isolated"],
                                      cleanup_errors=[{"error": "volume busy"}])
        self.assertEqual(code, 1)
        self.assertIn("cleanup_errors", results[0])


if __name__ == "__main__":
    unittest.main()
