"""Formal runner/fixture/oracle regressions; no Docker or wall-clock backoff needed."""
from __future__ import annotations

import copy
import importlib
import io
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from argparse import Namespace
from datetime import datetime, timezone
from contextlib import redirect_stdout
from unittest.mock import Mock, patch

from scripts.docker_e2e import task_recovery as recovery
from scripts.docker_e2e import task_result_oracle as oracle
from tests import test_docker_e2e_runner as runner_tests

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests/e2e/docker/openai_stub"))
fixture = importlib.import_module("task_recovery")
sys.path.pop(0)


def snapshot_with_roots(count=5):
    snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
    record = snapshot["task_result_settlements"][0]
    record.update(
        recovery_epoch=2, recovery_attempts=count,
        last_recovery_root_attempt_id=f"root-{count}" if count else None,
        next_recheck_at=None, recovery_paused_at=None, recovery_diagnostic_at=None,
    )
    times = [
        "2026-10-25T13:00:00Z", "2026-10-25T13:01:00Z", "2026-10-25T13:03:00Z",
        "2026-10-25T13:07:00Z", "2026-10-25T13:15:00Z",
    ]
    snapshot["task_result_recovery_admissions"] = [{
        "result_identity": record["result_identity"], "recovery_epoch": 2,
        "root_attempt_id": f"root-{index + 1}", "admitted_at": timestamp,
    } for index, timestamp in enumerate(times[:count])]
    snapshot["execution_protocol_attempts"] += [{
        "agent_id": "parent", "attempt_id": f"root-{index + 1}",
        "payload_json": json.dumps({"attempt_id": f"root-{index + 1}",
                                   "recovery_of_attempt_id": None}),
    } for index in range(count)]
    return snapshot, record


class RecoveryOracleTests(unittest.TestCase):
    def test_terminal_receipts_require_exact_structured_envelopes(self):
        def check(detail):
            return oracle.require_terminal_task_receipt(
                detail, task_id="task-1", result_message_id="result-1",
                activation_id="attempt-1",
            )

        task = {"task_id": "task-1", "status": "completed"}
        good = {
            "tool_name": "TaskStatus", "status": "success",
            "input": {"task_id": "task-1"},
            "output": {"envelope": {
                "tool_name": "TaskStatus", "status": "success", "result": {"task": task},
            }},
        }
        self.assertEqual(check(good)["task"], task)
        bad_outputs = [
            json.dumps(good["output"]), {"debug": good["output"]},
            {"result": {"task": task}}, {"envelope": {"result": {"task": task}}},
            {"envelope": {**good["output"]["envelope"], "status": "error"}},
            {"envelope": {**good["output"]["envelope"], "tool_name": "ExecCommand"}},
            {"envelope": {**good["output"]["envelope"],
                          "result": {"debug": {"task": task}}}},
            {"envelope": {**good["output"]["envelope"],
                          "result": {"task": {**task, "status": "running"}}}},
            {"envelope": {**good["output"]["envelope"],
                          "result": {"task": {**task, "task_id": "foreign"}}}},
        ]
        for output in bad_outputs:
            with self.subTest(output=output), self.assertRaises(AssertionError):
                check({**good, "output": output})
        wait = {
            "tool_name": "WaitFor", "status": "success",
            "input": {"wake": "task_result", "resource": "task-1", "delivery": "silent"},
            "output": {"envelope": {
                "tool_name": "WaitFor", "status": "success", "result": {
                    "continuation": "continue_turn", "task_id": "task-1",
                    "result_message_id": "result-1", "activation_id": "attempt-1",
                    "disposition": "task_result_already_admitted",
                },
            }},
        }
        check(wait)
        settled = copy.deepcopy(wait)
        settled["output"]["envelope"]["result"].update(
            disposition="task_result_settled", settlement_disposition="model_delivered",
        )
        check(settled)
        for field, value in [
            ("continuation", "yield_and_wait"), ("task_id", "foreign"),
            ("result_message_id", "foreign"), ("activation_id", "foreign"),
            ("disposition", "task_result_in_flight"),
        ]:
            broken = copy.deepcopy(wait)
            broken["output"]["envelope"]["result"][field] = value
            with self.subTest(field=field), self.assertRaises(AssertionError):
                check(broken)

    def test_running_task_with_nested_terminal_lookalike_is_not_terminal(self):
        snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
        task = json.loads(snapshot["tasks"][0]["payload_json"])
        task.update(status="running", debug={"status": "completed"})
        snapshot["tasks"][0]["payload_json"] = json.dumps(task)
        with self.assertRaises(AssertionError):
            oracle.require_task_reentry(
                snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
            )

    def test_five_distinct_roots_obey_real_policy(self):
        snapshot, record = snapshot_with_roots()
        self.assertEqual(len(oracle.recovery_ledger(
            snapshot, record, expected_count=5, real_backoff=True)), 5)

    def test_duplicate_root_sixth_root_and_shortened_policy_are_red(self):
        for mutation in ("duplicate", "sixth", "early", "epoch", "count"):
            with self.subTest(mutation=mutation):
                snapshot, record = snapshot_with_roots()
                rows = snapshot["task_result_recovery_admissions"]
                if mutation == "duplicate":
                    rows[-1]["root_attempt_id"] = rows[0]["root_attempt_id"]
                elif mutation == "sixth":
                    rows.append({**rows[-1], "root_attempt_id": "root-6"})
                    record["recovery_attempts"] = 6
                elif mutation == "early":
                    rows[-1]["admitted_at"] = "2026-10-25T13:14:59Z"
                elif mutation == "epoch":
                    rows[-1]["recovery_epoch"] = 1
                else:
                    record["recovery_attempts"] = 4
                with self.assertRaises(AssertionError):
                    oracle.recovery_ledger(snapshot, record, real_backoff=True)

    def test_fallback_and_replay_share_one_charged_root(self):
        snapshot, record = snapshot_with_roots(1)
        snapshot["execution_protocol_attempts"].append({
            "agent_id": "parent", "attempt_id": "fallback",
            "payload_json": json.dumps({"recovery_of_attempt_id": "root-1"}),
        })
        self.assertEqual(oracle.recovery_root(snapshot, "parent", "fallback"), "root-1")
        self.assertEqual(len(oracle.recovery_ledger(snapshot, record)), 1)
        snapshot["task_result_recovery_admissions"][0]["root_attempt_id"] = "fallback"
        with self.assertRaises(AssertionError):
            oracle.recovery_ledger(snapshot, record)

    def exhausted(self):
        snapshot, record = snapshot_with_roots()
        record.update(state="deferred", disposition=None, settled_at=None,
                      recovery_paused_at="2026-10-25T13:15:00Z",
                      recovery_diagnostic_at="2026-10-25T13:15:01Z")
        snapshot["briefs"] = [{
            "evidence_id": "brief_task_result_recovery_result-identity-1_2",
            "kind": "failure", "agent_id": "parent", "message_id": "result-1",
            "work_item_id": None,
        }]
        return snapshot, record

    def test_exhaustion_accepts_failure_not_false_success(self):
        snapshot, record = self.exhausted()
        oracle.require_recovery_exhausted(snapshot, captured=record)
        for field, value in [
            ("settled_at", "2026-10-25T13:15:00Z"),
            ("disposition", "model_delivered"), ("state", "settled"),
            ("next_recheck_at", "2026-10-25T13:16:00Z"),
            ("recovery_paused_at", None), ("recovery_diagnostic_at", None),
        ]:
            with self.subTest(field=field):
                broken = copy.deepcopy(snapshot)
                broken["task_result_settlements"][0][field] = value
                with self.assertRaises(AssertionError):
                    oracle.require_recovery_exhausted(broken, captured=record)
        snapshot["briefs"] *= 2
        with self.assertRaises(AssertionError):
            oracle.require_recovery_exhausted(snapshot, captured=record)

    def test_restart_preserves_epoch_owner_ledger_and_deadline(self):
        before, captured = snapshot_with_roots(1)
        captured["next_recheck_at"] = "2026-10-25T13:01:00Z"
        oracle.require_restart_preserved(before, copy.deepcopy(before),
                                        captured=captured, backoff=True)
        for field, value in [
            ("work_item_id", "other"), ("rejoin_generation", 99),
            ("recovery_epoch", 3), ("recovery_attempts", 0),
            ("next_recheck_at", "2026-10-25T13:00:30Z"),
        ]:
            with self.subTest(field=field):
                after = copy.deepcopy(before)
                after["task_result_settlements"][0][field] = value
                with self.assertRaises(AssertionError):
                    oracle.require_restart_preserved(
                        before, after, captured=captured, backoff=True)

    def test_workitem_terminal_result_binding_is_exact(self):
        snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
        brief = snapshot["briefs"][0]
        brief["work_item_id"] = "owner"
        brief["payload_json"] = json.dumps({
            **oracle.payload(brief), "work_item_id": "owner",
        })
        snapshot["work_items"] = [{
            "work_item_id": "owner", "agent_id": "parent", "state": "completed",
            "completed_at": "2026-10-25T13:00:00Z",
            "payload_json": json.dumps({"result_brief_id": "brief-1"}),
        }]
        oracle.require_result_brief(
            snapshot, agent_id="parent", turn_id="reentry-turn",
            message_id="result-1", work_item_id="owner",
        )
        snapshot["work_items"][0]["payload_json"] = json.dumps({"result_brief_id": "foreign"})
        with self.assertRaises(AssertionError):
            oracle.require_result_brief(
                snapshot, agent_id="parent", turn_id="reentry-turn",
                message_id="result-1", work_item_id="owner",
            )

    def test_captured_identity_and_replay_ancestry_are_fenced(self):
        snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
        captured = copy.deepcopy(snapshot["task_result_settlements"][0])
        for field in oracle.IDENTITY_FIELDS:
            with self.subTest(field=field):
                wrong = {**captured, field: "foreign"}
                with self.assertRaises(AssertionError):
                    oracle.require_task_reentry(
                        snapshot, agent_id="parent", task_id="task-1",
                        parent_turn_id="parent-turn", captured=wrong,
                    )
        attempt_row = snapshot["execution_protocol_attempts"][0]
        original = oracle.payload(attempt_row)
        snapshot["execution_protocol_attempts"].append({
            **attempt_row, "attempt_id": "predecessor",
            "payload_json": json.dumps({**original, "attempt_id": "predecessor"}),
        })
        attempt_row["payload_json"] = json.dumps({
            **original, "recovery_of_attempt_id": "predecessor",
            "source": {"identity": {"kind": "runtime_recovery"}, "generation": 1},
        })
        oracle.require_task_reentry(
            snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
        )
        parent = oracle.payload(snapshot["execution_protocol_attempts"][-1])
        parent["binding"] = {"kind": "work_item", "work_item_id": "foreign"}
        snapshot["execution_protocol_attempts"][-1]["payload_json"] = json.dumps(parent)
        with self.assertRaises(AssertionError):
            oracle.require_task_reentry(
                snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
            )

    def test_duplicate_consumption_hidden_in_scanner_input_is_red(self):
        snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
        snapshot["turn_records"].append({
            "agent_id": "parent", "turn_id": "second-delivery",
            "terminal_kind": "completed", "trigger_message_id": "scanner-wake",
            "payload_json": json.dumps({"idle_input_batch": {
                "items": [{"message_id": "result-1"}],
            }}),
        })
        with self.assertRaisesRegex(AssertionError, "duplicate"):
            oracle.require_task_reentry(
                snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
            )

    def test_failed_typed_outcome_is_not_delivery_and_fifth_pause_is_not_a_rejection(self):
        snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
        snapshot["task_result_settlements"][0]["recovery_paused_at"] = "2026-10-25T13:00:00Z"
        oracle.require_task_reentry(
            snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
        )
        row = snapshot["execution_protocol_outcomes"][0]
        outcome = oracle.payload(row)
        outcome["outcome"]["outcome"]["kind"] = "failed"
        row["payload_json"] = json.dumps(outcome)
        with self.assertRaisesRegex(AssertionError, "typed owner outcome"):
            oracle.require_task_reentry(
                snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
            )


    def test_scanner_requires_typed_cause_and_actual_model_input_not_owner_wake(self):
        snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
        record = snapshot["task_result_settlements"][0]
        snapshot["messages"].append({
            "message_id": "scanner-wake", "payload_json": json.dumps({
                "kind": "internal_followup", "authority_class": "runtime_instruction",
                "origin": {"kind": "system", "subsystem": "task_result_recovery"},
                "source_refs": {
                    "task_result_identity": record["result_identity"],
                    "task_result_message_id": "result-1",
                    "task_result_parent_turn_id": "parent-turn",
                    "task_result_rejoin_generation": "1",
                },
            }),
        })
        turn = snapshot["turn_records"][0]
        turn["trigger_message_id"] = "scanner-wake"
        turn["payload_json"] = json.dumps({"idle_input_batch": {
            "activation_id": "attempt-1", "items": [{"message_id": "result-1"}],
        }})
        row = snapshot["execution_protocol_attempts"][0]
        attempt = oracle.payload(row)
        attempt.update(source_message_id="scanner-wake", source={
            "identity": {"kind": "internal_followup", "message_id": "scanner-wake"},
            "generation": 1,
        })
        row["payload_json"] = json.dumps(attempt)
        brief = snapshot["briefs"][0]
        brief["message_id"] = "scanner-wake"
        brief["payload_json"] = json.dumps({
            **oracle.payload(brief), "related_message_id": "scanner-wake",
        })
        oracle.require_task_reentry(
            snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
        )
        for mutation in ("no-model-input", "foreign-activation", "untrusted"):
            with self.subTest(mutation=mutation):
                broken = copy.deepcopy(snapshot)
                if mutation == "untrusted":
                    message = oracle.payload(broken["messages"][-1])
                    message["authority_class"] = "external_input"
                    broken["messages"][-1]["payload_json"] = json.dumps(message)
                else:
                    batch = json.loads(broken["turn_records"][0]["payload_json"])
                    if mutation == "no-model-input":
                        batch["idle_input_batch"]["items"] = []
                    else:
                        batch["idle_input_batch"]["activation_id"] = "foreign"
                    broken["turn_records"][0]["payload_json"] = json.dumps(batch)
                with self.assertRaises(AssertionError):
                    oracle.require_task_reentry(
                        broken, agent_id="parent", task_id="task-1",
                        parent_turn_id="parent-turn",
                    )


def scanner_lineage():
    snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
    snapshot["messages"].append({
        "message_id": "scanner-wake", "payload_json": json.dumps({
            "kind": "internal_followup", "authority_class": "runtime_instruction",
            "origin": {"kind": "system", "subsystem": "task_result_recovery"},
            "source_refs": {
                "task_result_identity": "result-identity-1",
                "task_result_message_id": "result-1",
                "task_result_parent_turn_id": "parent-turn",
                "task_result_rejoin_generation": "1",
            },
        }),
    })
    row = snapshot["execution_protocol_attempts"][0]
    attempt = oracle.payload(row)
    snapshot["execution_protocol_attempts"].append({
        **row, "attempt_id": "scanner-attempt",
        "payload_json": json.dumps({
            **attempt, "attempt_id": "scanner-attempt", "turn_id": "scanner-turn",
            "source_message_id": "scanner-wake",
            "source": {"identity": {
                "kind": "internal_followup", "message_id": "scanner-wake",
            }},
        }),
    })
    snapshot["turn_records"].append({
        "agent_id": "parent", "turn_id": "scanner-turn",
        "terminal_kind": "deferred_to_fallback", "current_work_item_id": None,
        "trigger_message_id": "scanner-wake",
        "payload_json": json.dumps({"idle_input_batch": {
            "activation_id": "scanner-attempt",
            "items": [{"message_id": "scanner-wake"}, {"message_id": "result-1"}],
        }}),
    })
    row["payload_json"] = json.dumps({
        **attempt, "source_message_id": "lineage-wake",
        "source": {"identity": {"kind": "runtime_recovery"}},
        "recovery_of_attempt_id": "scanner-attempt",
    })
    turn = snapshot["turn_records"][0]
    turn["trigger_message_id"] = "lineage-wake"
    turn["payload_json"] = json.dumps({"input_message_ids": ["lineage-wake", "scanner-wake"]})
    snapshot["messages"].append({
        "message_id": "lineage-wake", "payload_json": json.dumps({
            "kind": "internal_followup", "authority_class": "runtime_instruction",
            "origin": {"kind": "system", "subsystem": "model_lineage_recovery"},
            "source_refs": {"source_message_id": "scanner-wake", "source_turn_id": "scanner-turn"},
        }),
    })
    brief = snapshot["briefs"][0]
    brief["message_id"] = "lineage-wake"
    brief["payload_json"] = json.dumps({
        **oracle.payload(brief), "related_message_id": "lineage-wake",
    })
    return snapshot


class ModelLineageTests(unittest.TestCase):
    def check(self, snapshot):
        return oracle.require_task_reentry(
            snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
        )

    def test_complete_scanner_ancestry_preserves_actual_task_model_input(self):
        self.check(scanner_lineage())

    def test_already_completed_ancestor_is_duplicate_consumption(self):
        snapshot = scanner_lineage()
        snapshot["turn_records"][-1]["terminal_kind"] = "completed"
        with self.assertRaisesRegex(AssertionError, "duplicate"):
            self.check(snapshot)

    def test_scanner_only_and_unrelated_model_input_remain_red(self):
        snapshot = scanner_lineage()
        source_turn = snapshot["turn_records"][-1]
        batch = oracle.payload(source_turn)
        batch["idle_input_batch"]["items"] = [{"message_id": "scanner-wake"}]
        source_turn["payload_json"] = json.dumps(batch)
        with self.assertRaisesRegex(AssertionError, "source causality"):
            self.check(snapshot)
        unrelated = copy.deepcopy(source_turn)
        unrelated.update(turn_id="unrelated-turn", trigger_message_id="unrelated")
        unrelated["payload_json"] = json.dumps({"idle_input_batch": {
            "activation_id": "unrelated-attempt", "items": [{"message_id": "result-1"}],
        }})
        snapshot["turn_records"].append(unrelated)
        with self.assertRaisesRegex(AssertionError, "source causality"):
            self.check(snapshot)

    def test_lineage_owner_identity_batch_and_missing_or_cyclic_parent_are_fenced(self):
        for mutation in (
            "owner", "agent", "attempt-id", "batch-activation", "turn-owner",
            "turn-trigger", "missing-parent", "cycle",
        ):
            with self.subTest(mutation=mutation):
                snapshot = scanner_lineage()
                row = snapshot["execution_protocol_attempts"][-1]
                attempt = oracle.payload(row)
                turn = snapshot["turn_records"][-1]
                if mutation == "owner":
                    attempt["binding"] = {"kind": "work_item", "work_item_id": "foreign"}
                elif mutation == "agent":
                    attempt["agent_id"] = "foreign"
                elif mutation == "attempt-id":
                    attempt["attempt_id"] = "foreign"
                elif mutation == "batch-activation":
                    value = oracle.payload(turn)
                    value["idle_input_batch"]["activation_id"] = "foreign"
                    turn["payload_json"] = json.dumps(value)
                elif mutation == "turn-owner":
                    turn["current_work_item_id"] = "foreign"
                elif mutation == "turn-trigger":
                    turn["trigger_message_id"] = "foreign"
                elif mutation == "missing-parent":
                    snapshot["execution_protocol_attempts"].pop()
                else:
                    attempt.update(
                        source={"identity": {"kind": "runtime_recovery"}},
                        recovery_of_attempt_id="scanner-attempt",
                    )
                row["payload_json"] = json.dumps(attempt)
                with self.assertRaises(AssertionError):
                    self.check(snapshot)



class DatabaseCaptureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        sys.path.insert(0, str(ROOT / "tests/e2e/docker/openai_stub"))
        try:
            spec = importlib.util.spec_from_file_location(
                "task_result_fault_fixture", ROOT / "tests/e2e/docker/fault_proxy/server.py")
            cls.helper = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(cls.helper)
        finally:
            sys.path.pop(0)

    def test_task_snapshot_preserves_all_rows_without_changing_default_capture(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "runtime.sqlite"
            with sqlite3.connect(path) as db:
                for table in recovery.TABLES | {"queue_entries"}:
                    db.execute(f'CREATE TABLE "{table}" (id INTEGER PRIMARY KEY)')
                for table in ("messages", "queue_entries"):
                    db.executemany(f'INSERT INTO "{table}" VALUES (?)',
                                   ((i,) for i in range(1001)))
            before = path.read_bytes()
            for complete in (False, True):
                with self.subTest(task_result=complete), \
                        patch.object(self.helper, "Path", return_value=Path(tmp)), \
                        redirect_stdout(io.StringIO()) as output:
                    self.helper.database(task_result=complete)
                    captured = json.loads(output.getvalue())
                    if complete:
                        snapshot = recovery.unwrap_database(captured)
                        for table in ("messages", "queue_entries"):
                            self.assertEqual(len(snapshot[table]), 1001)
                            self.assertEqual(snapshot[table][-1]["id"], 1000)
                            self.assertEqual(captured["counts"][table], 1001)
                        self.assertEqual(captured["truncated_tables"], [])
                    else:
                        self.assertEqual(len(captured["rows"]["queue_entries"]), 500)
                        self.assertIn("queue_entries", captured["truncated_tables"])
            self.assertEqual(path.read_bytes(), before)


class ProviderFixtureTests(unittest.TestCase):
    def test_script_uses_create_promoted_command_exact_wait_and_original_owner_completion(self):
        scenario = fixture.RecoveryScenario()
        wid, tid = "work_000000000000001", "task_000000000000001"
        request = {"input": [{"text": f"{wid} {tid} SCHEDULER-TASK-WAIT-abcdef"}]}
        names, arguments = [], []
        for _ in range(4):
            status, result = scenario.consume(request)
            self.assertEqual(status, 200)
            for item in result["output"]:
                if item["type"] == "function_call":
                    names.append(item["name"])
                    arguments.append(json.loads(item["arguments"]))
        self.assertEqual(names, ["CreateWorkItem", "ExecCommand", "WaitFor"])
        self.assertEqual(arguments[-1]["resource"], tid)
        self.assertEqual(arguments[-1]["wake"], "task_result")
        self.assertEqual(scenario.phase, 4)
        scenario.task_wait()
        _, result = scenario.task_wait()
        self.assertEqual(result["output"][-1]["name"], "CompleteWorkItem")
        self.assertEqual(json.loads(result["output"][-1]["arguments"])["work_item_id"], wid)

    def test_real_command_is_promoted_behind_volume_gate(self):
        scenario = fixture.RecoveryScenario()
        scenario.phase = 2
        status, body = scenario.task_wait()
        self.assertEqual(status, 200)
        call = body["output"][0]
        self.assertEqual(call["name"], "ExecCommand")
        args = json.loads(call["arguments"])
        self.assertIn(fixture.COMMAND_GATE, args["cmd"])
        self.assertEqual(args["yield_time_ms"], 50)

    def test_provider_failure_never_advances_success_phase(self):
        for action in ("fail", "success"):
            with self.subTest(action=action), tempfile.TemporaryDirectory() as directory:
                control = Path(directory) / "control.json"
                scenario = fixture.RecoveryScenario()
                scenario.phase = 4
                scenario.work_ids = ["owner"]
                rows = []

                def record(**row):
                    rows.append(row)
                    if row["state"] == "waiting":
                        control.write_text(json.dumps({
                            "releases": {row["request_id"]: action},
                        }))

                with patch.object(fixture, "CONTROL", control):
                    status, body = scenario.consume_gated({}, "b", record)
                self.assertEqual(status, 400 if action == "fail" else 200)
                self.assertEqual(scenario.phase, 4 if action == "fail" else 5)
                self.assertEqual([row["state"] for row in rows], ["waiting", "released"])

    def test_unreleased_barrier_fails_closed(self):
        scenario = fixture.RecoveryScenario()
        scenario.phase = 4
        status, _ = scenario.consume_gated({}, "a", Mock(), timeout=0)
        self.assertEqual(status, 504)
        self.assertEqual(scenario.phase, 4)

    def test_failure_gate_stays_armed_across_primary_and_backup_requests(self):
        scenario = fixture.RecoveryScenario()
        scenario.phase = 4
        with tempfile.TemporaryDirectory() as directory:
            control = Path(directory) / "control.json"

            def record(**row):
                if row["state"] == "waiting":
                    control.write_text(json.dumps({
                        "releases": {row["request_id"]: "fail"},
                    }))

            with patch.object(fixture, "CONTROL", control):
                for side in ("a", "b"):
                    status, _ = scenario.consume_gated({}, side, record)
                    self.assertEqual(status, 400)
                    self.assertEqual(scenario.phase, 4)


class RecoveryRunnerTests(unittest.TestCase):
    def owned_snapshot(self, spec):
        snapshot, record = snapshot_with_roots(spec["goal"])
        if spec["outcome"] == "exhausted":
            snapshot, record = RecoveryOracleTests().exhausted()
        record["agent_id"], record["work_item_id"] = "qa", "owner"
        if spec["goal"]:
            attempt = oracle.payload(snapshot["execution_protocol_attempts"][0])
            attempt["recovery_of_attempt_id"] = f"root-{spec['goal']}"
            snapshot["execution_protocol_attempts"][0]["payload_json"] = json.dumps(attempt)
        for table in ("briefs", "wait_conditions", "turn_records",
                      "execution_protocol_attempts", "execution_protocol_outcomes"):
            for row in snapshot[table]:
                row["agent_id"] = "qa"
                if "work_item_id" in row:
                    row["work_item_id"] = "owner"
                if "current_work_item_id" in row:
                    row["current_work_item_id"] = "owner"
                if "payload_json" in row:
                    value = oracle.payload(row)
                    if "agent_id" in value:
                        value["agent_id"] = "qa"
                    if table == "briefs":
                        value["work_item_id"] = "owner"
                    if "binding" in value:
                        value["binding"] = {"kind": "work_item", "work_item_id": "owner"}
                    if table == "execution_protocol_outcomes":
                        value["outcome"] = {
                            "owner": "work_item", "outcome": {"kind": "complete"},
                        }
                    row["payload_json"] = json.dumps(value)
        task = oracle.payload(snapshot["tasks"][0])
        task["work_item_id"] = "owner"
        snapshot["tasks"][0]["payload_json"] = json.dumps(task)
        snapshot["work_items"] = [{
            "agent_id": "qa", "work_item_id": "owner",
            "state": "completed" if spec["outcome"] == "success" else "open",
            "completed_at": "2026-10-25T13:16:00Z",
            "payload_json": json.dumps({"result_brief_id": "brief-1"}),
        }]
        return snapshot, record

    def live_owner_snapshot(self, snapshot):
        snapshot = copy.deepcopy(snapshot)
        record = snapshot["task_result_settlements"][0]
        record.update(state="caller_admitted", settled_at=None, disposition=None,
                      next_recheck_at="2026-10-25T13:00:30Z")
        snapshot["execution_protocol_attempts"][0]["lifecycle_state"] = "open"
        snapshot["turn_records"][0]["terminal_kind"] = None
        return snapshot

    def test_live_owner_requires_open_bound_attempt_beyond_recheck_deadline(self):
        snapshot, record = self.owned_snapshot({"goal": 0, "outcome": "success"})
        snapshot = self.live_owner_snapshot(snapshot)
        captured = {key: record[key] for key in oracle.IDENTITY_FIELDS}
        observed = datetime(2026, 10, 25, 13, 0, 35, tzinfo=timezone.utc)

        def check(value, at=observed):
            return oracle.require_live_task_owner(
                value, captured=captured, activation_id=record["activation_id"], observed_at=at,
            )

        self.assertEqual(check(snapshot), record["activation_id"])
        with self.assertRaisesRegex(AssertionError, "beyond"):
            check(snapshot, datetime(2026, 10, 25, 13, 0, 29, tzinfo=timezone.utc))
        for mutation in ("settled", "closed", "terminal", "owner", "wake"):
            broken = copy.deepcopy(snapshot)
            if mutation == "settled":
                broken["task_result_settlements"][0]["state"] = "settled"
            elif mutation == "closed":
                broken["execution_protocol_attempts"][0]["lifecycle_state"] = "settled"
            elif mutation == "terminal":
                broken["turn_records"][0]["terminal_kind"] = "failed"
            elif mutation == "owner":
                row = broken["execution_protocol_attempts"][0]
                value = oracle.payload(row)
                value["binding"]["work_item_id"] = "foreign"
                row["payload_json"] = json.dumps(value)
            else:
                broken["messages"].append({
                    "message_id": "competing-wake", "payload_json": json.dumps({
                        "source_refs": {"task_result_identity": captured["result_identity"]},
                    }),
                })
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                check(broken)

    def test_fast_fallback_keeps_exact_task_result_root(self):
        spec = {"goal": 0, "outcome": "success", "fallback": True}
        snapshot, record = self.owned_snapshot(spec)
        row = snapshot["execution_protocol_attempts"][0]
        original = oracle.payload(row)
        snapshot["execution_protocol_attempts"].append({
            **row, "attempt_id": "task-root",
            "payload_json": json.dumps({**original, "attempt_id": "task-root"}),
        })
        row["payload_json"] = json.dumps({
            **original, "recovery_of_attempt_id": "task-root",
            "source": {"identity": {"kind": "runtime_recovery"}, "generation": 2},
        })
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(Namespace(evidence_dir=Path(directory)),
                                       "fallback-success", spec)
            run.captured = {key: record[key] for key in oracle.IDENTITY_FIELDS}
            run.joins = [{"side": "b", "action": "success", "recovery_attempts": 0}]
            (run.evidence / "database.json").write_text(json.dumps({
                "rows": snapshot, "truncated_tables": [],
            }))
            (run.evidence / f"{run.proxy}.log").write_text('{"fault": "rst"}\n')
            self.assertEqual(run.verify_frozen()["recovery_roots"], 0)
            parent = oracle.payload(snapshot["execution_protocol_attempts"][-1])
            parent["source"]["identity"]["kind"] = "queue_message"
            snapshot["execution_protocol_attempts"][-1]["payload_json"] = json.dumps(parent)
            (run.evidence / "database.json").write_text(json.dumps({
                "rows": snapshot, "truncated_tables": [],
            }))
            with self.assertRaises(AssertionError):
                run.verify_frozen()

    def test_each_manifest_case_runs_its_frozen_oracle(self):
        manifest = json.loads(recovery.MANIFEST.read_text())
        for case, spec in manifest["cases"].items():
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                run = recovery.RecoveryRun(Namespace(evidence_dir=Path(directory)), case, spec)
                snapshot, record = self.owned_snapshot(spec)
                run.captured = {key: record[key] for key in oracle.IDENTITY_FIELDS}
                run.restarted = bool(spec.get("restart"))
                run.joins = [{
                    "root_attempt_id": row["root_attempt_id"], "recovery_attempts": index + 1,
                    "side": "b" if spec.get("fallback") else "a",
                    "action": "success" if spec["outcome"] == "success"
                    and index + 1 == spec["goal"] else "fail",
                } for index, row in enumerate(snapshot["task_result_recovery_admissions"])]
                if not spec["goal"]:
                    run.joins.append({
                        "root_attempt_id": "attempt-1", "recovery_attempts": 0,
                        "action": "success", "side": "b" if spec.get("fallback") else "a",
                    })
                (run.evidence / "database.json").write_text(json.dumps({
                    "rows": snapshot, "truncated_tables": [],
                }))
                (run.evidence / f"{run.proxy}.log").write_text('{"fault": "rst"}\n')
                if spec.get("alive_owner"):
                    active = self.live_owner_snapshot(snapshot)
                    for checkpoint in ("before", "after"):
                        (run.evidence / f"alive-owner-{checkpoint}.json").write_text(
                            json.dumps(active))
                    (run.evidence / "alive-owner-observed.json").write_text(json.dumps({
                        "at": "2026-10-25T13:00:35Z",
                        "activation_id": record["activation_id"],
                    }))
                result = run.verify_frozen()
                self.assertEqual(result["status"], "verified-task-result-isolated")
                self.assertEqual(result["recovery_roots"], spec["goal"])
                if spec.get("restart"):
                    run.restarted = False
                    with self.assertRaisesRegex(AssertionError, "checkpoint"):
                        run.verify_frozen()
                if spec["goal"]:
                    run.joins = []
                    with self.assertRaisesRegex(AssertionError, "provider outcome"):
                        run.verify_frozen()

    def test_backoff_restart_uses_exact_same_volume_and_candidate(self):
        with tempfile.TemporaryDirectory() as directory:
            args = Namespace(evidence_dir=Path(directory), image="sha256:candidate",
                             python_image="sha256:python", expected_revision="12345678")
            run = recovery.RecoveryRun(args, "restart-backoff",
                                       {"goal": 5, "outcome": "success", "restart": "backoff"})
            before, captured = self.owned_snapshot({"goal": 1, "outcome": "success"})
            captured["next_recheck_at"] = "2026-10-25T13:01:00Z"
            run.captured = {key: captured[key] for key in oracle.IDENTITY_FIELDS}
            run.networks = ["control", "front", "back"]
            run.containers = [run.daemon]
            run.daemon_env = {}
            run.wait_ready = Mock()
            run.snapshot = Mock(return_value=copy.deepcopy(before))
            run.barriers = Mock(return_value=[])

            def docker(*command, **kwargs):
                output = ""
                if command[0] == "inspect":
                    output = json.dumps([{
                        "Image": args.image,
                        "Mounts": [{"Name": run.volume, "Destination": "/var/lib/holon"}],
                        "NetworkSettings": {"Networks": {"control": {}, "front": {}}},
                    }])
                elif command[:3] == ("exec", run.daemon, "holon"):
                    output = "holon 0.54.1 (12345678)"
                return subprocess.CompletedProcess(command, 0, output, "")

            run.docker = Mock(side_effect=docker)
            run.restart(before, backoff=True)
            self.assertTrue(run.restarted)
            run.docker.assert_any_call("kill", "--signal", "KILL", run.daemon)
            launches = [call.args for call in run.docker.call_args_list
                        if call.args[:2] == ("run", "-d")]
            self.assertEqual(len(launches), 1)
            self.assertIn(f"{run.volume}:/var/lib/holon", launches[0])
            self.assertIn(args.image, launches[0])

    def test_manifest_preserves_policy_and_declares_uncontrolled_gaps(self):
        manifest = recovery.validate_manifest(json.loads(recovery.MANIFEST.read_text()))
        self.assertEqual(len(manifest["suites"]["slow"]), 4)
        self.assertTrue(manifest["pending_coverage"])
        manifest["backoff_seconds"][0] = 1
        with self.assertRaises(AssertionError):
            recovery.validate_manifest(manifest)

    def test_release_slow_gate_cannot_be_softened_or_hidden_behind_120_seconds(self):
        workflow = (ROOT / ".github/workflows/release-e2e.yml").read_text()
        gate = workflow.split("  task-result-recovery-required:\n", 1)[1].split("  core:\n", 1)[0]
        self.assertNotIn("continue-on-error", gate)
        self.assertIn("timeout-minutes: 40", gate)
        self.assertIn("--timeout 1800", gate)
        for name in json.loads(recovery.MANIFEST.read_text())["suites"]["slow"]:
            self.assertIn(name, gate)
        self.assertIn("needs: [candidate, task-result-recovery-required]", workflow)
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("--suite fast --timeout 180", ci)
        self.assertIn("target/docker-e2e/task-result-fast", ci)

    def test_cli_rejects_120_second_slow_gate_before_docker(self):
        with patch.object(recovery, "command") as command:
            with self.assertRaises(SystemExit) as error:
                recovery.main([
                    "--image", "candidate", "--python-image", "fixture",
                    "--expected-revision", "12345678", "--evidence-dir", "unused",
                    "--suite", "slow", "--timeout", "120",
                ])
            self.assertEqual(error.exception.code, 2)
            command.assert_not_called()

    def test_snapshot_truncation_or_missing_tables_cannot_pass(self):
        for database in (
            {"rows": {key: [] for key in recovery.TABLES}, "truncated_tables": ["tasks"]},
            {"rows": {}, "truncated_tables": []},
        ):
            with self.assertRaises(AssertionError):
                recovery.unwrap_database(database)

    def test_cleanup_failure_and_frozen_oracle_failure_stay_red(self):
        for cleanup_errors, oracle_error in ((["failed cleanup"], False), ([], True)):
            with self.subTest(cleanup=cleanup_errors), tempfile.TemporaryDirectory() as directory:
                run = Mock()
                run.evidence = Path(directory) / "case"
                run.run.return_value = {"case": "exact-success", "status": "awaiting-frozen-oracle"}
                run.cleanup.return_value = cleanup_errors
                run.verify_frozen.side_effect = AssertionError("bad settlement") if oracle_error else None
                with patch.object(recovery, "RecoveryRun", return_value=run), patch.object(
                    recovery, "command", return_value=subprocess.CompletedProcess([], 0, "sha256:id\n", "")
                ):
                    self.assertEqual(recovery.main([
                        "--image", "candidate", "--python-image", "fixture",
                        "--expected-revision", "12345678", "--evidence-dir", directory,
                        "--case", "exact-success",
                    ]), 1)
                run.cleanup.assert_called_once()
                if cleanup_errors:
                    run.verify_frozen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
