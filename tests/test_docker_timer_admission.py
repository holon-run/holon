"""Deterministic transcript and evidence-oracle tests; no Docker required."""
from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from scripts.docker_e2e import timer_admission as timer
from tests.test_docker_e2e_runner import ROOT, runner, stub, stub_batch_request


WID = "work_0123456789abcde"
TID = "timer_0123456789abcde"
MARKER = "SCHEDULER-TIMER-ADMISSION-abcd"


def tick(message_id: str = "tick-1", **overrides: object) -> dict:
    return {
        "id": message_id, "kind": "timer_tick",
        "authority_class": "runtime_instruction",
        "origin": {"kind": "timer", "timer_id": TID},
        "work_item_id": WID, **overrides,
    }


def fixtures() -> tuple[dict, dict]:
    wait = {
        "wait_condition_id": "wait-1", "agent_id": "main", "work_item_id": WID,
        "kind": "timer", "subject_ref": TID, "status": "active",
        "trigger_message_id": None,
    }
    before = {
        "wait_conditions": [wait],
        "work_items": [{"work_item_id": WID, "revision": 6}],
        "execution_protocol_work_items": [{
            "agent_id": "main", "work_item_id": WID, "generation": 2,
        }],
    }
    attempt = {
        "source_message_id": "tick-1",
        "source": {"identity": {
            "kind": "triggered_wait", "wait_id": "wait-1",
            "trigger_message_id": "tick-1",
        }, "generation": 17},
        "binding": {"kind": "work_item", "work_item_id": WID},
        "turn_id": "turn-1",
        "admitted_fences": {
            "work_item_source_revision": 7, "work_item_generation": 2,
            "source_revision": 17, "agent_control_revision": 1,
            "host_registry_revision": 1,
        },
    }
    after = {
        "wait_conditions": [{**wait, "status": "resolved", "trigger_message_id": "tick-1"}],
        "messages": [{"message_id": "tick-1", "payload_json": json.dumps({
            "kind": "timer_tick", "origin": {"kind": "timer", "timer_id": TID},
            "work_item_id": WID, "message_seq": 17,
        })}],
        "execution_protocol_attempts": [{
            "agent_id": "main", "attempt_id": "attempt-1",
            "lifecycle_state": "settled", "terminal_outcome_id": "outcome-1",
            "payload_json": json.dumps(attempt),
        }],
        "execution_protocol_outcomes": [{
            "outcome_id": "outcome-1",
            "payload_json": json.dumps({"attempt_id": "attempt-1"}),
        }],
        "model_requests": [{
            "agent_id": "main", "work_item_id": WID, "turn_id": "turn-1",
            "evidence_id": "request-1",
        }],
        "timer_wakes": [{
            "timer_id": TID, "message_id": "tick-1", "status": "incorporated",
            "fire_count": 1,
        }],
    }
    return before, after


class TimerAdmissionTests(unittest.TestCase):
    def test_manifest_is_stub_only_and_does_not_expand_required_profile(self) -> None:
        manifest = json.loads((ROOT / "tests/e2e/docker/manifest.json").read_text())
        runner.validate_manifest(manifest)
        selected = runner.select_cases(
            manifest, requested=["scheduler-timer-admission-recovery"],
            suite="core", tags=[],
        )
        self.assertEqual(len(selected), 1)
        self.assertEqual(selected[0]["provider_mode"], "stub")
        self.assertNotIn(
            selected[0]["id"], manifest["profiles"]["scheduler-required"]["case_ids"],
        )

    def test_first_online_timer_is_in_required_profile(self) -> None:
        manifest = json.loads((ROOT / "tests/e2e/docker/manifest.json").read_text())
        profile = runner.resolve_profile(manifest, "scheduler-required")
        selected = runner.select_cases(
            manifest, requested=profile["case_ids"], suite="core", tags=[],
        )
        online = next(
            case for case in selected if case["id"] == "scheduler-timer-online-admission"
        )
        self.assertEqual(online["provider_mode"], "stub")
        self.assertIn(online["id"], profile["required_coverage_ids"])
        with patch.object(timer, "run_case") as run:
            runner.CASE_RUNNERS[online["id"]]("harness", online)
        run.assert_called_once_with(
            "harness", online, restart_before_first_dispatch=False,
        )

    def test_online_orchestration_never_restarts_and_rejects_changed_daemon(self) -> None:
        # This checks the harness branch, not a real daemon execution.
        for restarted in (False, True):
            with self.subTest(restarted=restarted), tempfile.TemporaryDirectory() as tmp:
                evidence = Path(tmp)
                status = {"timer_dispatch_requests": [
                    {"phase": 4, "message_id": "tick-1"},
                    {"phase": 7, "message_id": "tick-2"},
                ]}
                (evidence / "stub-status.json").write_text(json.dumps(status))
                pending = [{"state": "pending"}, {"state": "pending"}]
                harness = Mock(
                    provider_mode="stub", agent_id="main", evidence=evidence,
                    container="daemon-under-test",
                )
                harness.assert_stub_complete = Mock()
                harness.wait_work_item_scheduling_state.return_value = {
                    "id": WID, "blocked_by_wait_id": "wait-1", "todo_list": pending,
                }
                harness.request.side_effect = [[{
                    "id": TID, "agent_id": "main", "repeat": True,
                    "interval_ms": 30000, "fire_count": 0,
                }], {"status": "cancelled"}]
                harness.wait_work_item.return_value = {
                    "id": WID, "result_brief_id": "brief-1",
                    "todo_list": [{"state": "completed"}, {"state": "completed"}],
                }
                harness.brief.return_value = {"body": f"{MARKER}-COMPLETE"}
                harness.docker.side_effect = [
                    SimpleNamespace(stdout="boot-1"),
                    SimpleNamespace(stdout="boot-2" if restarted else "boot-1"),
                ]
                with (
                    patch.object(timer.secrets, "token_hex", return_value="abcd"),
                    patch.object(timer, "snapshot", side_effect=[
                        {"timers": [{"timer_id": TID, "fire_count": 0}],
                         "work_items": [{"agent_id": "main"}]},
                        {}, {"execution_protocol_attempts": []},
                    ]) as snapshots,
                    patch.object(timer, "owned_wait"),
                    patch.object(timer, "wait_rearmed", return_value={
                        "id": WID, "blocked_by_wait_id": "wait-2",
                    }),
                    patch.object(timer, "require_dispatch", side_effect=[
                        {"message_id": "tick-1"}, {"message_id": "tick-2"},
                    ]) as dispatches,
                    patch.object(timer, "provider_requests", return_value=[1]),
                ):
                    case = {"id": "scheduler-timer-online-admission",
                            "phases": [{"prompt": "{marker}"}]}
                    if restarted:
                        with self.assertRaisesRegex(AssertionError, "restarted the daemon"):
                            timer.run_case(harness, case, restart_before_first_dispatch=False)
                        self.assertFalse((evidence / "timer-admission-oracle.json").exists())
                    else:
                        timer.run_case(harness, case, restart_before_first_dispatch=False)
                        oracle = json.loads(
                            (evidence / "timer-admission-oracle.json").read_text()
                        )
                        self.assertFalse(oracle["restart_before_first_dispatch"])
                        self.assertIsNone(oracle["restart_after_wait_id"])
                        self.assertEqual(oracle["online_daemon_started_at"], "boot-1")
                    harness.start.assert_called_once_with()
                    harness.stop.assert_not_called()
                    self.assertEqual(dispatches.call_count, 2)
                    self.assertTrue(all(not call.kwargs for call in snapshots.call_args_list))

    def test_complete_transcript_has_two_real_ticks_and_cancels_timer(self) -> None:
        scenario = stub.Scenario("scheduler-timer-admission")
        tools = []
        for phase in range(11):
            provenance = tick("tick-2" if phase >= 7 else "tick-1") if phase >= 4 else {}
            body = f"{MARKER} {WID} " + (TID if phase >= 3 else "")
            status, response = scenario.consume(stub_batch_request((provenance, body)))
            self.assertEqual(status, 200, response)
            call = next(item for item in response["output"] if item["type"] == "function_call")
            tools.append(call["name"])
            args = json.loads(call["arguments"])
            if phase in {3, 6}:
                self.assertEqual(args["resource"], TID)
                self.assertEqual(args["wake"], "timer")
                self.assertEqual(args["delivery"], "silent")
            if phase == 5:
                self.assertEqual(
                    [todo["state"] for todo in args["todo_list"]],
                    ["completed", "pending"],
                )
            if phase == 8:
                self.assertTrue(all(todo["state"] == "completed" for todo in args["todo_list"]))
        self.assertEqual(tools, [
            "CreateWorkItem", "PickWorkItem", "CreateTimer", "WaitFor", "GetWorkItem",
            "UpdateWorkItem", "WaitFor", "GetWorkItem", "UpdateWorkItem",
            "CancelTimer", "CompleteWorkItem",
        ])
        self.assertTrue(scenario.status()["complete"])
        self.assertEqual(scenario.status()["extra_requests"], 0)
        self.assertEqual(
            [entry["message_id"] for entry in scenario.status()["timer_dispatch_requests"]],
            ["tick-1", "tick-2"],
        )
        scenario.consume(stub_batch_request((tick("tick-3"), f"{MARKER} {WID} {TID}")))
        self.assertEqual(scenario.status()["extra_requests"], 1)
        self.assertFalse(scenario.status()["complete"])

    def test_stub_rejects_non_timer_wrong_owner_and_replayed_ticks(self) -> None:
        for provenance in [
            {}, tick(kind="operator_prompt"), tick(authority_class="external"),
            tick(work_item_id="other"), tick(origin={"kind": "system"}),
            tick(origin={"kind": "timer", "timer_id": "other"}),
        ]:
            with self.subTest(provenance=provenance):
                scenario = stub.Scenario("scheduler-timer-admission")
                scenario.phase = 4
                status, _ = scenario.consume(
                    stub_batch_request((provenance, f"{MARKER} {WID} {TID}"))
                )
                self.assertEqual(status, 409)
                self.assertEqual(scenario.phase, 4)
        scenario = stub.Scenario("scheduler-timer-admission")
        scenario.phase = 7
        scenario.timer_dispatch_requests = [{"message_id": "tick-1"}]
        status, _ = scenario.consume(stub_batch_request((tick(), f"{MARKER} {WID} {TID}")))
        self.assertEqual(status, 409)

    def check_dispatch(self, before: dict, after: dict) -> dict:
        return timer.require_dispatch(
            before, after, agent_id="main", work_item_id=WID, timer_id=TID, wait_id="wait-1",
        )

    def test_oracle_links_exact_wait_attempt_fences_and_provider_request(self) -> None:
        before, after = fixtures()
        result = self.check_dispatch(before, after)
        self.assertEqual(result["attempt_id"], "attempt-1")
        self.assertEqual(result["provider_request_ids"], ["request-1"])

    def test_oracle_rejects_fire_count_without_dispatch_or_business_evidence(self) -> None:
        for table in [
            "execution_protocol_attempts", "execution_protocol_outcomes",
            "timer_wakes",
        ]:
            with self.subTest(table=table):
                before, after = fixtures()
                after[table] = []
                with self.assertRaises(AssertionError):
                    self.check_dispatch(before, after)

    def test_provider_transport_oracle_checks_current_input_owner(self) -> None:
        request = stub_batch_request((tick(), MARKER))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "requests.jsonl"
            path.write_text(json.dumps(request) + "\n")
            self.assertEqual(timer.provider_requests(path, "tick-1", WID), [1])
            with self.assertRaisesRegex(AssertionError, "never reached"):
                timer.provider_requests(path, "tick-other", WID)
            with self.assertRaisesRegex(AssertionError, "never reached"):
                timer.provider_requests(path, "tick-1", "other-owner")

    def test_oracle_rejects_stale_fences_wrong_binding_and_duplicate_admission(self) -> None:
        for field, value in [
            ("work_item_source_revision", 6), ("work_item_generation", 1),
            ("source_revision", 16),
        ]:
            with self.subTest(field=field):
                before, after = fixtures()
                attempt = json.loads(after["execution_protocol_attempts"][0]["payload_json"])
                attempt["admitted_fences"][field] = value
                after["execution_protocol_attempts"][0]["payload_json"] = json.dumps(attempt)
                with self.assertRaisesRegex(AssertionError, "fences"):
                    self.check_dispatch(before, after)
        before, after = fixtures()
        attempt = json.loads(after["execution_protocol_attempts"][0]["payload_json"])
        attempt["binding"] = {"kind": "agent_lifecycle", "agent_id": "main"}
        after["execution_protocol_attempts"][0]["payload_json"] = json.dumps(attempt)
        with self.assertRaisesRegex(AssertionError, "binding"):
            self.check_dispatch(before, after)
        before, after = fixtures()
        after["execution_protocol_attempts"] *= 2
        with self.assertRaisesRegex(AssertionError, "exactly once"):
            self.check_dispatch(before, after)

    def test_oracle_rejects_unresolved_wait_and_wrong_owner(self) -> None:
        before, after = fixtures()
        after["wait_conditions"][0]["work_item_id"] = "other"
        with self.assertRaisesRegex(AssertionError, "ownership"):
            self.check_dispatch(before, after)
        before, after = fixtures()
        after["wait_conditions"][0]["status"] = "active"
        with self.assertRaisesRegex(AssertionError, "status"):
            self.check_dispatch(before, after)


if __name__ == "__main__":
    unittest.main()
