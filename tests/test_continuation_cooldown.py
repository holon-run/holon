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
    CASES, evaluate_native_cooldown, evaluate_recovery_cooldown,
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
        "queue_entries_current": [{"message_id": "m-native", "status": "processed"}],
        "tool_executions": [{"payload_json": json.dumps({
            "input": {"cmd": "qa-3772-native-gate"}, "status": "success"})}],
    }}
    waiting = copy.deepcopy(snapshot)
    waiting["rows"]["queue_entries_current"][0]["status"] = "claimed"
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
        "queue_entries_current": [{"message_id": "m-recovery", "status": "queued"}],
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
    snapshot["rows"]["queue_entries_current"][0]["status"] = "processed"
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
                "queue_entries_current"][0].update(status="interrupted"),
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
                    b["rows"]["queue_entries_current"][0]["status"] = "processed"
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
                    s["rows"]["queue_entries_current"][0]["status"] = "queued"
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

    def test_unselected_scenarios_are_untouched(self):
        handler = Mock()
        self.assertFalse(consume(handler, {}, "a", lambda key, default: "", Mock()))
        handler.reply.assert_not_called()

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

        def record(**row):
            self.records.append(row)
            if row.get("response") == "source_failure_held":
                self.source_held.set()

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
