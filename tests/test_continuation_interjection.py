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
    AcceptanceRun, evaluate_interjection, evaluate_recovery, evaluate_tool_continuation, main,
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
            "turn_id": "timer-turn", "kind": "completed",
            "last_assistant_message": "QA_TIMER_DONE"}},
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
        "conversation_input_assignments": [{
            "message_id": "m-park", "agent_id": "qa", "turn_id": "source-turn",
            "revision": 1, "assigned_at": "2026-10-26T00:00:00Z"}],
        "turn_records": [{"payload_json": json.dumps({
            "turn_id": "source-turn", "agent_id": "qa",
            "owner": {"kind": "agent_lifecycle", "agent_id": "qa"},
            "input_message_ids": ["m-park"],
        })}, {"payload_json": json.dumps({
            "turn_id": "timer-turn", "agent_id": "qa",
            "input_message_ids": ["m-timer", "m-fresh"],
        })}],
        "transcript_entries": [{"payload_json": json.dumps({
            "kind": "assistant_round", "data": {
                "turn_id": "source-turn", "blocks": [{
                    "type": "reasoning_text", "text": "QA_PROVIDER_OWNED_REASONING",
                }],
            },
        })}],
    }}


def interjection_http():
    return [{"marker": "QA_TIMER", "response": "tool_call", "response_held": True,
             "side": "b"},
            {"marker": "QA_PARK", "side": "a", "provider_owned_reasoning": {
                "type": "reasoning", "id": "rs_non_persisted",
                "content": [{"type": "reasoning_text",
                             "text": "QA_PROVIDER_OWNED_REASONING"}],
                "encrypted_content": "opaque-reasoning",
            }}]


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

    def test_runtime_rfc3339_fractional_precision(self):
        for fraction in ("", ".1", ".123", ".123456", ".428891790"):
            for offset in ("Z", "+00:00"):
                with self.subTest(fraction=fraction, offset=offset):
                    events = interjection_events()
                    events[3]["ts"] = "2026-10-26T00:00:30" + fraction + offset
                    before = interjection_snapshot()
                    before["rows"]["provider_queue_parking"][0]["not_before"] = (
                        "2026-10-26T00:02:00" + fraction + offset)
                    boundary = copy.deepcopy(before)
                    boundary["rows"]["queue_entries_current"][1]["status"] = "queued"
                    self.assertEqual(evaluate_timer(
                        events=events, before=before, snapshot=copy.deepcopy(before),
                        boundary=boundary), "verified-i3754-predeadline")

    def test_interrupted_source_must_not_be_interjected(self):
        with self.assertRaises(AssertionError):
            evaluate_timer(snapshot=interjection_snapshot(parked_status="interjected"))

    def test_aborted_timer_turn_fails(self):
        events = interjection_events() + [{
            "type": "turn_terminal", "payload": {
                "turn_id": "timer-turn", "kind": "aborted", "reason": "runtime_error"}}]
        with self.assertRaises(AssertionError):
            evaluate_timer(events=events)

    def test_aborted_source_turn_fails(self):
        events = interjection_events() + [{
            "type": "turn_terminal", "payload": {
                "turn_id": "source-turn", "kind": "aborted", "reason": "runtime_error"}}]
        with self.assertRaises(AssertionError):
            evaluate_timer(events=events)

    def test_parked_source_in_timer_input_batch_fails(self):
        snapshot = interjection_snapshot()
        turn = json.loads(snapshot["rows"]["turn_records"][1]["payload_json"])
        turn["input_message_ids"].append("m-park")
        snapshot["rows"]["turn_records"][1]["payload_json"] = json.dumps(turn)
        with self.assertRaises(AssertionError):
            evaluate_timer(snapshot=snapshot)

    def test_missing_fresh_in_timer_input_batch_fails(self):
        snapshot = interjection_snapshot()
        turn = json.loads(snapshot["rows"]["turn_records"][1]["payload_json"])
        turn["input_message_ids"].remove("m-fresh")
        snapshot["rows"]["turn_records"][1]["payload_json"] = json.dumps(turn)
        with self.assertRaises(AssertionError):
            evaluate_timer(snapshot=snapshot)

    def test_missing_completed_timer_turn_record_fails(self):
        snapshot = interjection_snapshot()
        snapshot["rows"]["turn_records"].pop()
        with self.assertRaises(AssertionError):
            evaluate_timer(snapshot=snapshot)

    def test_missing_completed_timer_turn_id_fails(self):
        events = interjection_events()
        del events[3]["payload"]["turn_id"]
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
            ("conversation_input_assignments", "turn_id", "different-turn"),
            ("messages", "payload_json", '{"text": "changed"}'),
        ):
            snapshot = interjection_snapshot()
            snapshot["rows"][table][0][field] = value
            with self.subTest(table=table), self.assertRaises(AssertionError):
                evaluate_timer(snapshot=snapshot)

    def test_original_input_assignment_is_required_and_source_linked(self):
        assignment = interjection_snapshot()["rows"]["conversation_input_assignments"][0]
        for assignments in ([], [assignment, assignment],
                            [dict(assignment, turn_id="other-turn")],
                            [dict(assignment, agent_id="other-agent")]):
            before = interjection_snapshot()
            before["rows"]["conversation_input_assignments"] = assignments
            with self.subTest(assignments=assignments), self.assertRaises(AssertionError):
                evaluate_timer(before=before)

    def test_response_must_be_held_before_tool_execution(self):
        with self.assertRaises(AssertionError):
            evaluate_timer(http=[{"marker": "QA_TIMER", "response": "tool_call"}])

    def test_timer_cannot_use_a_third_route_or_omit_opaque_source(self):
        http = interjection_http()
        http[0]["side"] = "c"
        with self.assertRaises(AssertionError):
            evaluate_timer(http=http)
        with self.assertRaises(AssertionError):
            evaluate_timer(http=interjection_http()[:1])

    def test_encrypted_only_reasoning_cannot_pass(self):
        http = interjection_http()
        del http[1]["provider_owned_reasoning"]["content"]
        with self.assertRaises(AssertionError):
            evaluate_timer(http=http)

    def test_parsed_source_reasoning_is_required(self):
        for data in ({}, {"turn_id": "source-turn", "blocks": []},
                     {"turn_id": "other-turn", "blocks": [{
                         "type": "reasoning_text", "text": "QA_PROVIDER_OWNED_REASONING"}]}):
            before = interjection_snapshot()
            before["rows"]["transcript_entries"][0]["payload_json"] = json.dumps({
                "kind": "assistant_round", "data": data})
            with self.subTest(data=data), self.assertRaises(AssertionError):
                evaluate_timer(before=before)

    def test_truncated_evidence_cannot_pass(self):
        snapshot = interjection_snapshot()
        snapshot["truncated_tables"] = ["conversation_input_assignments"]
        with self.assertRaises(AssertionError):
            evaluate_timer(snapshot=snapshot)


class RecoveryEvidenceTests(unittest.TestCase):
    def test_deadline_reentry_preserves_owner_and_single_consumption(self):
        self.assertEqual(evaluate_recovery(
            recovery_events(), recovery_snapshot(), interjection_snapshot()),
            "verified-i3754-isolated")

    def test_aborted_turns_fail_recovery(self):
        for turn_id in ("source-turn", "timer-turn", "replay-turn", "other-turn"):
            events = recovery_events() + [{
                "type": "turn_terminal", "payload": {
                    "turn_id": turn_id, "kind": "aborted", "reason": "runtime_error"}}]
            with self.subTest(turn_id=turn_id), self.assertRaises(AssertionError):
                evaluate_recovery(events, recovery_snapshot(), interjection_snapshot())

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
            turn = json.loads(snapshot["rows"]["turn_records"][-1]["payload_json"])
            turn[field] = value
            snapshot["rows"]["turn_records"][-1]["payload_json"] = json.dumps(turn)
            with self.subTest(field=field), self.assertRaises(AssertionError):
                evaluate_recovery(recovery_events(), snapshot, interjection_snapshot())


class FixtureAndEntryTests(unittest.TestCase):
    def request(self, body, settings, side="c"):
        handler = object.__new__(FIXTURE.Mock)
        raw = json.dumps({"input": body}).encode()
        handler.rfile = io.BytesIO(raw)
        handler.headers = {"Content-Length": str(len(raw))}
        handler.path = f"/{side}/v1/responses"
        handler.reply = Mock()
        with patch.object(FIXTURE, "setting", side_effect=lambda key, default: settings.get(key, default)), \
             patch.object(FIXTURE, "record") as record:
            handler.do_POST()
        return handler.reply, record

    def test_tool_call_record_contains_the_gate_marker(self):
        reply, record = self.request("QA_TOOL_CONTINUE",
                                    {"SCENARIO": "tool-continuation"}, side="a")
        self.assertEqual(reply.call_args.args[0], 200)
        self.assertTrue(any(call.kwargs.get("marker") == "QA_TOOL_CONTINUE"
                            and call.kwargs.get("response") == "tool_call"
                            for call in record.call_args_list))

    def test_helper_inputs_seed_cooling_and_the_source_returns_a_tool(self):
        reply, _ = self.request("QA_COOL_CHAIN", {"SCENARIO": "interjection"}, side="a")
        self.assertEqual(reply.call_args.args[0], 503)
        self.assertEqual(reply.call_args.args[2]["Retry-After"], "120")
        reply, _ = self.request("QA_COOL_CHAIN", {"SCENARIO": "interjection"}, side="b")
        self.assertEqual(reply.call_args.args[0], 200)
        reply, record = self.request("QA_PARK", {"SCENARIO": "interjection"}, side="a")
        self.assertEqual(reply.call_args.args[0], 200)
        self.assertTrue(any(call.kwargs.get("marker") == "QA_PARK"
                            and call.kwargs.get("response") == "tool_call"
                            for call in record.call_args_list))
        self.assertIn("qa-3754-park-gate", json.dumps(reply.call_args.args[1]))
        reasoning = {"type": "reasoning", "id": "rs_non_persisted",
                     "content": [{"type": "reasoning_text",
                                  "text": "QA_PROVIDER_OWNED_REASONING"}],
                     "encrypted_content": "opaque-reasoning"}
        self.assertIn(reasoning, reply.call_args.args[1]["output"])
        self.assertTrue(any(call.kwargs.get("provider_owned_reasoning") == reasoning
                            for call in record.call_args_list))
        reply, _ = self.request("QA_PARK", {"SCENARIO": "interjection"}, side="b")
        self.assertEqual(reply.call_args.args[0], 200)

    def test_cooling_fence_uses_canonical_endpoint_model_refs(self):
        run = object.__new__(AcceptanceRun)
        run.snapshot = Mock(return_value={"rows": {"provider_transport_incidents": [{
            "last_model_ref": "qa-a@default/gpt-5.4",
            "next_retry_at": "2099-01-01T00:00:00Z",
        }]}})
        self.assertIsNotNone(run.cooling_fence())
        self.assertIsNone(run.cooling_fence("qa-a/gpt-5.4"))
        self.assertIsNone(run.cooling_fence("qa-b@default/gpt-5.4"))

    def test_parked_source_must_match_original_prompt_not_recovery(self):
        run = object.__new__(AcceptanceRun)
        run.snapshot = Mock(return_value=interjection_snapshot())
        self.assertIsNotNone(run.parked_source("m-park"))
        self.assertIsNone(run.parked_source("m-original"))
        self.assertIsNone(run.parked_source(marker="QA_TIMER"))

    def test_real_source_is_held_before_helper_cooling_and_released_after(self):
        run = object.__new__(AcceptanceRun)
        run.evidence = Path("unused")
        run.create_and_bootstrap = Mock()
        calls = []
        run.api = Mock(side_effect=lambda method, path, body: (
            calls.append((path, body)) or {"message_id": "m-original"}))
        def cooling(model="qa-a@default/gpt-5.4"):
            calls.append(model)
            return True if model.startswith("qa-a@") else None
        run.cooling_fence = Mock(side_effect=cooling)
        run.parked_source = Mock(side_effect=lambda mid: calls.append(mid) or True)
        run.mock_records = Mock(return_value=[{"marker": "QA_PARK",
                                               "response": "tool_call"}])
        run.release = Mock(side_effect=lambda gate: calls.append(f"release:{gate}"))
        run.wait_until = Mock(side_effect=lambda predicate, *_: self.assertTrue(predicate()))
        source = interjection_snapshot()
        source["rows"]["turn_records"][0]["payload_json"] = json.dumps({
            "turn_id": "source-turn", "input_message_ids": ["m-original"]})
        run.snapshot = Mock(return_value=source)
        with patch("docker_e2e.continuation_interjection.write_json"):
            run.prepare_interrupted_source()
        run.create_and_bootstrap.assert_called_once_with("qa2")
        original = calls.index(("/api/control/agents/qa/prompt", {"text": "QA_PARK"}))
        self.assertGreater(calls.index("qa-a@default/gpt-5.4"), original)
        self.assertGreater(calls.index("release:qa-3754-park-gate"),
                           calls.index("qa-a@default/gpt-5.4"))
        self.assertEqual(calls[-1], "m-original")
        self.assertFalse(any(isinstance(call, tuple) and call[0].endswith("/model")
                             for call in calls))

    def test_timer_uses_unchanged_chain_and_holds_response_before_creation(self):
        run = object.__new__(AcceptanceRun)
        run.evidence = Path("unused")
        run.scenario = "interjection"
        run.daemon = "isolated-daemon"
        run.args = Mock(timeout=180)
        run.begin = run.prepare_interrupted_source = Mock()
        run.create_and_bootstrap = Mock(return_value=1)
        calls = []
        run.configure = Mock(side_effect=lambda **settings: calls.append(settings))
        run.api = Mock(side_effect=lambda method, path, body: (
            calls.append(path) or {"message_id": "fresh"}))
        run.snapshot = Mock(side_effect=lambda: (
            calls.append("snapshot") or interjection_snapshot()))
        run.wait_until = run.release = Mock()
        run.docker = Mock(side_effect=lambda *args: calls.append(args))
        run.wait_completed = Mock(return_value={"events": []})
        run.events = Mock(side_effect=lambda *args: (
            calls.append("events") or {"events": []}))
        run.mock_records = Mock(side_effect=lambda: (
            calls.append("http") or interjection_http()))
        with patch("docker_e2e.continuation_interjection.write_json"), \
             patch("docker_e2e.continuation_interjection.evaluate_interjection"), \
             patch("docker_e2e.continuation_interjection.evaluate_recovery",
                   return_value="verified-i3754-isolated"):
            self.assertEqual(run.run_i3754(), "verified-i3754-isolated")
        held = calls.index({"SCENARIO": "interjection", "HOLD_TIMER_RESPONSE": True})
        self.assertEqual(calls[held + 1], "/api/control/agents/qa/timers")
        self.assertFalse(any(isinstance(call, str) and call.endswith("/model")
                             for call in calls))
        self.assertEqual(calls[-4:], ["events", ("stop", "--time", "5", "isolated-daemon"),
                                      "snapshot", "http"])

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
