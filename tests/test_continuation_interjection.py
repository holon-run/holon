#!/usr/bin/env python3
"""Evidence gates for the continuation/interjection isolated acceptance cases."""
import json
import copy
import importlib.util
import io
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from docker_e2e.continuation_interjection import (  # noqa: E402
    evaluate_interjection, evaluate_recovery, evaluate_tool_continuation, main,
)

FIXTURE_PATH = Path(__file__).parent / "e2e/docker/fault_proxy/server.py"
SPEC = importlib.util.spec_from_file_location("continuation_fault_fixture", FIXTURE_PATH)
FIXTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURE)


def tool_events():
    return [
        {"type": "provider_candidate_reselected", "payload": {
            "dispatch_occurred": False, "predicate": "scope_cooling",
            "model_ref": "qa-b/gpt-5.4"}},
        {"type": "turn_terminal", "payload": {
            "kind": "completed", "last_assistant_message": "QA_TOOL_CONTINUED"}},
        {"type": "brief_created", "payload": {"kind": "result"}},
    ]


def tool_http():
    return [
        {"side": "a", "response": "tool_call", "tool_output": False},
        {"side": "b", "response": "continuation", "tool_output": True},
    ]


def tool_snapshot():
    return {"rows": {"tool_executions": [{
        "kind": "tool_execution",
        "payload_json": json.dumps({
            "input": {"cmd": "while [ ! -f /var/lib/holon/qa-3753-gate ]"},
            "status": "success"}),
    }]}}


def interjection_events():
    return [
        {"type": "queue_entry_claimed", "payload": {"message_id": "m-park"},
         "ts": "2026-10-26T00:00:00Z"},
        {"type": "queue_entry_settled", "payload": {
            "message_id": "m-park", "status": "interrupted",
            "reason": "provider_dispatch_parked"}},
        {"type": "operator_interjection_admitted", "payload": {
            "message_id": "m-fresh", "boundary": "before_tool_execution"}},
        {"type": "turn_terminal", "ts": "2026-10-26T00:00:30Z", "payload": {
            "kind": "completed", "last_assistant_message": "QA_TIMER_DONE"}},
        {"type": "brief_created", "payload": {"kind": "result"}},
    ]


def interjection_snapshot(parked_status="interrupted", fresh_status="interjected"):
    return {"rows": {
        "messages": [
            {"message_id": "m-park", "preview": "", "payload_json": '{"text": "QA_PARK"}'},
            {"message_id": "m-fresh", "preview": "", "payload_json": '{"text": "QA_FRESH"}'},
        ],
        "queue_entries_current": [
            {"message_id": "m-park", "status": parked_status,
             "payload_json": json.dumps({"text": "QA_PARK"})},
            {"message_id": "m-fresh", "status": fresh_status,
             "payload_json": json.dumps({"text": "QA_FRESH"})},
        ],
        "provider_queue_parking": [{
            "message_id": "m-park", "agent_id": "qa", "reason": "chain_cooling",
            "not_before": "2026-10-26T00:02:00Z", "episode_key": "episode-1"}],
        "scheduler_activation_inputs": [{
            "message_id": "m-park", "owner_kind": "agent_lifecycle", "owner_id": "qa",
            "activation_id": "original-activation"}],
        "turn_records": [{"payload_json": json.dumps({
            "turn_id": "source-turn", "owner": {"kind": "agent_lifecycle", "agent_id": "qa"},
        })}],
    }}


def interjection_http():
    return [{"marker": "QA_TIMER", "response": "tool_call", "response_held": True}]


def evaluate_timer(events=None, http=None, snapshot=None, before=None, boundary=None):
    return evaluate_interjection(
        interjection_events() if events is None else events,
        interjection_http() if http is None else http,
        interjection_snapshot() if snapshot is None else snapshot,
        interjection_snapshot() if before is None else before,
        interjection_snapshot(fresh_status="queued") if boundary is None else boundary)


def recovery_events():
    return interjection_events() + [
        {"type": "queue_entry_claimed", "payload": {"message_id": "m-park"},
         "ts": "2026-10-26T00:02:01Z"},
        {"type": "turn_replay_started", "payload": {
            "message_id": "m-park", "source_turn_id": "source-turn",
            "replay_turn_id": "replay-turn", "reason": "interrupted_queue_claim_reentry"}},
        {"type": "queue_entry_settled", "payload": {
            "message_id": "m-park", "status": "processed"}},
        {"type": "turn_terminal", "payload": {
            "kind": "completed", "last_assistant_message": "QA_PARK_DONE"}},
        {"type": "brief_created", "payload": {"kind": "result"}},
    ]


def recovery_snapshot():
    snapshot = interjection_snapshot(parked_status="processed")
    snapshot["rows"]["provider_queue_parking"] = []
    snapshot["rows"]["turn_records"].append({"payload_json": json.dumps({
        "turn_id": "replay-turn", "owner": {"kind": "agent_lifecycle", "agent_id": "qa"},
        "replay": {"reason": "interrupted_queue_claim_reentry"},
    })})
    return snapshot


class ToolContinuationEvidenceTests(unittest.TestCase):
    def test_independent_b_continuation_is_verified(self):
        self.assertEqual(
            evaluate_tool_continuation(tool_events(), tool_http(), tool_snapshot()),
            "verified-t3753-isolated")

    def test_cooled_a_must_not_receive_tool_history(self):
        http = tool_http() + [{"side": "a", "tool_output": True}]
        with self.assertRaises(AssertionError):
            evaluate_tool_continuation(tool_events(), http, tool_snapshot())

    def test_reselection_must_be_local_and_bounded(self):
        events = tool_events()
        events[0]["payload"]["dispatch_occurred"] = True
        with self.assertRaises(AssertionError):
            evaluate_tool_continuation(events, tool_http(), tool_snapshot())

    def test_tool_must_execute_once(self):
        snapshot = tool_snapshot()
        snapshot["rows"]["tool_executions"].append(snapshot["rows"]["tool_executions"][0])
        with self.assertRaises(AssertionError):
            evaluate_tool_continuation(tool_events(), tool_http(), snapshot)


    def test_success_text_does_not_mask_failed_tool(self):
        snapshot = tool_snapshot()
        record = json.loads(snapshot["rows"]["tool_executions"][0]["payload_json"])
        record.update(status="error", summary="not completed successfully")
        snapshot["rows"]["tool_executions"][0]["payload_json"] = json.dumps(record)
        with self.assertRaises(AssertionError):
            evaluate_tool_continuation(tool_events(), tool_http(), snapshot)


class InterjectionEvidenceTests(unittest.TestCase):
    def test_retained_source_does_not_starve_fresh_input(self):
        self.assertEqual(evaluate_timer(), "verified-i3754-predeadline")

    def test_interrupted_source_must_not_be_interjected(self):
        with self.assertRaises(AssertionError):
            evaluate_timer(snapshot=interjection_snapshot(parked_status="interjected"))

    def test_aborted_timer_turn_fails(self):
        events = interjection_events() + [{
            "type": "turn_terminal", "payload": {"kind": "aborted", "reason": "runtime_error"}}]
        with self.assertRaises(AssertionError):
            evaluate_timer(events=events)

    def test_single_delivery(self):
        events = interjection_events() + [{"type": "brief_created",
                                           "payload": {"kind": "result"}}]
        with self.assertRaises(AssertionError):
            evaluate_timer(events=events)


    def test_wrong_boundary_is_not_accepted(self):
        events = interjection_events()
        events[2]["payload"]["boundary"] = "before_provider_dispatch"
        with self.assertRaises(AssertionError):
            evaluate_timer(events=events)

    def test_timer_must_finish_before_original_deadline(self):
        events = interjection_events()
        events[3]["ts"] = "2026-10-26T00:02:01Z"
        with self.assertRaises(AssertionError):
            evaluate_timer(events=events)

    def test_real_provider_parking_settlement_is_required(self):
        with self.assertRaises(AssertionError):
            evaluate_timer(events=interjection_events()[2:])

    def test_both_inputs_must_exist_before_tool_execution(self):
        with self.assertRaises(AssertionError):
            evaluate_timer(boundary=interjection_snapshot())

    def test_timer_must_not_mutate_original_evidence(self):
        for table, field, value in (
            ("provider_queue_parking", "not_before", "2026-10-26T00:03:00Z"),
            ("scheduler_activation_inputs", "owner_id", "different-owner"),
            ("messages", "payload_json", '{"text": "changed"}'),
        ):
            snapshot = interjection_snapshot()
            snapshot["rows"][table][0][field] = value
            with self.subTest(table=table), self.assertRaises(AssertionError):
                evaluate_timer(snapshot=snapshot)

    def test_response_must_be_held_before_tool_execution(self):
        with self.assertRaises(AssertionError):
            evaluate_timer(http=[{"marker": "QA_TIMER", "response": "tool_call"}])

    def test_truncated_evidence_cannot_pass(self):
        snapshot = interjection_snapshot()
        snapshot["truncated_tables"] = ["scheduler_activation_inputs"]
        with self.assertRaises(AssertionError):
            evaluate_timer(snapshot=snapshot)


class RecoveryEvidenceTests(unittest.TestCase):
    def test_deadline_reentry_preserves_owner_and_single_consumption(self):
        self.assertEqual(evaluate_recovery(
            recovery_events(), recovery_snapshot(), interjection_snapshot()),
            "verified-i3754-isolated")

    def test_no_early_or_duplicate_reclaim(self):
        for timestamp, duplicate in (("2026-10-26T00:01:59Z", False),
                                     ("2026-10-26T00:02:01Z", True)):
            events = recovery_events()
            events[5]["ts"] = timestamp
            if duplicate:
                events.append(copy.deepcopy(events[5]))
            with self.subTest(timestamp=timestamp), self.assertRaises(AssertionError):
                evaluate_recovery(events, recovery_snapshot(), interjection_snapshot())

    def test_processed_is_once_not_merely_final_status(self):
        events = recovery_events()
        events.append(copy.deepcopy(events[7]))
        with self.assertRaises(AssertionError):
            evaluate_recovery(events, recovery_snapshot(), interjection_snapshot())

    def test_owner_and_replay_provenance_cannot_drift(self):
        for field, value in (("owner", {"kind": "work_item", "work_item_id": "other"}),
                             ("replay", {"reason": "other"})):
            snapshot = recovery_snapshot()
            turn = json.loads(snapshot["rows"]["turn_records"][1]["payload_json"])
            turn[field] = value
            snapshot["rows"]["turn_records"][1]["payload_json"] = json.dumps(turn)
            with self.subTest(field=field), self.assertRaises(AssertionError):
                evaluate_recovery(recovery_events(), snapshot, interjection_snapshot())


class FixtureAndEntryTests(unittest.TestCase):
    def request(self, body, settings):
        handler = object.__new__(FIXTURE.Mock)
        raw = json.dumps({"input": body}).encode()
        handler.rfile = io.BytesIO(raw)
        handler.headers = {"Content-Length": str(len(raw))}
        handler.path = "/c/v1/responses"
        handler.reply = Mock()
        with patch.object(FIXTURE, "setting", side_effect=lambda key, default: settings.get(key, default)), \
             patch.object(FIXTURE, "record") as record:
            handler.do_POST()
        return handler.reply, record

    def test_timer_marker_is_not_hidden_by_old_source_or_history(self):
        reply, record = self.request("QA_OTHER_DONE QA_PARK QA_TIMER",
                                    {"SCENARIO": "interjection"})
        self.assertEqual(reply.call_args.args[0], 200)
        self.assertTrue(any(call.kwargs.get("marker") == "QA_TIMER"
                            for call in record.call_args_list))

    def test_response_gate_precedes_reply(self):
        settings = {"SCENARIO": "interjection", "HOLD_TIMER_RESPONSE": True}

        def release(_):
            settings["HOLD_TIMER_RESPONSE"] = False

        with patch.object(FIXTURE.time, "sleep", side_effect=release) as sleep:
            reply, _ = self.request("QA_TIMER", settings)
        sleep.assert_called_once()
        self.assertEqual(reply.call_args.args[0], 200)

    def test_recovered_source_can_finish_without_another_tool(self):
        reply, _ = self.request("QA_PARK QA_TIMER", {
            "SCENARIO": "interjection", "RECOVER_PARK": True})
        self.assertIn("QA_PARK_DONE", json.dumps(reply.call_args.args))

    def test_entry_supplies_parent_startup_timeout_and_pins_images(self):
        with tempfile.TemporaryDirectory() as temp, \
             patch("docker_e2e.continuation_interjection.AcceptanceRun") as run, \
             patch("subprocess.run", return_value=subprocess.CompletedProcess(
                 [], 0, "sha256:fixture\n", "")), patch("builtins.print"):
            run.return_value.run.return_value = {"status": "verified-t3753-isolated"}
            self.assertEqual(main([
                "--image", "candidate", "--python-image", "fixture",
                "--expected-revision", "d841d82f", "--evidence-dir", temp,
                "--case", "T3753"]), 0)
            args = run.call_args.args[0]
            self.assertEqual(args.http_timeout, 45)
            self.assertEqual(args.image, "sha256:fixture")

    def test_bad_revision_fails_before_docker(self):
        with patch("subprocess.run") as docker, patch("sys.stderr", new=io.StringIO()):
            with self.assertRaises(SystemExit):
                main(["--image", "candidate", "--python-image", "fixture",
                      "--expected-revision", "dirty", "--evidence-dir", "unused"])
        docker.assert_not_called()


if __name__ == "__main__":
    unittest.main()
