"""Fail-closed route-throttle oracles and fault parameters; no Docker required."""
from __future__ import annotations

import copy
from email.utils import formatdate
import importlib.util
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock, patch

from scripts.docker_e2e import route_throttle as gate

ROOT = Path(__file__).resolve().parents[1]


def cycle(epoch=1):
    due = epoch * 100 + 40
    stamp = lambda seconds: f"2026-10-25T14:{seconds // 60:02}:{seconds % 60:02}+00:00"
    episode = {
        "episode_key": "episode", "binding_json": json.dumps({
            "agent_id": "qa", "root_message_id": "root",
            "owner": {"kind": "conversation", "interaction_id": "i"}}),
        "epoch": epoch, "revision": epoch, "state": "cooling",
        "total_requests": 2 * epoch, "request_count": 2, "candidate_cursor": 2,
        "failed_epochs": epoch, "not_before": stamp(due),
    }
    wire, ledger = [], []
    for n in range(1, epoch + 1):
        for side in ("a", "b"):
            at = 100 * n
            scope = json.dumps({"kind": "route_throttle", "key": side})
            ledger.append({
                "request_id": f"{n}-{side}", "episode_key": "episode", "epoch": n,
                "dispatched_at": stamp(at), "scopes_json": json.dumps([
                    {"scope": json.loads(scope), "was_probe": n > 1}]),
                "outcome": "rate_limited", "failed_scope_json": scope,
            })
            wire.append({"side": side, "status": 429, "retry_after": "30",
                         "at": gate.timestamp(stamp(at))})
    rows = {
        "provider_recovery_episodes": [episode], "provider_recovery_requests": ledger,
        "provider_queue_parking": [
            {"agent_id": "qa", "message_id": "fallback-message", "episode_key": "episode",
             "not_before": stamp(due)}],
        "provider_quota_incidents": [], "turn_records": [], "briefs": [],
        "provider_route_throttle_incidents": [
            {"identity_key": side, "last_model_ref": f"qa-{side}/gpt-5.4",
             "consecutive_failures": epoch, "server_not_before": stamp(100 * epoch + 30),
             "next_retry_at": stamp(100 * epoch + 30), "resolved_at": None}
            for side in ("a", "b")],
    }
    return rows, wire


class OracleTests(unittest.TestCase):
    def test_relative_and_absolute_wire_deadline(self):
        self.assertEqual(gate.header_deadline({"at": 100, "retry_after": "30"}), 130)
        self.assertEqual(gate.header_deadline(
            {"at": 100, "retry_after": formatdate(130, usegmt=True)}), 130)

    def test_two_real_failed_probe_cycles(self):
        first, _ = cycle()
        second, wire = cycle(2)
        gate.require_failed_cycle(second, "root", wire, first)
        third, wire = cycle(3)
        gate.require_failed_cycle(third, "root", wire, second)

    def test_cooling_observation_may_cross_deadline_but_never_dispatch_early(self):
        before, prior_wire = cycle()
        after, wire = cycle(2)
        due = gate.timestamp(before["provider_recovery_episodes"][0]["not_before"])
        gate.require_cooling_observation(before, before, "root", prior_wire,
                                         len(prior_wire), due - 1)
        gate.require_cooling_observation(before, after, "root", wire, len(prior_wire), due + 1)
        with self.assertRaisesRegex(AssertionError, "crash/restart changed"):
            gate.require_cooling_observation(before, after, "root", wire,
                                             len(prior_wire), due - 1)
        early_wire = copy.deepcopy(wire)
        early_wire[-1]["at"] = due - .001
        with self.assertRaisesRegex(AssertionError, "extra HTTP before"):
            gate.require_cooling_observation(before, after, "root", early_wire,
                                             len(prior_wire), due + 1)
        after["provider_recovery_requests"][-1]["dispatched_at"] = (
            before["provider_recovery_requests"][-1]["dispatched_at"])
        with self.assertRaisesRegex(AssertionError, "dispatch charge before"):
            gate.require_cooling_observation(before, after, "root", wire,
                                             len(prior_wire), due + 1)

    def test_wait_saves_the_matching_sample_without_resampling(self):
        run = gate.RouteThrottleRun.__new__(gate.RouteThrottleRun)
        run.deadline, run.evidence = 10, Path("evidence")
        rows, _ = cycle()
        run.last_snapshot = {"rows": rows}
        run.snapshot = Mock(return_value=rows)
        with patch.object(gate.time, "monotonic", return_value=1), \
                patch.object(gate, "write_json") as save:
            self.assertIs(run.wait(lambda sample: sample is rows, "matched"), rows)
        run.snapshot.assert_called_once_with()
        save.assert_called_once_with(Path("evidence/matched.json"),
                                     gate.redact_evidence(run.last_snapshot))

    def test_rejects_marker_only_completion(self):
        rows, _ = cycle()
        rows["debug"] = {"terminal_kind": "completed", "text": "QA_FALLBACK_COMPLETED"}
        with self.assertRaisesRegex(AssertionError, "completed source-bound"):
            gate.require_completion(rows, "root")

    def test_completion_requires_exact_structured_result_brief(self):
        rows, _ = cycle()
        rows["turn_records"] = [
            {"turn_id": "turn", "agent_id": "qa", "trigger_message_id": "root",
             "terminal_kind": "completed", "work_item_id": None, "payload_json": json.dumps({
                 "owner": {"kind": "conversation", "interaction_id": "i"}})}]
        brief = {"kind": "result", "agent_id": "qa", "turn_id": "turn",
                 "related_message_id": "root", "work_item_id": None}
        rows["briefs"] = [
            {"evidence_id": "brief", "kind": "result", "agent_id": "qa", "turn_id": "turn",
             "message_id": "root", "work_item_id": None, "payload_json": json.dumps(brief)}]
        self.assertEqual(gate.require_completion(rows, "root"), "turn")
        rows["briefs"][0]["message_id"] = "other-root"
        with self.assertRaisesRegex(AssertionError, "mismatch"):
            gate.require_completion(rows, "root")

    def test_duplicate_delivery_rejected(self):
        rows, _ = cycle()
        turn = {"turn_id": "turn", "agent_id": "qa", "trigger_message_id": "root",
                "terminal_kind": "completed"}
        rows["turn_records"] = [turn, dict(turn, turn_id="duplicate")]
        with self.assertRaisesRegex(AssertionError, "exactly one"):
            gate.require_completion(rows, "root")

    def test_fallback_consumption_keeps_original_owner(self):
        rows, _ = cycle()
        owner = {"kind": "conversation", "interaction_id": "i"}
        rows["turn_records"] = [{
            "turn_id": "fallback", "agent_id": "qa", "trigger_message_id": "recovery",
            "terminal_kind": "completed", "payload_json": json.dumps({
                "owner": owner, "input_message_ids": ["recovery", "root"]})}]
        brief = {"kind": "result", "agent_id": "qa", "turn_id": "fallback",
                 "related_message_id": "recovery", "work_item_id": None}
        rows["briefs"] = [{
            "evidence_id": "brief", "kind": "result", "agent_id": "qa", "turn_id": "fallback",
            "message_id": "recovery", "work_item_id": None, "payload_json": json.dumps(brief)}]
        self.assertTrue(gate.root_completed(rows, "root"))
        self.assertEqual(gate.require_completion(rows, "root"), "fallback")
        rows["turn_records"][0]["payload_json"] = json.dumps({
            "owner": {"kind": "conversation", "interaction_id": "wrong"},
            "input_message_ids": ["recovery", "root"]})
        with self.assertRaisesRegex(AssertionError, "root owner"):
            gate.require_completion(rows, "root")

    def test_restart_preserves_all_fences_and_remaining_budget(self):
        before, _ = cycle()
        gate.require_restart_preserved(before, copy.deepcopy(before), "root")
        for field, value in (("epoch", 2), ("total_requests", 0), ("candidate_cursor", 0),
                             ("not_before", "2026-10-25T14:00:00Z"), ("revision", 2)):
            with self.subTest(field=field):
                after = copy.deepcopy(before)
                after["provider_recovery_episodes"][0][field] = value
                with self.assertRaisesRegex(AssertionError, "episode"):
                    gate.require_restart_preserved(before, after, "root")

    def test_rejects_wire_not_durably_charged(self):
        rows, wire = cycle()
        with self.assertRaisesRegex(AssertionError, "wire requests"):
            gate.require_budget(rows, ["root"], wire + [wire[0]])

    def test_rejects_429_misclassified_as_non_recovery_response(self):
        rows, wire = cycle()
        rows["provider_recovery_requests"][0]["outcome"] = "non_recovery_response"
        with self.assertRaisesRegex(AssertionError, "RateLimited"):
            gate.require_budget(rows, ["root"], wire)

    def test_rejects_fabricated_quota_identity(self):
        rows, _ = cycle()
        rows["provider_quota_incidents"] = [{"identity_key": "invented"}]
        with self.assertRaisesRegex(AssertionError, "quota identity"):
            gate.require_no_identity(rows)

    def test_rejects_quota_dispatch_scope_even_without_incident(self):
        rows, _ = cycle()
        rows["provider_recovery_requests"][0]["scopes_json"] = json.dumps([
            {"scope": {"kind": "quota", "key": "invented"}, "was_probe": False}])
        with self.assertRaisesRegex(AssertionError, "fabricated"):
            gate.require_no_identity(rows)

    def test_rejects_resolved_failed_cycle_or_missing_parking(self):
        for mutation in ("resolved", "missing-parking", "short-header"):
            rows, wire = cycle()
            if mutation == "resolved":
                rows["provider_recovery_episodes"][0]["state"] = "resolved"
            elif mutation == "missing-parking":
                rows["provider_queue_parking"] = []
            else:
                rows["provider_route_throttle_incidents"][0]["server_not_before"] = (
                    "2026-10-25T14:00:00Z")
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                gate.require_failed_cycle(rows, "root", wire)

    def test_rejects_early_probe(self):
        before, _ = cycle()
        after, wire = cycle(2)
        after["provider_recovery_requests"][-1]["dispatched_at"] = "2026-10-25T14:00:00Z"
        with self.assertRaisesRegex(AssertionError, "before the previous durable deadline"):
            gate.require_failed_cycle(after, "root", wire, before)

    def test_rejects_duplicate_failed_scope_in_one_epoch(self):
        rows, wire = cycle()
        rows["provider_recovery_requests"][1]["failed_scope_json"] = (
            rows["provider_recovery_requests"][0]["failed_scope_json"])
        with self.assertRaisesRegex(AssertionError, "same failed scope"):
            gate.require_budget(rows, ["root"], wire)

    def test_rejects_retry_without_probe_admission(self):
        before, _ = cycle()
        after, wire = cycle(2)
        scope = json.loads(after["provider_recovery_requests"][-1]["scopes_json"])
        scope[0]["was_probe"] = False
        after["provider_recovery_requests"][-1]["scopes_json"] = json.dumps(scope)
        with self.assertRaisesRegex(AssertionError, "admitted route-throttle probe"):
            gate.require_failed_cycle(after, "root", wire, before)

    def test_rejects_epoch_budget_expansion(self):
        rows, wire = cycle()
        ledger = rows["provider_recovery_requests"]
        ledger.extend([dict(ledger[0], request_id="extra-1"),
                       dict(ledger[1], request_id="extra-2")])
        with self.assertRaisesRegex(AssertionError, "three dispatches"):
            gate.require_budget(rows, ["root"], wire * 2)

    def test_matrix_and_layered_coverage_are_explicit(self):
        self.assertEqual(len(gate.CASES), 4)
        self.assertIn("not covered", gate.LAYERED_COVERAGE["network_origin"])
        self.assertIn("not executed", gate.LAYERED_COVERAGE[
            "missing_malformed_zero_retry_after"]["layer"])


class RetryAfterFixtureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        sys.path.insert(0, str(ROOT / "tests/e2e/docker/openai_stub"))
        try:
            spec = importlib.util.spec_from_file_location(
                "route_throttle_fault_fixture", ROOT / "tests/e2e/docker/fault_proxy/server.py")
            cls.fixture = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(cls.fixture)
        finally:
            sys.path.pop(0)

    def test_default_unchanged_and_non_429_has_no_hint(self):
        with patch.object(self.fixture, "setting", side_effect=lambda key, default: default):
            self.assertEqual(self.fixture.retry_after_headers("a", 429), {"Retry-After": "1"})
            self.assertEqual(self.fixture.retry_after_headers("a", 400), {})

    def test_normal_proxy_timeout_default_and_override(self):
        for value, timeout, select_timeout in ((None, 10, 30), (45, 45, 45)):
            with self.subTest(value=value):
                proxy = self.fixture.Proxy.__new__(self.fixture.Proxy)
                proxy.server, proxy.request = Mock(side="a"), Mock()
                configured = lambda key, default: (
                    value if key == "UPSTREAM_TIMEOUT" and value is not None else default)
                with patch.object(self.fixture, "setting", side_effect=configured), \
                        patch.object(self.fixture, "record"), \
                        patch.object(self.fixture.socket, "create_connection") as connect, \
                        patch.object(self.fixture.select, "select",
                                     return_value=([], [], [])) as select:
                    proxy.handle()
                    connect.assert_called_once_with(("mock", 8080), timeout=timeout)
                    self.assertEqual(select.call_args.args[3], select_timeout)

    def test_missing_invalid_relative_and_absolute(self):
        for value, expected in ((None, {}), ("invalid", {"Retry-After": "invalid"}),
                                ({"seconds": 30, "format": "seconds"}, {"Retry-After": "30"}),
                                ({"seconds": 30, "format": "http-date"},
                                 {"Retry-After": formatdate(130, usegmt=True)})):
            with self.subTest(value=value), patch.object(
                    self.fixture, "setting", return_value=value), patch.object(
                    self.fixture.time, "time", return_value=100):
                self.assertEqual(self.fixture.retry_after_headers("a", 429), expected)


if __name__ == "__main__":
    unittest.main()
