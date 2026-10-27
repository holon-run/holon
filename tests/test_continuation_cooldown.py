"""Strict gates for the real-provider continuation/recovery Docker cases."""
import copy
import importlib.util
import json
import sys
import threading
import unittest
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(ROOT / "tests/e2e/docker/openai_stub"))
from docker_e2e.continuation_interjection import (  # noqa: E402
    CASES, INTERJECTION_CASES, AcceptanceRun,
    evaluate_native_cooldown, evaluate_recovery_cooldown,
    evaluate_native_interjection, evaluate_recovery_interjection,
)
from continuation_cooldown import consume, NATIVE_REASONING  # noqa: E402

SPEC = importlib.util.spec_from_file_location(
    "cooldown_fault_fixture", ROOT / "tests/e2e/docker/fault_proxy/server.py")
FIXTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURE)


def native_evidence():
    wait = {
        "turn_id": "source-turn", "attempt_id": "attempt-1", "model": "qa-a@default/gpt-5.4",
        "started_at": "2026-10-27T00:00:01Z", "not_before": "2026-10-27T00:00:12Z",
        "cause": {"failure_kind": "server_error", "http_status": 503,
                  "occurred_at": "2026-10-27T00:00:00Z",
                  "retry_after": {"source": "server_header", "seconds": 12}},
    }
    events = [
        {"type": "agent_state_changed", "payload": {"provider_cooldown_wait": wait}},
        {"type": "agent_state_changed", "payload": {
            "provider_cooldown_wait_ended": {"outcome": "resumed", "turn_id": "source-turn"}}},
        {"type": "turn_terminal", "payload": {
            "kind": "completed", "turn_id": "source-turn",
            "last_assistant_message": "QA_NATIVE_DONE"}},
        {"type": "brief_created", "payload": {"kind": "result"}},
        {"type": "queue_entry_settled", "payload": {
            "message_id": "m-native", "status": "processed"}},
    ]
    http = [
        {"side": "a", "status": 503, "response": "cooling"},
        {"side": "a", "model": "gpt-5.4", "response": "native_tool_call"},
        {"side": "a", "model": "gpt-5.4", "status": 200, "tool_output": True,
         "native_context": [NATIVE_REASONING], "at": 1793059212,
         "tool_outputs": [{"type": "function_call_output", "call_id": "call-native",
                           "output": "QA_NATIVE_TOOL_DONE"}]},
    ]
    snapshot = {"rows": {
        "messages": [{"message_id": "m-native", "payload_json": '{"text": "QA_NATIVE"}'}],
        "queue_entries": [{"message_id": "m-native", "status": "processed"}],
        "tool_executions": [{"payload_json": json.dumps({
            "input": {"cmd": "qa-3772-native-gate"}, "status": "success"})}],
    }}
    waiting = copy.deepcopy(snapshot)
    waiting["rows"]["queue_entries"][0]["status"] = "claimed"
    observation = {
        "state": {"agent": {"agent": {"provider_cooldown_wait": wait}}}, "snapshot": waiting,
        "http_before": http[:2], "http_after": http[:2],
        "tcp_before": [{"side": "a"}], "tcp_after": [{"side": "a"}],
        "requests_before": [{"request_id": "r1"}], "requests_after": [{"request_id": "r1"}],
        "sampled_until": "2026-10-27T00:00:03Z", "elapsed_seconds": 1.1,
    }
    return events, http, snapshot, observation, "m-native"


def recovery_evidence():
    source = {"agent_id": "qa", "message_id": "m-recovery", "payload_json": json.dumps({
        "kind": "internal_followup", "authority_class": "runtime_instruction",
        "origin": {"kind": "system", "subsystem": "model_lineage_recovery"},
        "source_refs": {"source_message_id": "m-source", "source_turn_id": "source-turn"},
        "metadata": {"provider_recovery": {
            "source_message_id": "m-source", "source_turn_id": "source-turn",
            "source_terminal_kind": "provider_failed_needs_recovery",
        }},
    })}
    before = {"rows": {
        "messages": [source],
        "queue_entries": [{"message_id": "m-recovery", "status": "queued"}],
        "provider_queue_parking": [{
            "message_id": "m-recovery", "not_before": "2026-10-27T00:00:12Z"}],
        "tool_executions": [{
            "evidence_id": "tool-original", "agent_id": "qa", "turn_id": "source-turn",
            "payload_json": json.dumps({
                "id": "tool-original", "turn_id": "source-turn", "agent_id": "qa",
                "completed_at": "2026-10-27T00:00:00Z",
                "input": {"cmd": "qa-3772-native-gate"}, "status": "success",
                "output": {"is_error": False, "envelope": {"result": {
                    "exit_status": 0,
                    "stdout_preview": "QA_NATIVE_TOOL_DONE",
                }}},
            }),
        }],
    }}
    snapshot = copy.deepcopy(before)
    snapshot["rows"]["provider_queue_parking"] = []
    snapshot["rows"]["queue_entries"][0]["status"] = "processed"
    owner = {"kind": "agent_lifecycle", "agent_id": "qa"}
    snapshot["rows"]["turn_records"] = [{"payload_json": json.dumps(turn)} for turn in [
        {"turn_id": "source-turn", "owner": owner,
         "input_message_ids": ["m-source"],
         "tool_execution_ids": ["tool-original"],
         "terminal": {"kind": "provider_failed_needs_recovery"}},
        {"turn_id": "recovered-turn", "owner": owner,
         "trigger": {"message_id": "m-recovery"},
         "input_message_ids": ["m-recovery", "m-source"]},
    ]]
    events = [
        {"type": "queue_entry_settled", "payload": {
            "message_id": "m-recovery", "reason": "provider_dispatch_parked"}},
        {"type": "queue_entry_settled", "payload": {
            "message_id": "m-recovery", "status": "processed"}},
        {"type": "queue_entry_claimed", "ts": "2026-10-27T00:00:12Z",
         "payload": {"message_id": "m-recovery"}},
        {"type": "turn_terminal", "payload": {
            "kind": "completed", "turn_id": "recovered-turn",
            "last_assistant_message": "QA_RECOVERY_DONE"}},
        {"type": "brief_created", "payload": {"kind": "result"}},
        {"type": "queue_entry_settled", "payload": {
            "message_id": "m-source", "status": "processed"}},
    ]
    http = [
        {"response": "source_failure", "status": 503, "at": 1793059201},
        *[{"response": "cooling", "status": 503, "side": side} for side in ("a", "b")],
        {"recovery_input": ["m-recovery"], "at": 1793059212,
         "response": "QA_RECOVERY_DONE", "has_tool_evidence": False,
         "tool_evidence_refs": ["tool-original"]},
    ]
    return events, http, snapshot, before


def canonical_message(mid, text):
    payload = {
        "id": mid, "agent_id": "qa", "kind": "operator_prompt",
        "origin": {"kind": "operator"},
        "authority_class": "operator_instruction",
        "delivery_surface": "http_control_prompt",
        "admission_context": "control_authenticated",
        "body": {"kind": "text", "text": text},
    }
    return {"message_id": mid, "agent_id": "qa", "payload_json": json.dumps(payload)}


def rendered_fresh(mid, text):
    return {"message_id": mid, "text": text, "origin": '{"kind":"operator"}',
            "authority_class": "operator_instruction", "delivery_surface": "http_control_prompt",
            "admission_context": "control_authenticated"}


def native_interjection_evidence():
    events, http, snapshot, observation, mid = copy.deepcopy(native_evidence())
    original = canonical_message(mid, "QA_NATIVE")
    fresh = canonical_message("m-fresh", "QA_FRESH_NATIVE")
    snapshot["rows"]["messages"] = [original, fresh]
    snapshot["rows"]["queue_entries"].append({
        "message_id": "m-fresh", "status": "interjected"})
    snapshot["rows"]["turn_records"] = [{"payload_json": json.dumps({
        "turn_id": "source-turn", "agent_id": "qa",
        "owner": {"kind": "agent_lifecycle", "agent_id": "qa"},
        "trigger": {"message_id": mid}, "input_message_ids": [mid, "m-fresh"],
    })}]
    tool = recovery_evidence()[2]["rows"]["tool_executions"][0]
    snapshot["rows"]["tool_executions"] = [tool]
    observation["snapshot"]["rows"]["tool_executions"] = [copy.deepcopy(tool)]
    observation["snapshot"]["rows"]["messages"] = [copy.deepcopy(original)]
    wait = observation["state"]["agent"]["agent"]["provider_cooldown_wait"]
    next_wait = {**wait, "started_at": "2026-10-27T00:00:04Z"}
    events[1]["payload"]["provider_cooldown_wait_ended"].update(
        outcome="interjected", ended_at="2026-10-27T00:00:04Z")
    admitted = {"type": "operator_interjection_admitted", "ts": "2026-10-27T00:00:04Z",
                "payload": {"turn_id": "source-turn", "message_id": "m-fresh",
                            "boundary": "before_provider_continuation",
                            "authority_class": "operator_instruction"}}
    events[2:2] = [
        admitted,
        {"type": "agent_state_changed", "payload": {"provider_cooldown_wait": next_wait}},
        {"type": "agent_state_changed", "payload": {"provider_cooldown_wait_ended": {
            "outcome": "resumed", "turn_id": "source-turn",
            "ended_at": "2026-10-27T00:00:12Z"}}},
    ]
    events[5]["payload"]["last_assistant_message"] = "QA_NATIVE_INTERJECT_DONE"
    http[-1].update(response="QA_NATIVE_INTERJECT_DONE",
                    interjections=[rendered_fresh("m-fresh", "QA_FRESH_NATIVE")])
    active = copy.deepcopy(snapshot)
    active["rows"]["queue_entries"][0]["status"] = "claimed"
    observation["reentered"] = {
        "state": {"agent": {"agent": {"provider_cooldown_wait": next_wait}}},
        "snapshot": active, "sampled_at": "2026-10-27T00:00:05Z",
    }
    return events, http, snapshot, observation, mid, "m-fresh"


def recovery_interjection_evidence():
    events, http, snapshot, before = copy.deepcopy(recovery_evidence())
    original = canonical_message("m-source", "QA_NATIVE")
    root = canonical_message("m-fresh", "QA_FRESH_COMPETE")
    follow = canonical_message("m-follow", "QA_FRESH_FOLLOW")
    recovery = json.loads(before["rows"]["messages"][0]["payload_json"])
    recovery.update(id="m-recovery", agent_id="qa", delivery_surface="runtime_system",
                    admission_context="runtime_owned", causation_id="m-source")
    recovery["metadata"]["provider_recovery"]["root_message_id"] = "m-source"
    before["rows"]["messages"][0]["payload_json"] = json.dumps(recovery)
    before["rows"]["messages"].append(original)
    before["rows"]["conversation_input_assignments"] = [{
        "message_id": "m-source", "agent_id": "qa", "turn_id": "source-turn", "revision": 1}]
    parking = before["rows"]["provider_queue_parking"][0]
    parking.update(not_before="2026-10-27T00:00:30Z", chain_fingerprint="sha256:chain",
                   episode_key="episode-original")
    snapshot["rows"]["messages"] = copy.deepcopy(before["rows"]["messages"]) + [root, follow]
    snapshot["rows"]["conversation_input_assignments"] = copy.deepcopy(
        before["rows"]["conversation_input_assignments"])
    snapshot["rows"]["queue_entries"].extend([
        {"message_id": "m-source", "status": "processed"},
        {"message_id": "m-fresh", "status": "processed"},
        {"message_id": "m-follow", "status": "interjected"},
    ])
    turns = [json.loads(row["payload_json"]) for row in snapshot["rows"]["turn_records"]]
    turns[0].update(agent_id="qa", trigger={
        "message_id": "m-source", "origin": {"kind": "operator"},
        "authority_class": "operator_instruction"})
    turns[1].update(agent_id="qa", trigger={
        "message_id": "m-recovery", "origin": recovery["origin"],
        "authority_class": "runtime_instruction"})
    turns.append({
        "turn_id": "fresh-turn", "agent_id": "qa", "owner": turns[0]["owner"],
        "trigger": {"message_id": "m-fresh"},
        "input_message_ids": ["m-fresh", "m-follow"],
    })
    snapshot["rows"]["turn_records"] = [{"payload_json": json.dumps(turn)} for turn in turns]
    before["rows"]["turn_records"] = [copy.deepcopy(snapshot["rows"]["turn_records"][0])]
    boundary = copy.deepcopy(snapshot)
    boundary["rows"]["turn_records"].pop(1)
    boundary["rows"]["provider_queue_parking"] = copy.deepcopy(before["rows"]["provider_queue_parking"])
    boundary["rows"]["queue_entries"][0]["status"] = "queued"
    queued = copy.deepcopy(boundary)
    queued["rows"]["queue_entries"][3]["status"] = "queued"
    events[2]["ts"] = "2026-10-27T00:00:30Z"
    events[3]["ts"] = "2026-10-27T00:00:31Z"
    events[:0] = [
        {"type": "turn_terminal", "payload": {
            "kind": "provider_failed_needs_recovery", "turn_id": "source-turn"}},
        {"type": "provider_queue_parked", "payload": {
            "message_id": "m-recovery", "not_before": parking["not_before"],
            "episode_key": parking["episode_key"]}},
        {"type": "operator_interjection_admitted", "ts": "2026-10-27T00:00:11Z",
         "payload": {"message_id": "m-follow", "turn_id": "fresh-turn",
                     "authority_class": "operator_instruction", "boundary": "before_tool_execution"}},
        {"type": "turn_terminal", "ts": "2026-10-27T00:00:12Z", "payload": {
            "kind": "completed", "turn_id": "fresh-turn", "last_assistant_message": "QA_FRESH_DONE"}},
        {"type": "brief_created", "payload": {"kind": "result"}},
    ]
    http[1].update(at=1793059199, retry_after="24")
    http[2].update(at=1793059199.1, retry_after="1")
    http[-1].update(at=1793059230, current_input=[{"id": "m-recovery"}, {"id": "m-source"}])
    http[:0] = [{"response": "source_failure_held", "at": 1793059198}]
    http.extend([
        {"response": "fresh_response_held", "side": "b", "at": 1793059210},
        {"response": "QA_FRESH_DONE", "side": "b", "at": 1793059212,
         "interjections": [rendered_fresh("m-follow", "QA_FRESH_FOLLOW")]},
    ])
    competition = {
        "snapshot": boundary, "queued": queued,
        "f0_id": "m-fresh", "f1_id": "m-follow",
        "started_at": "2026-10-27T00:00:10Z", "sampled_at": "2026-10-27T00:00:13Z",
        "b_incident": {"last_model_ref": "qa-b@default/gpt-5.4",
                       "next_retry_at": "2026-10-27T00:00:10Z"},
    }
    return events, http, snapshot, before, competition


def update_message(snapshot, mid, **changes):
    row = next(row for row in snapshot["rows"]["messages"] if row["message_id"] == mid)
    payload = json.loads(row["payload_json"])
    payload.update(changes)
    row["payload_json"] = json.dumps(payload)


def update_turn(snapshot, turn_id, **changes):
    row = next(row for row in snapshot["rows"]["turn_records"]
               if json.loads(row["payload_json"])["turn_id"] == turn_id)
    payload = json.loads(row["payload_json"])
    payload.update(changes)
    row["payload_json"] = json.dumps(payload)


class NativeInterjectionOracleTests(unittest.TestCase):
    def test_selected_set_does_not_relabel_legacy_i3754(self):
        self.assertEqual(INTERJECTION_CASES,
                         ("T3753", "T3772", "T3772I", "R3773", "R3773I"))
        self.assertEqual(set(CASES), set(INTERJECTION_CASES) | {"I3754"})
        self.assertTrue(callable(AcceptanceRun.run_i3754))
        self.assertTrue(callable(AcceptanceRun.run_t3772i))
        self.assertTrue(callable(AcceptanceRun.run_r3773i))

    def test_real_wait_interjected_reentered_and_resumed_with_one_final(self):
        self.assertEqual(evaluate_native_interjection(*native_interjection_evidence()),
                         "verified-t3772i-isolated")

    def test_interjection_requires_the_real_queue_snapshot_table(self):
        args = native_interjection_evidence()
        rows = args[2]["rows"]
        rows["queue_entries_current"] = rows.pop("queue_entries")
        with self.assertRaises(AssertionError):
            evaluate_native_interjection(*args)

    def test_operator_kind_uses_the_canonical_serialized_variant(self):
        args = native_interjection_evidence()
        update_message(args[2], args[-1], kind="prompt")
        with self.assertRaises(AssertionError):
            evaluate_native_interjection(*args)

    def test_native_provider_window_is_not_required_to_inline_opaque_blocks(self):
        args = native_interjection_evidence()
        args[1][-1].update(previous_response_id="resp_a_native", native_context=[])
        self.assertEqual(evaluate_native_interjection(*args), "verified-t3772i-isolated")

    def test_independent_negative_gates(self):
        for name in (
            "missing_reentered_wait", "wrong_attempt", "wrong_deadline", "wrong_model",
            "reentered_after_deadline", "missing_second_wait_event", "wrong_wait_turn",
            "resume_before_deadline", "first_wait_not_interjected", "wrong_wait_order",
            "duplicate_admission", "wrong_admission_turn", "wrong_boundary",
            "wrong_fresh_id", "wrong_fresh_status", "independent_fresh_reply",
            "fresh_claimed", "marker_only", "wrong_rendered_origin", "wrong_rendered_text",
            "missing_fresh_text", "wrong_fresh_authority", "wrong_fresh_admission",
            "wrong_source_provenance", "duplicate_inputs", "missing_original_input",
            "third_input", "changed_trigger", "changed_source", "parking_after_interjection",
            "tool_changed", "tool_failed", "duplicate_final", "missing_consumption",
            "duplicate_consumption", "early_http", "lost_native", "duplicate_continuation",
        ):
            with self.subTest(name=name):
                e, h, s, o, mid, fresh = copy.deepcopy(native_interjection_evidence())
                wait = o["reentered"]["state"]["agent"]["agent"]["provider_cooldown_wait"]
                if name == "missing_reentered_wait":
                    o["reentered"]["state"]["agent"]["agent"]["provider_cooldown_wait"] = None
                elif name in ("wrong_attempt", "wrong_deadline", "wrong_model"):
                    wait[{"wrong_attempt": "attempt_id", "wrong_deadline": "not_before",
                          "wrong_model": "model"}[name]] = (
                        "2026-10-27T00:00:06Z" if name == "wrong_deadline" else "wrong")
                elif name == "reentered_after_deadline":
                    o["reentered"]["sampled_at"] = "2026-10-27T00:00:13Z"
                elif name == "missing_second_wait_event":
                    e.pop(3)
                elif name == "wrong_wait_turn":
                    e[4]["payload"]["provider_cooldown_wait_ended"]["turn_id"] = "other"
                elif name == "resume_before_deadline":
                    e[4]["payload"]["provider_cooldown_wait_ended"]["ended_at"] = "2026-10-27T00:00:10Z"
                elif name == "first_wait_not_interjected":
                    e[1]["payload"]["provider_cooldown_wait_ended"]["outcome"] = "resumed"
                elif name == "wrong_wait_order":
                    e[2], e[3] = e[3], e[2]
                elif name == "duplicate_admission":
                    e.append(copy.deepcopy(e[2]))
                elif name in ("wrong_admission_turn", "wrong_boundary", "wrong_fresh_id"):
                    e[2]["payload"][{"wrong_admission_turn": "turn_id", "wrong_boundary": "boundary",
                                      "wrong_fresh_id": "message_id"}[name]] = "wrong"
                elif name == "wrong_fresh_status":
                    s["rows"]["queue_entries"][1]["status"] = "processed"
                elif name == "independent_fresh_reply":
                    s["rows"]["turn_records"].append({"payload_json": json.dumps({
                        "turn_id": "fresh-turn", "input_message_ids": [fresh]})})
                elif name == "fresh_claimed":
                    e.append({"type": "queue_entry_claimed", "payload": {"message_id": fresh}})
                elif name == "marker_only":
                    h[-1]["interjections"] = []
                elif name in ("wrong_rendered_origin", "wrong_rendered_text"):
                    h[-1]["interjections"][0][
                        "origin" if name == "wrong_rendered_origin" else "text"] = (
                            '{"kind":"external"}' if name == "wrong_rendered_origin" else "QA_FRESH")
                elif name == "missing_fresh_text":
                    update_message(s, fresh, body={"kind": "text", "text": ""})
                elif name in ("wrong_fresh_authority", "wrong_fresh_admission"):
                    update_message(s, fresh, **{
                        "authority_class" if name == "wrong_fresh_authority"
                        else "admission_context": "untrusted"})
                elif name == "wrong_source_provenance":
                    update_message(s, mid, origin={"kind": "external"})
                elif name in ("duplicate_inputs", "missing_original_input", "third_input"):
                    inputs = {"duplicate_inputs": [mid, fresh, fresh],
                              "missing_original_input": [fresh],
                              "third_input": [mid, fresh, "other"]}[name]
                    update_turn(s, "source-turn", input_message_ids=inputs)
                elif name == "changed_trigger":
                    update_turn(s, "source-turn", trigger={"message_id": fresh})
                elif name == "changed_source":
                    update_message(o["reentered"]["snapshot"], mid, body={"text": "changed"})
                elif name == "parking_after_interjection":
                    o["reentered"]["snapshot"]["rows"]["provider_queue_parking"] = [{"message_id": mid}]
                elif name == "tool_changed":
                    o["reentered"]["snapshot"]["rows"]["tool_executions"][0]["evidence_id"] = "other"
                elif name == "tool_failed":
                    row = s["rows"]["tool_executions"][0]
                    payload = json.loads(row["payload_json"])
                    payload["output"]["is_error"] = True
                    row["payload_json"] = json.dumps(payload)
                elif name == "duplicate_final":
                    e.append(copy.deepcopy(e[5]))
                elif name == "missing_consumption":
                    e.pop()
                elif name == "duplicate_consumption":
                    e.append(copy.deepcopy(e[-1]))
                elif name == "early_http":
                    h[-1]["at"] -= 1
                elif name == "lost_native":
                    h[-1]["native_context"] = []
                elif name == "duplicate_continuation":
                    h.append(copy.deepcopy(h[-1]))
                with self.assertRaises(AssertionError):
                    evaluate_native_interjection(e, h, s, o, mid, fresh)


class RecoveryInterjectionOracleTests(unittest.TestCase):
    def test_healthy_b_competition_preserves_queued_recovery_and_validated_lineage(self):
        self.assertEqual(evaluate_recovery_interjection(*recovery_interjection_evidence()),
                         "verified-r3773i-isolated")

    def test_independent_source_and_recovery_lineage_negative_gates(self):
        for name in ("source_message_id", "source_turn_id", "directive_source", "root",
                     "causation", "original_trigger", "recovery_trigger", "original_provenance",
                     "recovery_provenance", "runtime_admission", "original_owner", "duplicate_recovery",
                     "duplicate_recovery_final", "duplicate_final", "source_not_consumed",
                     "recovery_not_consumed", "duplicate_source_consumption",
                     "duplicate_recovery_consumption", "duplicate_inputs", "missing_source_input",
                     "third_input", "missing_recovery_claim", "duplicate_recovery_http",
                     "duplicate_recovery_input"):
            with self.subTest(name=name):
                e, h, s, b, c = copy.deepcopy(recovery_interjection_evidence())
                checkpoints = (s, b, c["snapshot"], c["queued"])
                for checkpoint in checkpoints:
                    recovery = json.loads(checkpoint["rows"]["messages"][0]["payload_json"])
                    if name == "source_message_id":
                        recovery["source_refs"]["source_message_id"] = "wrong"
                    elif name == "source_turn_id":
                        recovery["source_refs"]["source_turn_id"] = "wrong"
                    elif name == "directive_source":
                        recovery["metadata"]["provider_recovery"]["source_message_id"] = "wrong"
                    elif name == "root":
                        recovery["metadata"]["provider_recovery"]["root_message_id"] = "wrong"
                    elif name == "causation":
                        recovery["causation_id"] = "wrong"
                    elif name == "recovery_provenance":
                        recovery["origin"] = {"kind": "external"}
                    elif name == "runtime_admission":
                        recovery["admission_context"] = "public_unauthenticated"
                    checkpoint["rows"]["messages"][0]["payload_json"] = json.dumps(recovery)
                if name == "original_trigger":
                    update_turn(s, "source-turn", trigger={"message_id": "wrong"})
                elif name == "recovery_trigger":
                    update_turn(s, "recovered-turn", trigger={"message_id": "wrong"})
                elif name == "original_provenance":
                    for checkpoint in checkpoints:
                        update_message(checkpoint, "m-source", origin={"kind": "external"})
                elif name == "original_owner":
                    update_turn(s, "source-turn", owner={"kind": "work_item", "work_item_id": "other"})
                elif name == "duplicate_recovery":
                    s["rows"]["messages"].append(copy.deepcopy(s["rows"]["messages"][0]))
                elif name in ("duplicate_recovery_final", "duplicate_final"):
                    terminal = next(event for event in e if event["type"] == "turn_terminal"
                                    and event["payload"].get("last_assistant_message")
                                    == ("QA_RECOVERY_DONE" if name == "duplicate_recovery_final"
                                        else "QA_FRESH_DONE"))
                    e.append(copy.deepcopy(terminal))
                elif name in ("source_not_consumed", "recovery_not_consumed"):
                    mid = "m-source" if name == "source_not_consumed" else "m-recovery"
                    e[:] = [event for event in e if not (
                        event["type"] == "queue_entry_settled"
                        and event["payload"].get("message_id") == mid
                        and event["payload"].get("status") == "processed")]
                elif name in ("duplicate_source_consumption", "duplicate_recovery_consumption"):
                    mid = "m-source" if name == "duplicate_source_consumption" else "m-recovery"
                    e.append(copy.deepcopy(next(event for event in e if (
                        event["type"] == "queue_entry_settled"
                        and event["payload"].get("message_id") == mid
                        and event["payload"].get("status") == "processed"))))
                elif name in ("duplicate_inputs", "missing_source_input", "third_input"):
                    inputs = {"duplicate_inputs": ["m-recovery", "m-source", "m-source"],
                              "missing_source_input": ["m-recovery"],
                              "third_input": ["m-recovery", "m-source", "other"]}[name]
                    update_turn(s, "recovered-turn", input_message_ids=inputs)
                elif name == "missing_recovery_claim":
                    e[:] = [event for event in e if event["type"] != "queue_entry_claimed"]
                elif name == "duplicate_recovery_http":
                    h.append(copy.deepcopy(next(row for row in h if row.get("recovery_input"))))
                elif name == "duplicate_recovery_input":
                    next(row for row in h if row.get("recovery_input"))["recovery_input"].append(
                        "m-recovery")
                with self.assertRaises(AssertionError):
                    evaluate_recovery_interjection(e, h, s, b, c)

    def test_competition_is_not_a_marker_only_or_rewritten_parking_case(self):
        for name in ("deadline", "fingerprint", "episode", "assignment", "queued_owner",
                     "preclaimed_recovery", "fresh_not_queued", "fresh_not_interjected",
                     "fresh_wrong_turn", "fresh_duplicate_admission", "fresh_wrong_boundary",
                     "missing_rendered_fresh", "fresh_external_origin", "follow_external_origin",
                     "b_still_cooling", "competition_after_deadline", "fresh_on_a",
                     "f0_not_root", "f0_external", "f0_wrong_trigger", "f1_upgraded",
                     "missing_fresh_barrier", "drained_recovery", "fresh_parked",
                     "recovery_http_leaks_fresh", "recovery_http_before_deadline",
                     "same_retry_after", "no_source_barrier",
                     "failure_before_helper", "lost_tool_reference", "mutated_tool_success"):
            with self.subTest(name=name):
                e, h, s, b, c = copy.deepcopy(recovery_interjection_evidence())
                if name in ("deadline", "fingerprint", "episode"):
                    c["snapshot"]["rows"]["provider_queue_parking"][0][
                        {"deadline": "not_before", "fingerprint": "chain_fingerprint",
                         "episode": "episode_key"}[name]] = "wrong"
                elif name == "assignment":
                    c["snapshot"]["rows"]["conversation_input_assignments"][0]["turn_id"] = "wrong"
                elif name == "queued_owner":
                    c["snapshot"]["rows"]["queue_entries"][0]["status"] = "interrupted"
                elif name == "preclaimed_recovery":
                    c["snapshot"]["rows"]["turn_records"].append(copy.deepcopy(
                        s["rows"]["turn_records"][1]))
                elif name == "fresh_not_queued":
                    c["queued"]["rows"]["queue_entries"][3]["status"] = "processed"
                elif name == "fresh_not_interjected":
                    s["rows"]["queue_entries"][3]["status"] = "processed"
                elif name in ("fresh_wrong_turn", "fresh_wrong_boundary"):
                    e[2]["payload"][
                        "turn_id" if name == "fresh_wrong_turn" else "boundary"] = "wrong"
                elif name == "fresh_duplicate_admission":
                    e.append(copy.deepcopy(e[2]))
                elif name == "missing_rendered_fresh":
                    h[-1]["interjections"] = []
                elif name == "fresh_external_origin":
                    update_message(s, "m-fresh", origin={"kind": "external"})
                elif name == "follow_external_origin":
                    update_message(s, "m-follow", origin={"kind": "external"})
                elif name == "b_still_cooling":
                    c["b_incident"]["next_retry_at"] = "2026-10-27T00:00:20Z"
                elif name == "competition_after_deadline":
                    c["sampled_at"] = "2026-10-27T00:00:31Z"
                elif name == "fresh_on_a":
                    h[-1]["side"] = "a"
                elif name == "f0_not_root":
                    update_message(s, "m-fresh", causation_id="m-follow")
                elif name == "f0_external":
                    update_message(s, "m-fresh", kind="timer")
                elif name == "f0_wrong_trigger":
                    update_turn(s, "fresh-turn", trigger={"message_id": "wrong"})
                elif name == "f1_upgraded":
                    update_message(s, "m-follow", authority_class="runtime_instruction")
                elif name == "missing_fresh_barrier":
                    h[:] = [row for row in h if row.get("response") != "fresh_response_held"]
                elif name == "drained_recovery":
                    e.append({"type": "operator_interjection_admitted", "payload": {
                        "message_id": "m-recovery", "turn_id": "fresh-turn"}})
                elif name == "fresh_parked":
                    c["queued"]["rows"]["provider_queue_parking"].append(
                        {"message_id": "m-fresh"})
                elif name == "recovery_http_leaks_fresh":
                    next(row for row in h if row.get("recovery_input"))[
                        "current_input"].append({"id": "m-fresh"})
                elif name == "recovery_http_before_deadline":
                    next(row for row in h if row.get("recovery_input"))["at"] -= 1
                elif name == "same_retry_after":
                    next(row for row in h if row.get("side") == "b")["retry_after"] = "24"
                elif name == "no_source_barrier":
                    h.pop(0)
                elif name == "failure_before_helper":
                    next(row for row in h if row.get("response") == "source_failure")["at"] -= 10
                elif name == "lost_tool_reference":
                    next(row for row in h if row.get("recovery_input"))["tool_evidence_refs"] = []
                elif name == "mutated_tool_success":
                    s["rows"]["tool_executions"][0]["payload_json"] = "{}"
                with self.assertRaises(AssertionError):
                    evaluate_recovery_interjection(e, h, s, b, c)


class NativeOracleTests(unittest.TestCase):
    def test_cases_are_executable_cli_choices(self):
        self.assertTrue({"T3772", "R3773"}.issubset(CASES))

    def test_real_fault_active_wait_and_single_continuation(self):
        self.assertEqual(evaluate_native_cooldown(*native_evidence()), "verified-t3772-isolated")

    def test_provider_window_retains_opaque_state_without_inline_replay(self):
        events, http, snapshot, observation, mid = native_evidence()
        http[-1].update(previous_response_id="resp_a_native", native_context=[])
        self.assertEqual(evaluate_native_cooldown(
            events, http, snapshot, observation, mid), "verified-t3772-isolated")

    def test_negative_gates(self):
        mutations = {
            "http_io": lambda e, h, s, o: o["http_after"].append({"side": "a"}),
            "tcp_io": lambda e, h, s, o: o["tcp_after"].append({"side": "a"}),
            "budget_charge": lambda e, h, s, o: o["requests_after"].append({"request_id": "r2"}),
            "no_real_failure": lambda e, h, s, o: h.pop(0),
            "missing_query_wait": lambda e, h, s, o: o["state"]["agent"]["agent"].clear(),
            "wrong_cause": lambda e, h, s, o: o["state"]["agent"]["agent"]["provider_cooldown_wait"]["cause"].update(
                http_status=429),
            "raw_diagnostic": lambda e, h, s, o: o["state"]["agent"]["agent"]["provider_cooldown_wait"]["cause"].update(
                detail="fixture-only diagnostic"),
            "early_retry": lambda e, h, s, o: h[-1].update(at=1793059211),
            "wrong_model": lambda e, h, s, o: h[-1].update(model="other"),
            "wrong_provider": lambda e, h, s, o: h[-1].update(side="b"),
            "lost_native": lambda e, h, s, o: h[-1].update(native_context=[]),
            "rebuilt_native": lambda e, h, s, o: h[-1].update(native_context=[{
                "type": "reasoning", "content": NATIVE_REASONING["content"]}]),
            "wrong_call": lambda e, h, s, o: h[-1]["tool_outputs"][0].update(call_id="other"),
            "lost_result": lambda e, h, s, o: h[-1]["tool_outputs"][0].update(output=""),
            "duplicate_result": lambda e, h, s, o: h[-1]["tool_outputs"].append(
                h[-1]["tool_outputs"][0]),
            "rerun_tool": lambda e, h, s, o: s["rows"]["tool_executions"].append(
                s["rows"]["tool_executions"][0]),
            "source_parked": lambda e, h, s, o: o["snapshot"]["rows"][
                "queue_entries"][0].update(status="interrupted"),
            "terminal_repair": lambda e, h, s, o: e.append({
                "type": "turn_terminal", "payload": {"kind": "provider_failed_needs_recovery"}}),
            "replay": lambda e, h, s, o: e.append({"type": "turn_replay_started"}),
            "duplicate_consumption": lambda e, h, s, o: e.append(copy.deepcopy(e[-1])),
            "missing_query_event": lambda e, h, s, o: e.pop(0),
            "not_resumed": lambda e, h, s, o: e[1]["payload"][
                "provider_cooldown_wait_ended"].update(outcome="validation_failed"),
            "truncated": lambda e, h, s, o: s.update(truncated_tables=["messages"]),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                events, http, snapshot, observation, mid = copy.deepcopy(native_evidence())
                # Isolate samples which otherwise deliberately share immutable fixtures.
                observation = copy.deepcopy(observation)
                observation["http_after"] = copy.deepcopy(observation["http_after"])
                mutate(events, http, snapshot, observation)
                with self.assertRaises(AssertionError):
                    evaluate_native_cooldown(events, http, snapshot, observation, mid)


class RecoveryOracleTests(unittest.TestCase):
    def test_compound_failure_retains_and_consumes_recovery_input(self):
        self.assertEqual(evaluate_recovery_cooldown(*recovery_evidence()),
                         "verified-r3773-isolated")

    def test_negative_gates(self):
        for name in ("missing_source", "mutated_source", "missing_parking", "consumed",
                     "missing_fault", "early", "lost_evidence", "duplicate_dispatch",
                     "rerun_tool", "missing_claim", "duplicate_consumption", "aborted",
                     "changed_owner", "wrong_recovery_input", "not_processed",
                     "input_only_marker", "tool_error", "wrong_tool_turn",
                     "wrong_source_binding", "duplicate_original_consumption"):
            with self.subTest(name=name):
                e, h, s, b = copy.deepcopy(recovery_evidence())
                if name == "missing_source":
                    b["rows"]["messages"] = []
                elif name == "mutated_source":
                    s["rows"]["messages"][0]["payload_json"] = "{}"
                elif name == "missing_parking":
                    b["rows"]["provider_queue_parking"] = []
                elif name == "consumed":
                    b["rows"]["queue_entries"][0]["status"] = "processed"
                elif name == "missing_fault":
                    h.pop(0)
                elif name == "early":
                    h[-1]["at"] -= 1
                elif name == "lost_evidence":
                    h[-1]["tool_evidence_refs"] = ["tool-unrelated"]
                elif name == "duplicate_dispatch":
                    h.append(copy.deepcopy(h[-1]))
                elif name == "rerun_tool":
                    s["rows"]["tool_executions"] *= 2
                elif name == "missing_claim":
                    e.pop(2)
                elif name == "duplicate_consumption":
                    e.append(copy.deepcopy(e[1]))
                elif name == "aborted":
                    e.append({"type": "turn_terminal", "payload": {"kind": "aborted"}})
                elif name == "changed_owner":
                    turn = json.loads(s["rows"]["turn_records"][1]["payload_json"])
                    turn["owner"] = {"kind": "conversation", "conversation_id": "other"}
                    s["rows"]["turn_records"][1]["payload_json"] = json.dumps(turn)
                elif name == "wrong_recovery_input":
                    turn = json.loads(s["rows"]["turn_records"][1]["payload_json"])
                    turn["input_message_ids"].append("unrelated")
                    s["rows"]["turn_records"][1]["payload_json"] = json.dumps(turn)
                elif name == "not_processed":
                    s["rows"]["queue_entries"][0]["status"] = "queued"
                elif name in ("input_only_marker", "tool_error", "wrong_tool_turn"):
                    for checkpoint in (s, b):
                        row = checkpoint["rows"]["tool_executions"][0]
                        tool = json.loads(row["payload_json"])
                        if name == "input_only_marker":
                            tool["input"]["cmd"] += " QA_NATIVE_TOOL_DONE"
                            tool["output"]["envelope"]["result"]["stdout_preview"] = ""
                        elif name == "tool_error":
                            tool["output"]["is_error"] = True
                        else:
                            row["turn_id"] = tool["turn_id"] = "unrelated"
                        row["payload_json"] = json.dumps(tool)
                elif name == "wrong_source_binding":
                    for checkpoint in (s, b):
                        row = checkpoint["rows"]["messages"][0]
                        source = json.loads(row["payload_json"])
                        source["source_refs"]["source_turn_id"] = "unrelated"
                        row["payload_json"] = json.dumps(source)
                elif name == "duplicate_original_consumption":
                    e.append(copy.deepcopy(e[-1]))
                with self.assertRaises(AssertionError):
                    evaluate_recovery_cooldown(e, h, s, b)


class ProviderFixtureTests(unittest.TestCase):
    def request(self, payload, scenario="native-cooldown", side="a", **settings):
        handler, records = Mock(), Mock()
        handled = consume(handler, payload, side,
                          lambda key, default: {"SCENARIO": scenario, **settings}.get(key, default),
                          records)
        self.assertTrue(handled)
        return handler.reply, records

    def test_actual_503_includes_server_retry_header(self):
        reply, records = self.request({"input": "QA_COOL_NATIVE"})
        self.assertEqual(reply.call_args.args[0], 503)
        self.assertEqual(reply.call_args.args[2], {"Retry-After": "12"})
        self.assertEqual(records.call_args.kwargs["response"], "cooling")

    def test_native_round_retains_opaque_context_and_gated_tool(self):
        reply, _ = self.request({"input": "QA_NATIVE"})
        output = reply.call_args.args[1]["output"]
        self.assertEqual(output[1], NATIVE_REASONING)
        self.assertIn("qa-3772-native-gate", output[0]["arguments"])

    def test_native_continuation_logs_original_result_and_context(self):
        reply, records = self.request({"input": [NATIVE_REASONING, {
            "type": "function_call_output", "call_id": "call-native",
            "output": "QA_NATIVE_TOOL_DONE"}]})
        self.assertEqual(reply.call_args.args[0], 200)
        self.assertEqual(records.call_args.kwargs["native_context"], [NATIVE_REASONING])
        self.assertEqual(len(records.call_args.kwargs["tool_outputs"]), 1)

    def test_compound_fault_holds_real_source_request_before_failure(self):
        reply, records = self.request({"input": [{
            "type": "function_call_output", "call_id": "call-native", "output": "done"}]},
            scenario="recovery-cooldown")
        self.assertEqual(reply.call_args.args[0], 503)
        self.assertEqual(records.call_args_list[0].kwargs["response"], "source_failure_held")
        self.assertEqual(records.call_args_list[1].kwargs["response"], "source_failure")

    def test_operator_root_competition_holds_then_returns_gated_tool(self):
        reply, records = self.request({"input": "QA_FRESH_COMPETE"},
                                      scenario="recovery-cooldown-interjection")
        output = reply.call_args.args[1]["output"]
        self.assertEqual(reply.call_args.args[0], 200)
        self.assertEqual(records.call_args_list[0].kwargs["response"],
                         "fresh_response_held")
        self.assertEqual(records.call_args_list[1].kwargs["response"], "fresh_tool_call")
        self.assertEqual(output[0]["call_id"], "call-fresh-compete")
        self.assertIn("qa-3773-fresh-gate", output[0]["arguments"])

    def test_fresh_competition_settles_once_with_follow_up_interjection(self):
        reply, records = self.request({"input": [
            {"role": "user", "content": [{"type": "input_text", "text": "QA_FRESH_COMPETE"}]},
            {"type": "function_call_output", "call_id": "call-fresh-compete",
             "output": "QA_FRESH_TOOL_DONE"},
        ]}, scenario="recovery-cooldown-interjection")
        self.assertEqual(reply.call_args.args[0], 200)
        self.assertEqual(records.call_args.kwargs["response"], "QA_FRESH_DONE")

    def test_unselected_scenarios_are_untouched(self):
        handler = Mock()
        self.assertFalse(consume(handler, {}, "a", lambda key, default: "", Mock()))
        handler.reply.assert_not_called()

    def test_fresh_native_continuation_without_root_text_does_not_fail_source(self):
        reply, records = self.request({
            "previous_response_id": "resp_b_fresh",
            "input": [{"type": "function_call_output", "call_id": "call-fresh-compete",
                       "output": "QA_FRESH_TOOL_DONE"}],
        }, scenario="recovery-cooldown-interjection", side="b")
        self.assertEqual(reply.call_args.args[0], 200)
        self.assertEqual(records.call_count, 1)
        self.assertEqual(records.call_args.kwargs["response"], "QA_FRESH_DONE")
        self.assertEqual(records.call_args.kwargs["current_input"], [])

    def test_unrelated_native_output_cannot_take_fresh_success_path(self):
        reply, records = self.request({
            "previous_response_id": "resp_b_fresh",
            "input": [{"type": "function_call_output", "call_id": "call-native",
                       "output": "QA_FRESH_TOOL_DONE"}],
        }, scenario="recovery-cooldown-interjection", side="b")
        self.assertEqual(reply.call_args.args[0], 503)
        self.assertEqual(records.call_args.kwargs["response"], "source_failure")

    def test_manifest_matches_real_runner_cases(self):
        manifest = json.loads((ROOT / "tests/e2e/docker/continuation_interjection_manifest.json")
                              .read_text())
        self.assertEqual(tuple(case["id"] for case in manifest["cases"]), CASES)
        self.assertTrue((ROOT / manifest["runner"]).is_file())


class FaultProxyHttpTests(unittest.TestCase):
    def setUp(self):
        self.settings = {"SCENARIO": "native-cooldown"}
        self.records = []
        self.source_held = threading.Event()
        self.fresh_held = threading.Event()

        def record(**row):
            self.records.append(row)
            if row.get("response") == "source_failure_held":
                self.source_held.set()
            if row.get("response") == "fresh_response_held":
                self.fresh_held.set()

        for patcher in (
            patch.object(FIXTURE, "setting", side_effect=(
                lambda key, default: self.settings.get(key, default))),
            patch.object(FIXTURE, "record", side_effect=record),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), FIXTURE.Mock)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop_server)

    def stop_server(self):
        self.settings["HOLD_SOURCE_FAILURE"] = False
        self.settings["HOLD_FRESH_RESPONSE"] = False
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def request(self, payload, side="a"):
        url = f"http://127.0.0.1:{self.server.server_port}/{side}/v1/responses"
        try:
            result = urllib.request.urlopen(urllib.request.Request(
                url, json.dumps(payload).encode(), {"Content-Type": "application/json"}),
                timeout=3)
        except urllib.error.HTTPError as error:
            result = error
        with result:
            return result.status, result.headers, json.load(result)

    def recovery_payload(self, tool_evidence=False, tool_ref=None):
        provenance = {
            "id": "helper-repair", "kind": "internal_followup",
            "authority_class": "runtime_instruction",
            "origin": {"kind": "system", "subsystem": "model_lineage_recovery"},
        }
        text = (
            "## current_input\n"
            "Current input batch (receive order; each item retains its own "
            "authority and reply route):\n"
            "- reply_context_id: helper-repair\n"
            "  source_ref: message:helper-repair\n"
            f"  provenance: {json.dumps(provenance)}\n"
            "  representation: full\n  body:\n"
            + ("QA_NATIVE_TOOL_DONE" if tool_evidence else "resume helper")
            + (f"\ntool_execution_id={tool_ref}" if tool_ref else "")
        )
        return {"input": [{"role": "user", "content": [
            {"type": "input_text", "text": text}]}]}

    def test_fixture_serves_a_real_http_failure_and_native_continuation(self):
        status, headers, _ = self.request({"input": "QA_COOL_NATIVE"})
        self.assertEqual(status, 503)
        self.assertEqual(headers["Retry-After"], "12")
        status, _, body = self.request({"input": "QA_NATIVE"})
        self.assertEqual(status, 200)
        self.assertIn(NATIVE_REASONING, body["output"])
        status, _, _ = self.request({"input": [NATIVE_REASONING, {
            "type": "function_call_output", "call_id": "call-native",
            "output": "QA_NATIVE_TOOL_DONE"}]})
        self.assertEqual(status, 200)
        self.assertEqual([row["status"] for row in self.records], [503, 200, 200])
        self.assertEqual(self.records[-1]["native_context"], [NATIVE_REASONING])

    def test_real_recovery_request_is_held_until_both_helper_failures(self):
        self.settings.update(SCENARIO="recovery-cooldown", HOLD_SOURCE_FAILURE=True)
        with ThreadPoolExecutor(max_workers=1) as pool:
            source = pool.submit(self.request, {"input": [{
                "type": "function_call_output", "call_id": "call-native",
                "output": "QA_NATIVE_TOOL_DONE"}]})
            try:
                self.assertTrue(self.source_held.wait(timeout=2))
                for side, payload in (("a", {"input": "QA_COOL_RECOVERY"}),
                                      ("b", self.recovery_payload())):
                    status, headers, _ = self.request(payload, side)
                    self.assertEqual(status, 503)
                    self.assertEqual(headers["Retry-After"], "12")
                self.assertFalse(source.done())
            finally:
                self.settings["HOLD_SOURCE_FAILURE"] = False
            self.assertEqual(source.result(timeout=3)[0], 503)
        self.assertEqual([row["response"] for row in self.records],
                         ["source_failure_held", "cooling", "cooling", "source_failure"])
        self.assertEqual([row["side"] for row in self.records], ["a", "a", "b", "a"])

    def test_fresh_root_response_is_really_held_until_released(self):
        self.settings.update(SCENARIO="recovery-cooldown-interjection",
                             HOLD_FRESH_RESPONSE=True)
        with ThreadPoolExecutor(max_workers=1) as pool:
            root = pool.submit(self.request, {"input": "QA_FRESH_COMPETE"}, "b")
            try:
                self.assertTrue(self.fresh_held.wait(timeout=2))
                self.assertFalse(root.done())
            finally:
                self.settings["HOLD_FRESH_RESPONSE"] = False
            status, _, body = root.result(timeout=3)
            self.assertEqual(status, 200)
            self.assertIn("call-fresh-compete", json.dumps(body))
        self.assertEqual([row["response"] for row in self.records],
                         ["fresh_response_held", "fresh_tool_call"])
        self.assertEqual([row["side"] for row in self.records], ["b", "b"])

    def test_real_http_fresh_native_continuation_without_root_text(self):
        self.settings.update(SCENARIO="recovery-cooldown-interjection")
        status, _, _ = self.request({
            "previous_response_id": "resp_b_fresh",
            "input": [{"type": "function_call_output", "call_id": "call-fresh-compete",
                       "output": "QA_FRESH_TOOL_DONE"}],
        }, "b")
        self.assertEqual(status, 200)
        self.assertEqual([row["response"] for row in self.records], ["QA_FRESH_DONE"])
        self.assertEqual([row["side"] for row in self.records], ["b"])

    def test_helper_fault_does_not_reject_source_recovery_or_released_helper(self):
        self.settings.update(SCENARIO="recovery-cooldown", HOLD_SOURCE_FAILURE=True)
        status, _, _ = self.request(self.recovery_payload(tool_evidence=True), "b")
        self.assertEqual(status, 200)
        self.settings["HOLD_SOURCE_FAILURE"] = False
        status, _, _ = self.request(self.recovery_payload(), "b")
        self.assertEqual(status, 200)
        self.assertEqual([row["response"] for row in self.records],
                         ["QA_RECOVERY_DONE", "QA_RECOVERY_DONE"])

    def test_recovery_observes_tool_reference_without_inventing_inline_output(self):
        self.settings.update(SCENARIO="recovery-cooldown")
        status, _, _ = self.request(self.recovery_payload(tool_ref="tool-original"), "b")
        self.assertEqual(status, 200)
        self.assertEqual(self.records[-1]["tool_evidence_refs"], ["tool-original"])
        self.assertFalse(self.records[-1]["has_tool_evidence"])

    def test_unselected_scenario_keeps_existing_fault_dispatch(self):
        self.settings.update(SCENARIO="", STATUS_A=429, RETRY_AFTER_A="7")
        status, headers, _ = self.request({"input": "QA_EXISTING"})
        self.assertEqual(status, 429)
        self.assertEqual(headers["Retry-After"], "7")
        self.assertEqual(self.records[0]["retry_after"], "7")


if __name__ == "__main__":
    unittest.main()
