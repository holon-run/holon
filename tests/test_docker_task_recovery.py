"""Formal runner/fixture/oracle regressions; no Docker or wall-clock backoff needed."""
from __future__ import annotations

import copy
import importlib
import io
import json
import re
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
                                   "source_message_id": f"message-root-{index + 1}",
                                   "binding": {"kind": "work_item",
                                               "work_item_id": record["work_item_id"]},
                                   "recovery_of_attempt_id": None}),
    } for index in range(count)]
    return snapshot, record


class RecoveryOracleTests(unittest.TestCase):
    def replay_snapshot(self):
        snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
        ingress = snapshot["turn_records"][0]
        replay = copy.deepcopy(ingress)
        ingress["terminal_kind"] = "interrupted"
        replay["turn_id"] += "-replay"
        attempt = oracle.payload(snapshot["execution_protocol_attempts"][0])
        snapshot["execution_protocol_attempts"].append({
            **snapshot["execution_protocol_attempts"][0],
            "attempt_id": "interrupted-predecessor",
            "lifecycle_state": "interrupted",
            "payload_json": json.dumps({
                **attempt, "attempt_id": "interrupted-predecessor",
            }),
        })
        attempt["recovery_of_attempt_id"] = "interrupted-predecessor"
        snapshot["execution_protocol_attempts"][0]["payload_json"] = json.dumps(attempt)
        replay["payload_json"] = json.dumps({
            "replay": {
                "source_message_id": attempt["source_message_id"],
                "source_turn_id": ingress["turn_id"],
                "reason": "interrupted_queue_claim_reentry",
                "prior_terminal": {"kind": "interrupted"},
            },
            "idle_input_batch": {
                "activation_id": attempt["attempt_id"],
                "items": [{"message_id": attempt["source_message_id"]}],
            },
        })
        snapshot["turn_records"].append(replay)
        brief = snapshot["briefs"][0]
        brief["turn_id"] = replay["turn_id"]
        brief["payload_json"] = json.dumps({
            **oracle.payload(brief), "turn_id": replay["turn_id"],
        })
        return snapshot

    def test_interrupted_ingress_turn_is_not_the_replay_execution_turn(self):
        oracle.require_task_reentry(
            self.replay_snapshot(), agent_id="parent", task_id="task-1",
            parent_turn_id="parent-turn",
        )

    def test_replay_execution_requires_exact_ingress_batch_source_and_owner(self):
        for mutation in ("batch", "source", "ingress", "owner", "terminal", "duplicate"):
            snapshot = self.replay_snapshot()
            replay = snapshot["turn_records"][-1]
            body = oracle.payload(replay)
            if mutation == "batch":
                body["idle_input_batch"]["activation_id"] = "foreign"
            elif mutation == "source":
                body["replay"]["source_message_id"] = "foreign"
            elif mutation == "ingress":
                body["replay"]["source_turn_id"] = "foreign"
            elif mutation == "owner":
                replay["current_work_item_id"] = "foreign"
            elif mutation == "terminal":
                snapshot["turn_records"][0]["terminal_kind"] = "completed"
            else:
                snapshot["turn_records"].append(copy.deepcopy(replay))
            replay["payload_json"] = json.dumps(body)
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                oracle.require_task_reentry(
                    snapshot, agent_id="parent", task_id="task-1",
                    parent_turn_id="parent-turn",
                )

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
        for turn in snapshot["turn_records"]:
            turn["terminal_kind"] = "failed"
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

    def test_exhaustion_rejects_consumption_but_not_structured_model_suppression(self):
        snapshot, captured = self.exhausted()
        snapshot["turn_records"].append({
            "agent_id": "parent", "turn_id": "suppressed-source",
            "terminal_kind": "completed", "trigger_message_id": "result-1",
            "payload_json": json.dumps({
                "input_message_ids": ["result-1"], "external_side_effect_possible": False,
                "terminal": {
                    "kind": "completed", "reason": "reducer_only/model_reentry_suppressed",
                    "no_brief_reason": {
                        "kind": "reducer_only", "reason": "model_reentry_suppressed",
                    },
                },
            }),
        })
        oracle.require_recovery_exhausted(snapshot, captured=captured)
        for mutation in ("reason-only", "mismatched-reason", "unknown-reason", "side-effect",
                         "model-input", "live-input", "brief", "completion", "stored-brief"):
            broken = copy.deepcopy(snapshot)
            stored = oracle.payload(broken["turn_records"][-1])
            if mutation == "reason-only":
                del stored["terminal"]["no_brief_reason"]
            elif mutation == "mismatched-reason":
                stored["terminal"]["no_brief_reason"]["reason"] = "parent_turn_already_delivered"
            elif mutation == "unknown-reason":
                stored["terminal"]["reason"] = "reducer_only/unknown"
                stored["terminal"]["no_brief_reason"]["reason"] = "unknown"
            elif mutation == "side-effect":
                stored["external_side_effect_possible"] = True
            elif mutation == "model-input":
                stored["idle_input_batch"] = {"items": [{"message_id": "result-1"}]}
            elif mutation == "live-input":
                stored["live_input_batches"] = [{"items": [{"message_id": "result-1"}]}]
            elif mutation == "brief":
                stored["produced_brief_ids"] = ["real-brief"]
            elif mutation == "completion":
                stored["completed_work_item_ids"] = ["owner"]
            else:
                broken["briefs"].append({
                    "agent_id": "parent", "turn_id": "suppressed-source", "kind": "result",
                    "evidence_id": "unlisted-brief", "message_id": "foreign",
                })
            broken["turn_records"][-1]["payload_json"] = json.dumps(stored)
            with self.subTest(mutation=mutation), self.assertRaisesRegex(
                AssertionError, "successful consumption",
            ):
                oracle.require_recovery_exhausted(broken, captured=captured)

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

    def test_reducer_only_duplicate_notification_is_not_successful_consumption(self):
        snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
        duplicate = {
            "agent_id": "parent", "turn_id": "reduced-notification",
            "terminal_kind": "completed", "trigger_message_id": "result-1",
            "payload_json": json.dumps({
                "turn_id": "reduced-notification", "input_message_ids": ["result-1"],
                "external_side_effect_possible": False,
                "terminal": {
                    "kind": "completed", "reason": "reducer_only/parent_turn_already_delivered",
                    "no_brief_reason": {
                        "kind": "reducer_only", "reason": "parent_turn_already_delivered",
                    },
                },
            }),
        }
        snapshot["turn_records"].append(duplicate)

        def check(value=snapshot):
            return oracle.require_task_reentry(
                value, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
            )

        check()
        for mutation in ("reason-only", "side-effect", "model-input", "brief", "completion"):
            broken = copy.deepcopy(snapshot)
            stored = oracle.payload(broken["turn_records"][-1])
            if mutation == "reason-only":
                del stored["terminal"]["no_brief_reason"]
            elif mutation == "side-effect":
                stored["external_side_effect_possible"] = True
            elif mutation == "model-input":
                stored["idle_input_batch"] = {"items": [{"message_id": "result-1"}]}
            elif mutation == "brief":
                stored["produced_brief_ids"] = ["real-brief"]
            else:
                stored["completed_work_item_ids"] = ["owner"]
            broken["turn_records"][-1]["payload_json"] = json.dumps(stored)
            with self.subTest(mutation=mutation), self.assertRaisesRegex(AssertionError, "duplicate"):
                check(broken)

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
            "activation_id": "attempt-1", "items": [{"message_id": "scanner-wake"}],
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
        for mutation in ("no-model-input", "unrelated-model-input",
                         "foreign-activation", "untrusted"):
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
                    elif mutation == "unrelated-model-input":
                        batch["idle_input_batch"]["items"] = [
                            {"message_id": "result-1"},
                        ]
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
            "items": [{"message_id": "scanner-wake"}],
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
    def test_only_explicit_unwaited_scanner_mode_accepts_no_wait(self):
        snapshot = scanner_lineage()
        snapshot["wait_conditions"] = []
        with self.assertRaises(AssertionError):
            self.check(snapshot)
        oracle.require_task_reentry(
            snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
            wait_policy="none",
        )

    def test_unwaited_mode_rejects_reserved_wait_and_direct_or_untyped_source(self):
        for mutation in ("wait", "direct", "untyped"):
            snapshot = scanner_lineage()
            if mutation != "wait":
                snapshot["wait_conditions"] = []
            if mutation == "direct":
                snapshot = runner_tests.LiveAcceptanceEvidenceTests().task_snapshot()
                snapshot["wait_conditions"] = []
            if mutation == "untyped":
                message = next(row for row in snapshot["messages"]
                               if row["message_id"] == "scanner-wake")
                body = oracle.payload(message)
                body["source_refs"]["task_result_identity"] = "foreign"
                message["payload_json"] = json.dumps(body)
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                oracle.require_task_reentry(
                    snapshot, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn",
                    wait_policy="none",
                )

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

    def test_scanner_wake_must_be_the_activation_model_input(self):
        snapshot = scanner_lineage()
        source_turn = snapshot["turn_records"][-1]
        batch = oracle.payload(source_turn)
        batch["idle_input_batch"]["items"] = [{"message_id": "result-1"}]
        source_turn["payload_json"] = json.dumps(batch)
        with self.assertRaisesRegex(AssertionError, "source causality"):
            self.check(snapshot)
        unrelated = copy.deepcopy(source_turn)
        unrelated.update(turn_id="unrelated-turn", trigger_message_id="unrelated")
        unrelated["payload_json"] = json.dumps({"idle_input_batch": {
            "activation_id": "unrelated-attempt", "items": [{"message_id": "scanner-wake"}],
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
                        patch.object(self.helper, "Path", side_effect=lambda value:
                                     Path(tmp) if value == "/evidence-home" else Path(value)), \
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


class ScannerSetupTests(unittest.TestCase):
    def setup_snapshot(self):
        return {
            "tasks": [{"owner_agent_id": "qa", "payload_json": json.dumps({
                "id": "real-task", "agent_id": "qa",
                "status": "running", "work_item_id": "owner",
                "detail": {"parent_turn_id": "parent", "rejoin_generation": 7},
            })}],
            "work_items": [{"work_item_id": "owner",
                "state": "open", "plan_status": "ready", "blocked_by": None,
            }],
            "execution_protocol_attempts": [], "wait_conditions": [],
        }

    def test_scanner_setup_captures_real_task_without_a_reserved_wait(self):
        self.assertEqual(recovery.scanner_setup_identity(self.setup_snapshot()), {
            "agent_id": "qa", "task_id": "real-task", "work_item_id": "owner",
            "parent_turn_id": "parent", "rejoin_generation": 7,
        })

    def test_scanner_setup_waits_for_setup_execution_to_close(self):
        snapshot = self.setup_snapshot()
        snapshot["execution_protocol_attempts"] = [{"agent_id": "qa", "lifecycle_state": "open"}]
        self.assertIsNone(recovery.scanner_setup_identity(snapshot))

    def test_scanner_setup_rejects_blocked_closed_or_reserved_owner(self):
        for field, value in (("state", "completed"), ("plan_status", "draft"),
                             ("blocked_by", "independent dependency")):
            snapshot = self.setup_snapshot()
            owner = snapshot["work_items"][0]
            owner[field] = value
            with self.subTest(field=field), self.assertRaises(AssertionError):
                recovery.scanner_setup_identity(snapshot)
        for status in ("active", "triggered"):
            snapshot = self.setup_snapshot()
            snapshot["wait_conditions"] = [{
                "agent_id": "qa", "work_item_id": "owner", "status": status,
            }]
            with self.subTest(status=status), self.assertRaises(AssertionError):
                recovery.scanner_setup_identity(snapshot)


class ProviderFixtureTests(unittest.TestCase):
    def test_scanner_first_barrier_recognizes_only_observed_lineage_successors(self):
        source = {
            "id": "scanner-source", "kind": "internal_followup",
            "origin": {"kind": "system", "subsystem": "task_result_recovery"},
        }
        successor = {
            "id": "fallback-source", "kind": "internal_followup",
            "origin": {"kind": "system", "subsystem": "model_lineage_recovery"},
            "authority_class": "runtime_instruction",
            "source_refs": {"source_message_id": "scanner-source", "source_turn_id": "failed-turn"},
        }
        for mutation in (None, "foreign-source", "missing-turn", "wrong-origin", "wrong-authority"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                scenario = fixture.RecoveryScenario()
                scenario.phase, scenario.scanner_first = 4, True
                replay = copy.deepcopy(successor)
                if mutation == "foreign-source":
                    replay["source_refs"]["source_message_id"] = "foreign"
                elif mutation == "missing-turn":
                    del replay["source_refs"]["source_turn_id"]
                elif mutation == "wrong-origin":
                    replay["origin"]["subsystem"] = "other"
                elif mutation == "wrong-authority":
                    replay["authority_class"] = "external"
                with patch.object(fixture, "CONTROL", Path(directory) / "missing.json"), patch.object(
                    scenario, "current_input_items",
                    side_effect=[[{"body": "pending result", "provenance": source}],
                                 [{"body": "recover failed turn", "provenance": replay}]],
                ):
                    scenario.consume_gated({}, "a", Mock(), timeout=0)
                    record = Mock()
                    status, _ = scenario.consume_gated({}, "b", record, timeout=0)
                self.assertEqual(status, 504 if mutation is None else 200)
                self.assertEqual(record.called, mutation is None)
                self.assertEqual(scenario.phase, 4)
                if mutation is None:
                    self.assertEqual(
                        record.call_args_list[0].kwargs["source_message_ids"],
                        ["fallback-source"],
                    )

    def test_scanner_first_owner_resume_does_not_open_recovery_barrier(self):
        for provenance, recovery_input in (
            ({"kind": "work_item_resume"}, False),
            ({"kind": "internal_followup", "origin": {"kind": "system", "subsystem": "other"}}, False),
            ({"kind": "task_result"}, True),
            ({"kind": "internal_followup",
              "origin": {"kind": "system", "subsystem": "task_result_recovery"}}, True),
        ):
            with self.subTest(provenance=provenance), tempfile.TemporaryDirectory() as directory:
                scenario = fixture.RecoveryScenario()
                scenario.phase, scenario.scanner_first = 4, True
                record = Mock()
                with patch.object(fixture, "CONTROL", Path(directory) / "missing.json"), patch.object(
                    scenario, "current_input_items",
                    return_value=[{"body": "Handle the pending task results", "provenance": provenance}],
                ):
                    status, _ = scenario.consume_gated({}, "a", record, timeout=0)
                self.assertEqual(status, 504 if recovery_input else 200)
                self.assertEqual(scenario.phase, 4)
                self.assertEqual(record.called, recovery_input)

    def test_only_scanner_first_setup_leaves_owner_runnable_without_waitfor(self):
        for scanner_first in (False, True):
            scenario = fixture.RecoveryScenario()
            scenario.phase = 3
            scenario.work_ids = ["work_000000000000001"]
            scenario.task_ids = ["task_000000000000001"]
            with tempfile.TemporaryDirectory() as directory:
                control = Path(directory) / "control.json"
                control.write_text(json.dumps({"scanner_first": scanner_first}))
                with patch.object(fixture, "CONTROL", control):
                    status, result = scenario.consume_gated({}, "A", Mock())
            self.assertEqual(status, 200)
            self.assertEqual(scenario.phase, 4)
            calls = [item for item in result["output"] if item["type"] == "function_call"]
            self.assertEqual([call["name"] for call in calls],
                             [] if scanner_first else ["WaitFor"])

    def test_fairness_script_uses_new_real_task_and_parks_without_completing_owner(self):
        scenario = fixture.RecoveryScenario()
        scenario.fairness, scenario.phase = True, 6
        scenario.work_ids = ["work_000000000000001"]
        scenario.task_ids = ["task_000000000000001", "task_000000000000002"]
        calls = []
        for _ in range(4):
            status, result = scenario.consume({})
            self.assertEqual(status, 200)
            calls.extend(item for item in result["output"] if item["type"] == "function_call")
        self.assertEqual([call["name"] for call in calls],
                         ["ExecCommand", "WaitFor", "WaitFor"])
        self.assertIn(fixture.FAIRNESS_GATE, json.loads(calls[0]["arguments"])["cmd"])
        self.assertEqual(json.loads(calls[1]["arguments"])["resource"], scenario.task_ids[-1])
        self.assertEqual(json.loads(calls[2]["arguments"]), {
            "wake": "operator_input", "delivery": "final",
            "reason": "preserve the shared owner and its exhausted obligation",
        })
        self.assertEqual(scenario.phase, scenario.expected_phase())

    def test_fairness_transition_requires_fixture_control_and_explicit_current_owner_prompt(self):
        with tempfile.TemporaryDirectory() as directory:
            control = Path(directory) / "control.json"
            control.write_text(json.dumps({"fairness_owner": "owner"}))
            scenario = fixture.RecoveryScenario()
            scenario.phase, scenario.work_ids = 4, ["owner"]
            with patch.object(fixture, "CONTROL", control), patch.object(
                scenario, "current_input_items",
                return_value=[{
                    "body": "SCHEDULER-TASK-FAIRNESS under owner",
                    "provenance": {"kind": "operator"},
                }],
            ):
                status, body = scenario.consume_gated({}, "a", Mock())
            self.assertEqual(status, 200)
            self.assertTrue(scenario.fairness)
            self.assertEqual(body["output"][0]["name"], "ExecCommand")
            for configured, body in (
                (False, "SCHEDULER-TASK-FAIRNESS under owner"),
                (True, "SCHEDULER-TASK-FAIRNESS under foreign"),
                (True, "an unrelated prompt under owner"),
            ):
                if configured:
                    control.write_text(json.dumps({"fairness_owner": "owner"}))
                else:
                    control.unlink()
                scenario = fixture.RecoveryScenario()
                scenario.phase = 4
                with patch.object(fixture, "CONTROL", control), patch.object(
                    scenario, "current_input_items",
                    return_value=[{"body": body, "provenance": {"kind": "operator"}}],
                ):
                    status, _ = scenario.consume_gated({}, "a", Mock(), timeout=0)
                self.assertEqual(status, 504)
                self.assertFalse(scenario.fairness)

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
                self.assertEqual(status, 503 if action == "fail" else 200)
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
                    self.assertEqual(status, 503)
                    self.assertEqual(scenario.phase, 4)


class RecoveryRunnerTests(unittest.TestCase):
    def test_provider_barrier_uses_actual_source_and_ancestry_not_latest_settlement(self):
        snapshot, record = self.owned_snapshot({"goal": 5, "outcome": "success"})
        snapshot["execution_protocol_attempts"].append({
            "agent_id": "qa", "attempt_id": "late-fallback",
            "payload_json": json.dumps({
                "source_message_id": "late-source", "recovery_of_attempt_id": "root-3",
                "binding": {"kind": "work_item", "work_item_id": "owner"},
            }),
        })
        check = lambda value, ids: recovery.provider_barrier_identity(value, record, ids)
        self.assertEqual(check(snapshot, ["late-source"]), {
            "activation_id": "late-fallback", "root_attempt_id": "root-3",
            "recovery_attempts": 3,
        })
        self.assertEqual(check(snapshot, ["message-root-5"])["recovery_attempts"], 5)
        self.assertIsNone(check(snapshot, ["not-captured-yet"]))
        for mutation in ("missing", "ambiguous", "owner", "ancestor-owner", "foreign-root"):
            broken = copy.deepcopy(snapshot)
            ids = ["late-source"]
            if mutation == "missing":
                ids = []
            elif mutation == "ambiguous":
                ids.append("message-root-5")
            else:
                row = (broken["execution_protocol_attempts"][-1] if mutation != "ancestor-owner"
                       else next(row for row in broken["execution_protocol_attempts"]
                                 if row["attempt_id"] == "root-3"))
                value = oracle.payload(row)
                if mutation == "foreign-root":
                    value["recovery_of_attempt_id"] = "attempt-1"
                    root = broken["execution_protocol_attempts"][0]
                    stored = oracle.payload(root)
                    stored["source"]["identity"]["task_id"] = "foreign-task"
                    stored["recovery_of_attempt_id"] = None
                    root["payload_json"] = json.dumps(stored)
                else:
                    value["binding"]["work_item_id"] = "foreign-owner"
                row["payload_json"] = json.dumps(value)
            with self.subTest(mutation=mutation), self.assertRaisesRegex(
                AssertionError, "provider barrier",
            ):
                check(broken, ids)

    def test_exhausted_case_never_waits_for_a_successful_release(self):
        snapshot, record = self.owned_snapshot({"goal": 5, "outcome": "exhausted"})
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(
                Namespace(evidence_dir=Path(directory)), "restart-fifth-exhausted",
                {"goal": 5, "outcome": "exhausted"},
            )
            # A replayed provider request reports its real charged root and the
            # budget it observed; it is not relabeled as the fifth root request.
            run.joins = [{"root_attempt_id": "root-1", "action": "fail",
                          "budget_before_release": 5}]
            self.assertTrue(run.recovery_outcomes_complete(snapshot, record))
            run.joins.append({"root_attempt_id": "root-1", "action": "success",
                              "budget_before_release": 5})
            self.assertFalse(run.recovery_outcomes_complete(snapshot, record))
            uncharged, uncharged_record = self.owned_snapshot(
                {"goal": 5, "outcome": "exhausted"})
            uncharged["task_result_recovery_admissions"].pop()
            uncharged_record["recovery_attempts"] = 4
            uncharged_record["last_recovery_root_attempt_id"] = "root-4"
            self.assertFalse(run.recovery_outcomes_complete(uncharged, uncharged_record))

    def test_exhausted_observation_waits_for_the_required_restart(self):
        snapshot, record = self.owned_snapshot({"goal": 5, "outcome": "exhausted"})
        with tempfile.TemporaryDirectory() as directory:
            for checkpoint in ("fifth", "backoff"):
                with self.subTest(checkpoint=checkpoint):
                    run = recovery.RecoveryRun(
                        Namespace(evidence_dir=Path(directory)), "restart-fifth-exhausted",
                        {"goal": 5, "outcome": "exhausted", "restart": checkpoint},
                    )
                    # The fifth root is charged but still cooling before its
                    # barrier; only an older lineage has released a failure.
                    run.joins = [{"root_attempt_id": "root-1", "action": "fail",
                                  "budget_before_release": 5}]
                    self.assertFalse(run.recovery_outcomes_complete(snapshot, record))
                    run.restarted = True
                    self.assertTrue(run.recovery_outcomes_complete(snapshot, record))
                    run.joins.append({"root_attempt_id": "root-5", "action": "success",
                                      "budget_before_release": 5})
                    self.assertFalse(run.recovery_outcomes_complete(snapshot, record))

    def test_success_release_requires_the_fully_charged_budget(self):
        snapshot, record = self.owned_snapshot({"goal": 5, "outcome": "success"})
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(
                Namespace(evidence_dir=Path(directory)), "fifth-success",
                {"goal": 5, "outcome": "success"},
            )
            run.joins = [{"root_attempt_id": "root-1", "action": "fail",
                          "budget_before_release": 1}]
            self.assertFalse(run.recovery_outcomes_complete(snapshot, record))
            run.joins.append({"root_attempt_id": "root-1", "action": "success",
                              "budget_before_release": 4})
            self.assertFalse(run.recovery_outcomes_complete(snapshot, record))
            run.joins[-1] = {**run.joins[-1], "budget_before_release": 5}
            self.assertTrue(run.recovery_outcomes_complete(snapshot, record))

    def test_restart_replay_resolves_to_the_live_attempt_on_one_chain(self):
        snapshot, record = self.owned_snapshot({"goal": 5, "outcome": "success"})
        original = next(row for row in snapshot["execution_protocol_attempts"]
                        if row["attempt_id"] == "root-5")
        stored = oracle.payload(original)
        stored["terminal_at"] = "2026-10-25T13:15:00Z"
        original["payload_json"] = json.dumps(stored)
        snapshot["execution_protocol_attempts"].append({
            "agent_id": "qa", "attempt_id": "replay-5",
            "payload_json": json.dumps({
                "source_message_id": "message-root-5",
                "recovery_of_attempt_id": "root-5",
                "binding": {"kind": "work_item", "work_item_id": "owner"},
            }),
        })
        identity = recovery.provider_barrier_identity(
            snapshot, record, ["message-root-5"])
        self.assertEqual(identity["activation_id"], "replay-5")
        self.assertEqual(identity["recovery_attempts"], 5)
        replay = snapshot["execution_protocol_attempts"][-1]
        value = oracle.payload(replay)
        value["terminal_at"] = "2026-10-25T13:16:00Z"
        replay["payload_json"] = json.dumps(value)
        with self.assertRaisesRegex(AssertionError, "only matched terminal attempts"):
            recovery.provider_barrier_identity(snapshot, record, ["message-root-5"])

    def test_released_barrier_cannot_trigger_another_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(
                Namespace(evidence_dir=Path(directory)), "restart-fifth",
                {"goal": 5, "outcome": "success"},
            )
            run.docker = Mock(return_value=Namespace(stdout=json.dumps({
                "layer": "recovery_barrier", "state": "waiting", "request_id": "old",
            })))
            self.assertEqual(len(run.barriers()), 1)
            run.releases["old"] = "fail"
            self.assertEqual(run.barriers(), [])

    def test_pending_provider_request_is_observed_before_its_snapshot(self):
        # The daemon charges the recovery root before issuing the provider
        # request, so the snapshot that explains a pending barrier must be
        # taken after the barrier was read, never before it.
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(
                Namespace(evidence_dir=Path(directory)), "restart-fifth-exhausted",
                {"goal": 5, "outcome": "exhausted"},
            )
            calls = []
            run.barriers = Mock(side_effect=lambda: calls.append("barriers") or ["barrier"])
            run.snapshot = Mock(side_effect=lambda label: calls.append(label) or "rows")
            barriers, snapshot = run.observe_provider_request()
            self.assertEqual(calls, ["barriers", "observation"])
            self.assertEqual((barriers, snapshot), (["barrier"], "rows"))

    def test_zero_budget_success_releases_only_an_initial_result_root_once(self):
        manifest = recovery.validate_manifest(json.loads(recovery.MANIFEST.read_text()))
        for case in ("exact-success", "fallback-success", "alive-owner",
                     "controlled-exact-first"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                run = recovery.RecoveryRun(
                    Namespace(evidence_dir=Path(directory)), case, manifest["cases"][case],
                )
                record = {"recovery_attempts": 0, "last_recovery_root_attempt_id": None}
                barrier = {"root_attempt_id": "initial-root", "recovery_attempts": 0}
                self.assertEqual(run.release_action(barrier, record), "success")
                self.assertEqual(run.release_action(
                    {**barrier, "recovery_attempts": 1}, record), "fail")
                self.assertEqual(run.release_action(
                    barrier, {**record, "recovery_attempts": 1}), "fail")
                run.releases["first-request"] = "success"
                self.assertEqual(run.release_action(barrier, record), "fail")

    def test_only_the_designated_root_may_be_released_as_success(self):
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(
                Namespace(evidence_dir=Path(directory)), "restart-fifth",
                {"goal": 5, "outcome": "success", "restart": "fifth"},
            )
            record = {"recovery_attempts": 5,
                      "last_recovery_root_attempt_id": "root-5"}
            self.assertEqual(
                run.release_action({"root_attempt_id": "root-5"}, record), "success")
            # A retry from an earlier lineage that is still queued after the
            # restart must not consume the designated root's success.
            self.assertEqual(
                run.release_action({"root_attempt_id": "root-3"}, record), "fail")
            uncharged = {**record, "recovery_attempts": 4}
            self.assertEqual(
                run.release_action({"root_attempt_id": "root-5"}, uncharged), "fail")
            exhausted = recovery.RecoveryRun(
                Namespace(evidence_dir=Path(directory)), "restart-fifth-exhausted",
                {"goal": 5, "outcome": "exhausted", "restart": "fifth"},
            )
            self.assertEqual(
                exhausted.release_action({"root_attempt_id": "root-5"}, record), "fail")

    def test_fifth_restart_requires_the_designated_root_barrier(self):
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(
                Namespace(evidence_dir=Path(directory)), "restart-fifth",
                {"goal": 5, "outcome": "success", "restart": "fifth"},
            )
            record = {"recovery_attempts": 5,
                      "last_recovery_root_attempt_id": "root-5"}
            self.assertFalse(run.restart_due(record, [{"root_attempt_id": "root-3"}]))
            self.assertFalse(run.restart_due({**record, "recovery_attempts": 4},
                                            [{"root_attempt_id": "root-5"}]))
            self.assertTrue(run.restart_due(record, [{"root_attempt_id": "root-5"}]))

    def order_evidence(self, order):
        scanner_first = order == "scanner-first"
        after, record = self.owned_snapshot({"goal": int(scanner_first), "outcome": "success"})
        before = copy.deepcopy(after)
        current = before["task_result_settlements"][0]
        current.update(state="caller_admitted", disposition=None, settled_at=None)
        source_id = "scanner-source" if scanner_first else record["message_id"]
        if scanner_first:
            current["activation_id"] = "root-1"
            source = {
                "id": source_id, "kind": "internal_followup",
                "agent_id": record["agent_id"], "work_item_id": record["work_item_id"],
                "origin": {"kind": "system", "subsystem": "task_result_recovery"},
                "authority_class": "runtime_instruction", "admission_context": "runtime_owned",
                "delivery_surface": "runtime_system",
                "source_refs": {
                    "task_result_message_id": record["message_id"],
                    "task_result_identity": record["result_identity"],
                    "task_result_rejoin_generation": str(record["rejoin_generation"]),
                    "task_result_parent_turn_id": record["parent_turn_id"],
                },
            }
            for snapshot in (before, after):
                snapshot["wait_conditions"] = []
                snapshot["messages"].append({
                    "message_id": source_id, "payload_json": json.dumps(source),
                })
            row = next(row for row in before["execution_protocol_attempts"]
                       if row["attempt_id"] == "root-1")
            value = oracle.payload(row)
            value.update(source_message_id=source_id, source={"identity": {
                "kind": "internal_followup", "message_id": source_id,
            }},
                         binding={"kind": "work_item", "work_item_id": record["work_item_id"]})
            row["payload_json"] = json.dumps(value)
            before["queue_entries"] = [{"message_id": record["message_id"], "status": "queued"}]
            final = after["execution_protocol_attempts"][0]
            attempt = oracle.payload(final)
            attempt.update(
                attempt_id="root-1", source_message_id=source_id,
                source={"identity": {"kind": "internal_followup", "message_id": source_id}},
                recovery_of_attempt_id=None,
            )
            final.update(attempt_id="root-1", payload_json=json.dumps(attempt))
            after["execution_protocol_attempts"] = [final]
            record["activation_id"] = "root-1"
            turn = after["turn_records"][0]
            turn.update(trigger_message_id=source_id, payload_json=json.dumps({
                "idle_input_batch": {
                    "activation_id": "root-1", "items": [{"message_id": source_id}],
                },
            }))
            brief = after["briefs"][0]
            brief.update(message_id=source_id, payload_json=json.dumps({
                **oracle.payload(brief), "related_message_id": source_id,
            }))
            outcome = after["execution_protocol_outcomes"][0]
            outcome.update(attempt_id="root-1", payload_json=json.dumps({
                **oracle.payload(outcome), "attempt_id": "root-1",
            }))
        else:
            source = None
        next(row for row in before["execution_protocol_attempts"]
             if row["attempt_id"] == current["activation_id"])["lifecycle_state"] = "open"
        build = "holon 0.54.1 (1662676f) [recovery-acceptance-test]"
        event = {
            "protocol": 1, "request_id": "order", "build": build,
            **{key: record[key] for key in ("agent_id", "task_id", "work_item_id")},
            "fence": {
                "result_message_id": record["message_id"],
                **{key: record[key] for key in (
                    "result_identity", "parent_turn_id", "rejoin_generation", "recovery_epoch",
                )},
            },
            "recovery_attempts": 0, "last_recovery_root_attempt_id": None,
            "observed_at": "2026-10-25T13:00:00Z",
            "source_message_id": record["message_id"] if scanner_first else None,
            "queue_entry": {"message_id": record["message_id"], "status": "queued"},
        }
        held = {**copy.deepcopy(event),
                "event": "task-result-held" if scanner_first else "scanner-held"}
        scanner = {**copy.deepcopy(event), "event": "scanner-enqueued",
                   "source": source, "source_message_id": source_id,
                   "queue_entry": {"message_id": source_id, "status": "queued"}}
        released = {**copy.deepcopy(event),
                    "event": "task-result-released" if scanner_first else "scanner-released",
                    "observed_at": "2026-10-25T13:00:32Z"}
        return before, after, record, held, scanner if scanner_first else None, released, build

    def test_controlled_order_requires_canonical_claim_and_complete_private_fences(self):
        for order in ("exact-first", "scanner-first"):
            before, after, record, held, scanner, released, build = self.order_evidence(order)
            captured = {key: record[key] for key in oracle.IDENTITY_FIELDS}

            def check(left=before, right=after, gate=held, scan=scanner, release=released,
                      candidate=build):
                return oracle.require_first_admission(
                    left, after=right, captured=captured, order=order,
                    held=gate, scanner=scan, released=release,
                    request_id="order", expected_build=candidate,
                )

            check()
            for mutation in ("build", "gate", "fence", "winner", "charge", "release", "second-root"):
                left, right, gate = copy.deepcopy((before, after, held))
                release, candidate = released, build
                if mutation == "build":
                    candidate = "holon 0.54.1 (1662676f)"
                elif mutation == "gate":
                    gate["recovery_attempts"] = 1
                elif mutation == "fence":
                    gate["fence"]["rejoin_generation"] += 1
                elif mutation == "winner":
                    attempt = next(row for row in left["execution_protocol_attempts"]
                                   if row["attempt_id"] == left["task_result_settlements"][0]["activation_id"])
                    value = oracle.payload(attempt)
                    value["source_message_id"] = "foreign"
                    attempt["payload_json"] = json.dumps(value)
                elif mutation == "charge":
                    right["task_result_settlements"][0]["recovery_epoch"] += 1
                elif mutation == "release":
                    release = None
                else:
                    right["task_result_recovery_admissions"].append({
                        "result_identity": record["result_identity"], "recovery_epoch": 2,
                        "root_attempt_id": "extra-root", "admitted_at": "2026-10-25T13:20:00Z",
                    })
                with self.subTest(order=order, mutation=mutation), self.assertRaises(AssertionError):
                    check(left, right, gate, release=release, candidate=candidate)

    def test_controlled_order_preserves_nanosecond_ack_order(self):
        for order in ("exact-first", "scanner-first"):
            before, after, record, held, scanner, released, build = self.order_evidence(order)
            held["observed_at"] = "2026-10-26T04:26:03.711528800Z"
            released["observed_at"] = "2026-10-26T05:26:03.711528801+01:00"

            def check():
                return oracle.require_first_admission(
                    before, after=after, captured={key: record[key] for key in oracle.IDENTITY_FIELDS},
                    order=order, held=held, scanner=scanner, released=released,
                    request_id="order", expected_build=build,
                )

            with self.subTest(order=order):
                check()
                released["observed_at"] = "2026-10-26T04:26:03.711528799Z"
                with self.assertRaisesRegex(AssertionError, "before it was held"):
                    check()

    def test_order_controller_waits_for_claim_before_releasing_the_loser(self):
        for order in ("exact-first", "scanner-first"):
            before, _, record, held, scanner, released, build = self.order_evidence(order)
            with self.subTest(order=order), tempfile.TemporaryDirectory() as directory:
                run = recovery.RecoveryRun(
                    Namespace(evidence_dir=Path(directory)), order,
                    {"goal": int(order == "scanner-first"), "outcome": "success", "first_order": order},
                )
                run.captured = {key: record[key] for key in oracle.IDENTITY_FIELDS}
                run.setup_identity = {key: record[key] for key in (
                    "agent_id", "task_id", "work_item_id", "parent_turn_id", "rejoin_generation",
                )}
                run.acceptance_request = "order"
                (run.evidence / "build.json").write_text(json.dumps({"binary_version": build}))
                events = {event["event"]: event for event in (held, scanner) if event is not None}
                run.acceptance_event = Mock(side_effect=events.get)
                run.docker = Mock()
                run.acceptance_control(
                    run.setup_identity, prearm=True,
                    hold_scanner=order == "exact-first", hold_task_result=order == "scanner-first",
                )
                armed = json.loads(run.docker.call_args.kwargs["data"])
                self.assertIsNone(armed["fence"])
                self.assertEqual(armed["hold_task_result"], order == "scanner-first")
                self.assertEqual(armed["hold_scanner"], order == "exact-first")
                run.docker.reset_mock()
                self.assertFalse(run.observe_order(before, []))
                run.docker.assert_not_called()
                self.assertTrue(run.observe_order(before, [{"request_id": "barrier"}]))
                self.assertTrue(run.order_captured)
                self.assertFalse(run.order_verified)
                control = json.loads(run.docker.call_args.kwargs["data"])
                self.assertEqual(control["fence"], held["fence"])
                self.assertFalse(control["hold_task_result"])
                self.assertFalse(control["hold_scanner"])
                # The run loop publishes the loser release acknowledgement only
                # after this winner's held provider request can finish.
                self.assertTrue(run.observe_order(before, [{"request_id": "barrier"}]))
                self.assertFalse(run.observe_order(before, []))
                self.assertFalse(run.order_verified)
                events[released["event"]] = released
                self.assertTrue(run.observe_order(before, [{"request_id": "barrier"}]))
                self.assertTrue(run.order_verified)
                run.docker.assert_called_once()

    def test_scanner_first_rejects_false_queue_source_and_owner_proofs(self):
        before, after, record, held, scanner, released, build = self.order_evidence("scanner-first")
        for mutation in ("source-fence", "source-origin", "scanner-queue", "original-queue",
                         "closed", "owner", "early-release", "held-build", "held-source"):
            left, gate, scan, release = copy.deepcopy((before, held, scanner, released))
            if mutation.startswith("source-"):
                row = next(row for row in left["messages"]
                           if row["message_id"] == scan["source_message_id"])
                value = oracle.payload(row)
                if mutation == "source-fence":
                    value["source_refs"]["task_result_rejoin_generation"] = "999"
                else:
                    value["origin"]["subsystem"] = "ordinary_followup"
                row["payload_json"] = json.dumps(value)
                scan["source"] = value
            elif mutation == "scanner-queue":
                scan["queue_entry"]["status"] = "dropped"
            elif mutation == "original-queue":
                left["queue_entries"][0]["status"] = "processed"
            elif mutation in ("closed", "owner"):
                row = next(row for row in left["execution_protocol_attempts"]
                           if row["attempt_id"] == "root-1")
                if mutation == "closed":
                    row["lifecycle_state"] = "interrupted"
                else:
                    value = oracle.payload(row)
                    value["binding"]["work_item_id"] = "foreign"
                    row["payload_json"] = json.dumps(value)
            elif mutation == "early-release":
                release["observed_at"] = "2026-10-25T12:00:00Z"
            elif mutation == "held-build":
                gate["build"] = "another test build [recovery-acceptance-test]"
            else:
                gate["source_message_id"] = "foreign"
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                oracle.require_first_admission(
                    left, after=after, captured={key: record[key] for key in oracle.IDENTITY_FIELDS},
                    order="scanner-first", held=gate, scanner=scan, released=release,
                    request_id="order", expected_build=build,
                )

    def sixth_evidence(self):
        before, record = self.owned_snapshot({"goal": 5, "outcome": "exhausted"})
        after = copy.deepcopy(before)
        source_id = "msg_task_result_recovery_acceptance_sixth"
        source = {
            "id": source_id, "agent_id": "qa", "work_item_id": "owner",
            "kind": "internal_followup",
            "origin": {"kind": "system", "subsystem": "task_result_recovery"},
            "authority_class": "runtime_instruction", "admission_context": "runtime_owned",
            "delivery_surface": "runtime_system",
            "source_refs": {
                "task_result_message_id": record["message_id"],
                "task_result_identity": record["result_identity"],
                "task_result_rejoin_generation": str(record["rejoin_generation"]),
                "task_result_parent_turn_id": record["parent_turn_id"],
                "recovery_acceptance_request_id": "sixth",
            },
        }
        after["messages"].append({
            "message_id": source_id, "payload_json": json.dumps(source),
        })
        after["queue_entries"] = [{"message_id": source_id, "status": "processed"}]
        build = "holon 0.54.1 (1662676f) [recovery-acceptance-test]"
        event = {
            "protocol": 1, "request_id": "sixth", "build": build,
            **{key: record[key] for key in (
                "agent_id", "task_id", "work_item_id", "recovery_attempts",
                "last_recovery_root_attempt_id", "recovery_paused_at",
            )},
            "fence": {
                "result_message_id": record["message_id"],
                **{key: record[key] for key in (
                    "result_identity", "parent_turn_id", "rejoin_generation", "recovery_epoch",
                )},
            },
            "source_message_id": source_id, "source": source,
            "queue_entry": {"message_id": source_id, "status": "dequeued"},
        }
        enqueued = {**copy.deepcopy(event), "event": "sixth-enqueued"}
        rejected = {
            **copy.deepcopy(event), "event": "budget-rejected",
            "rejection_stage": "scheduler_canonical_claim",
            "reason": "recovery_budget_exhausted",
        }
        return before, after, record, enqueued, rejected, build

    def test_sixth_rejection_requires_actual_correctly_fenced_queued_source(self):
        evidence = self.sixth_evidence()

        def check(values):
            before, after, captured, enqueued, rejected, build = values
            oracle.require_sixth_rejected(
                before, after, captured=captured, enqueued=enqueued, rejected=rejected,
                request_id="sixth", expected_build=build,
            )

        check(evidence)
        for mutation in (
            "no-marker", "wrong-build", "foreign-fence", "wrong-epoch", "stale-reason",
            "no-enqueue", "fake-source", "queue-pending", "source-ref", "no-ack",
            "new-attempt", "sixth-charge", "changed-pause",
        ):
            with self.subTest(mutation=mutation):
                values = copy.deepcopy(evidence)
                before, after, captured, enqueued, rejected, build = values
                if mutation == "no-marker":
                    values = (*values[:-1], "holon 0.54.1 (1662676f)")
                elif mutation == "wrong-build":
                    rejected["build"] = build.replace("1662676f", "deadbeef")
                elif mutation == "foreign-fence":
                    rejected["fence"]["result_identity"] = "foreign"
                elif mutation == "wrong-epoch":
                    rejected["fence"]["recovery_epoch"] += 1
                elif mutation == "stale-reason":
                    rejected["reason"] = "invalid_or_stale"
                elif mutation == "no-enqueue":
                    after["messages"].pop()
                elif mutation == "fake-source":
                    rejected["source"]["authority_class"] = "operator"
                elif mutation == "queue-pending":
                    after["queue_entries"][0]["status"] = "queued"
                elif mutation == "source-ref":
                    source = oracle.payload(after["messages"][-1])
                    source["source_refs"]["task_result_rejoin_generation"] = "999"
                    after["messages"][-1]["payload_json"] = json.dumps(source)
                elif mutation == "no-ack":
                    enqueued.clear()
                elif mutation == "new-attempt":
                    after["execution_protocol_attempts"].append({
                        "agent_id": "qa", "attempt_id": "sixth-attempt",
                        "payload_json": json.dumps({
                            "attempt_id": "sixth-attempt", "recovery_of_attempt_id": None,
                            "source_message_id": enqueued["source_message_id"],
                        }),
                    })
                elif mutation == "sixth-charge":
                    after["task_result_settlements"][0]["recovery_attempts"] = 6
                else:
                    after["task_result_settlements"][0]["recovery_paused_at"] = "changed"
                with self.assertRaises(AssertionError):
                    check(values)

    def test_sixth_checkpoint_cannot_be_replaced_by_five_roots_and_silence(self):
        before, after, record, enqueued, rejected, build = self.sixth_evidence()
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(
                Namespace(evidence_dir=Path(directory)), "sixth-rejected",
                {"goal": 5, "outcome": "exhausted", "sixth": True},
            )
            run.captured = {key: record[key] for key in oracle.IDENTITY_FIELDS}
            run.acceptance_request = "sixth"
            run.joins = [{"root_attempt_id": f"root-{index}", "action": "fail",
                          "activation_id": f"root-{index}",
                          "source_message_ids": [f"message-root-{index}"],
                          "recovery_attempts": index} for index in range(1, 6)]
            (run.evidence / "database.json").write_text(json.dumps({
                "rows": after, "truncated_tables": [],
            }))
            with self.assertRaisesRegex(AssertionError, "checkpoint was skipped"):
                run.verify_frozen()
            run.sixth_requested = True
            for name, value in (
                ("sixth-before", before), ("acceptance-sixth-enqueued", enqueued),
                ("acceptance-budget-rejected", rejected),
                ("build", {"binary_version": build}),
            ):
                (run.evidence / f"{name}.json").write_text(json.dumps(value))
            self.assertTrue(run.verify_frozen()["sixth_source_rejected"])
            run.docker = Mock(return_value=Namespace(stdout="null"))
            self.assertFalse(run.observe_sixth(after))
            self.assertTrue(run.sixth_requested)
            self.assertEqual(run.docker.call_count, 2)

    def coexisting_snapshot(self):
        before, exhausted = self.owned_snapshot({"goal": 5, "outcome": "exhausted"})
        fresh, _ = self.owned_snapshot({"goal": 0, "outcome": "success"})
        encoded = json.dumps(fresh)
        for identifier in (
            "result-identity-1", "parent-turn", "reentry-turn", "attempt-1",
            "outcome-1", "task-1", "result-1", "wait-1", "brief-1",
        ):
            encoded = encoded.replace(identifier, f"fresh-{identifier}")
        fresh = json.loads(encoded)
        after = copy.deepcopy(before)
        for table, rows in fresh.items():
            if table != "work_items":
                after.setdefault(table, []).extend(rows)
        return before, after, exhausted, fresh["task_result_settlements"][0]

    def test_same_owner_fairness_requires_real_independent_consumption_without_revoking_a(self):
        before, after, exhausted, fresh = self.coexisting_snapshot()

        def check(snapshot):
            oracle.require_coexisting_fairness(
                before, snapshot, captured=exhausted, fresh=fresh,
            )

        check(after)
        for mutation in ("owner", "complete-owner", "epoch", "roots", "consume-a",
                         "consume-b", "brief", "same-task", "rearm-a"):
            with self.subTest(mutation=mutation):
                broken = copy.deepcopy(after)
                a, b = broken["task_result_settlements"]
                if mutation == "owner":
                    b["work_item_id"] = "foreign"
                elif mutation == "complete-owner":
                    broken["work_items"][0]["state"] = "completed"
                elif mutation == "epoch":
                    a["recovery_epoch"] += 1
                elif mutation == "roots":
                    broken["task_result_recovery_admissions"].pop()
                elif mutation == "consume-a":
                    a.update(state="settled", disposition="model_delivered")
                elif mutation == "consume-b":
                    b["settled_at"] = None
                elif mutation == "brief":
                    broken["briefs"][-1]["message_id"] = exhausted["message_id"]
                elif mutation == "same-task":
                    b["task_id"] = exhausted["task_id"]
                else:
                    a["next_recheck_at"] = "2026-10-25T13:17:00Z"
                with self.assertRaises(AssertionError):
                    check(broken)

    def test_fairness_observer_releases_only_the_new_owned_exact_wait_then_freezes_identity(self):
        before, after, exhausted, fresh = self.coexisting_snapshot()
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(
                Namespace(evidence_dir=Path(directory)), "coexisting-owner",
                {"goal": 5, "outcome": "exhausted", "fairness": True},
            )
            run.captured = {key: exhausted[key] for key in oracle.IDENTITY_FIELDS}
            run.docker = Mock()
            (run.evidence / "fairness-before.json").write_text(json.dumps(before))
            pending = copy.deepcopy(after)
            pending["task_result_settlements"].pop()
            task = oracle.payload(pending["tasks"][-1])
            task["status"] = "running"
            pending["tasks"][-1]["payload_json"] = json.dumps(task)
            pending["wait_conditions"][-1].update(status="active", kind="task")
            broken = copy.deepcopy(pending)
            broken["wait_conditions"][-1]["work_item_id"] = "foreign"
            with self.assertRaisesRegex(AssertionError, "shared owner"):
                run.observe_fairness(broken)
            run.docker.assert_not_called()
            self.assertFalse(run.observe_fairness(pending))
            run.docker.assert_called_once_with(
                "exec", run.daemon, "bash", "-c", f"touch {recovery.FAIRNESS_GATE}",
            )
            self.assertTrue(run.observe_fairness(after))
            captured = json.loads((run.evidence / "fairness-captured.json").read_text())
            self.assertEqual(captured, {
                key: fresh[key] for key in oracle.IDENTITY_FIELDS
            })

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
            "completed_at": "2026-10-25T13:16:00Z" if spec["outcome"] == "success" else None,
            "payload_json": json.dumps(
                {"result_brief_id": "brief-1"} if spec["outcome"] == "success" else {}),
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
            "payload_json": json.dumps({
                **original, "attempt_id": "task-root", "turn_id": "original-turn",
            }),
        })
        row["payload_json"] = json.dumps({
            **original, "recovery_of_attempt_id": "task-root",
            "source_message_id": "fallback-source",
            "source": {"identity": {"kind": "runtime_recovery"}, "generation": 2},
        })
        original_turn = copy.deepcopy(snapshot["turn_records"][0])
        original_turn.update(turn_id="original-turn", terminal_kind="interrupted")
        snapshot["turn_records"][0]["trigger_message_id"] = "fallback-source"
        snapshot["turn_records"].append(original_turn)
        snapshot["briefs"][0].update(message_id="fallback-source", payload_json=json.dumps({
            **oracle.payload(snapshot["briefs"][0]), "related_message_id": "fallback-source",
        }))
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(Namespace(evidence_dir=Path(directory)),
                                       "fallback-success", spec)
            run.captured = {key: record[key] for key in oracle.IDENTITY_FIELDS}
            run.joins = [{
                "side": "b", "action": "success", "recovery_attempts": 0,
                "source_message_ids": ["fallback-source"],
                "activation_id": row["attempt_id"], "root_attempt_id": "task-root",
                "budget_before_release": 0,
            }]
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

    def test_frozen_provider_join_preserves_recorded_attempt_after_restart(self):
        spec = {"goal": 5, "outcome": "success"}
        with tempfile.TemporaryDirectory() as directory:
            run = recovery.RecoveryRun(Namespace(evidence_dir=Path(directory)), "fifth-success", spec)
            snapshot, record = self.owned_snapshot(spec)
            for row in snapshot["execution_protocol_attempts"]:
                if row["attempt_id"].startswith("root-"):
                    value = oracle.payload(row)
                    value.update(
                        source_message_id=f"message-{row['attempt_id']}",
                        binding={"kind": "work_item", "work_item_id": "owner"},
                    )
                    row["payload_json"] = json.dumps(value)
            run.captured = {key: record[key] for key in oracle.IDENTITY_FIELDS}
            run.joins = [{
                "activation_id": f"root-{index}", "root_attempt_id": f"root-{index}",
                "source_message_ids": [f"message-root-{index}"],
                "recovery_attempts": index, "side": "a",
                "action": "success" if index == 5 else "fail",
                "budget_before_release": 5 if index == 5 else index,
            } for index in range(1, 6)]
            original = next(row for row in snapshot["execution_protocol_attempts"]
                            if row["attempt_id"] == "root-5")
            value = oracle.payload(original)
            original["payload_json"] = json.dumps({
                **value, "terminal_at": "2026-10-25T13:15:00Z",
            })
            snapshot["execution_protocol_attempts"].append({
                "agent_id": "qa", "attempt_id": "replay-5",
                "payload_json": json.dumps({
                    **value, "recovery_of_attempt_id": "root-5",
                    "terminal_at": "2026-10-25T13:16:00Z",
                }),
            })
            (run.evidence / "database.json").write_text(json.dumps({
                "rows": snapshot, "truncated_tables": [],
            }))
            run.verify_frozen()
            run.joins[-1]["source_message_ids"] = ["message-root-3"]
            with self.assertRaisesRegex(AssertionError, "provider barrier"):
                run.verify_frozen()

    def test_each_manifest_case_runs_its_frozen_oracle(self):
        manifest = json.loads(recovery.MANIFEST.read_text())
        for case, spec in manifest["cases"].items():
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                run = recovery.RecoveryRun(Namespace(evidence_dir=Path(directory)), case, spec)
                snapshot, record = self.owned_snapshot(spec)
                if spec.get("first_order"):
                    before, snapshot, record, held, scanner, released, build = self.order_evidence(
                        spec["first_order"])
                    run.acceptance_request = "order"
                    run.order_verified = True
                    values = [("order-before", before), ("build", {"binary_version": build})]
                    values += [(f"acceptance-{event['event']}", event)
                               for event in (held, scanner, released) if event is not None]
                    for name, value in values:
                        (run.evidence / f"{name}.json").write_text(json.dumps(value))
                if spec.get("sixth"):
                    before, snapshot, record, enqueued, rejected, build = self.sixth_evidence()
                    run.acceptance_request = "sixth"
                    run.sixth_requested = True
                    for name, value in (
                        ("sixth-before", before), ("acceptance-sixth-enqueued", enqueued),
                        ("acceptance-budget-rejected", rejected), ("build", {"binary_version": build}),
                    ):
                        (run.evidence / f"{name}.json").write_text(json.dumps(value))
                if spec.get("fairness"):
                    before, snapshot, record, fresh = self.coexisting_snapshot()
                    for name, value in (
                        ("fairness-before", before), ("fairness-captured", fresh),
                        ("fairness-pre-terminal", {
                            key: fresh[key] for key in (
                                "agent_id", "task_id", "work_item_id",
                                "parent_turn_id", "rejoin_generation",
                            )
                        }),
                    ):
                        (run.evidence / f"{name}.json").write_text(json.dumps(value))
                run.captured = {key: record[key] for key in oracle.IDENTITY_FIELDS}
                run.restarted = bool(spec.get("restart"))
                run.joins = [{
                    "root_attempt_id": row["root_attempt_id"], "recovery_attempts": index + 1,
                    "activation_id": row["root_attempt_id"],
                    "source_message_ids": [oracle.payload(next(
                        attempt for attempt in snapshot["execution_protocol_attempts"]
                        if attempt["attempt_id"] == row["root_attempt_id"]
                    ))["source_message_id"]],
                    "side": "b" if spec.get("fallback") else "a",
                    "action": "success" if spec["outcome"] == "success"
                    and index + 1 == spec["goal"] else "fail",
                    "budget_before_release": spec["goal"] if spec["outcome"] == "success"
                    and index + 1 == spec["goal"] else index + 1,
                } for index, row in enumerate(snapshot["task_result_recovery_admissions"])]
                if not spec["goal"]:
                    run.joins.append({
                        "root_attempt_id": "attempt-1", "recovery_attempts": 0,
                        "activation_id": "attempt-1",
                        "source_message_ids": [record["message_id"]],
                        "action": "success", "side": "b" if spec.get("fallback") else "a",
                        "budget_before_release": 0,
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
                if spec.get("first_order"):
                    self.assertTrue(result["scanner_order_controlled"])
                    run.order_verified = False
                    with self.assertRaisesRegex(AssertionError, "checkpoint was skipped"):
                        run.verify_frozen()
                if spec.get("restart") and spec["outcome"] == "exhausted":
                    for mutation in ("consumed", "sixth", "success", "fallback"):
                        broken = copy.deepcopy(snapshot)
                        joins = run.joins
                        if mutation == "consumed":
                            broken["task_result_settlements"][0].update(
                                state="settled", disposition="model_delivered",
                            )
                        elif mutation == "sixth":
                            broken["task_result_recovery_admissions"].append({
                                **broken["task_result_recovery_admissions"][-1],
                                "root_attempt_id": "sixth-root",
                            })
                        elif mutation == "success":
                            run.joins = [{**row, "action": "success"} for row in joins]
                        else:
                            run.joins = [{**row, "side": "a"} for row in joins]
                        (run.evidence / "database.json").write_text(json.dumps({
                            "rows": broken, "truncated_tables": [],
                        }))
                        with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                            run.verify_frozen()
                        run.joins = joins
                    (run.evidence / "database.json").write_text(json.dumps({
                        "rows": snapshot, "truncated_tables": [],
                    }))
                if spec.get("restart"):
                    run.restarted = False
                    with self.assertRaisesRegex(AssertionError, "checkpoint"):
                        run.verify_frozen()
                if spec["goal"]:
                    run.joins = []
                    with self.assertRaisesRegex(AssertionError, "provider release"):
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
        self.assertEqual(len(manifest["suites"]["slow"]), 6)
        self.assertTrue(manifest["pending_coverage"])
        manifest["backoff_seconds"][0] = 1
        with self.assertRaises(AssertionError):
            recovery.validate_manifest(manifest)

    def test_restart_exhaustion_is_a_required_fallback_case_not_two_separate_drills(self):
        manifest = json.loads(recovery.MANIFEST.read_text())
        case = "restart-fifth-exhausted"
        self.assertIn(case, manifest["suites"]["slow"])
        self.assertEqual(manifest["cases"][case], {
            "goal": 5, "outcome": "exhausted", "restart": "fifth",
            "fallback": True, "timeout_seconds": 3600,
        })

    def test_release_slow_gate_cannot_be_softened_or_hidden_behind_120_seconds(self):
        workflow = (ROOT / ".github/workflows/release-e2e.yml").read_text()
        gate = workflow.split("  task-result-recovery-required:\n", 1)[1].split("  core:\n", 1)[0]
        self.assertNotIn("continue-on-error", gate)
        self.assertIn("timeout-minutes: 70", gate)
        self.assertIn("--timeout 3600", gate)
        for name in json.loads(recovery.MANIFEST.read_text())["suites"]["slow"]:
            self.assertIn(name, gate)
        self.assertIn("needs: [candidate, task-result-recovery-required, task-result-controlled-required]",
                      workflow)
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("--suite fast --timeout 180", ci)
        self.assertIn("target/docker-e2e/task-result-fast", ci)

    def test_controlled_suite_is_a_separate_compile_time_hard_gate_in_ci_and_release(self):
        manifest = recovery.validate_manifest(json.loads(recovery.MANIFEST.read_text()))
        self.assertEqual(manifest["suites"]["controlled"], [
            "controlled-exact-first", "controlled-scanner-first", "sixth-rejected",
        ])
        for path in ("ci.yml", "release-e2e.yml"):
            workflow = (ROOT / ".github/workflows" / path).read_text()
            # Inspect a complete job rather than accidentally accepting a soft sibling step.
            gate = re.split(r"\n  [a-z][a-z0-9-]*:\n",
                            workflow.split("  task-result-controlled-required:\n", 1)[1], 1)[0]
            self.assertNotIn("continue-on-error", gate)
            self.assertIn("--recovery-acceptance", gate)
            self.assertIn("timeout-minutes: 100", gate)
            self.assertIn("--suite controlled --timeout 3600", gate)
            self.assertIn("holon:recovery-acceptance", gate)

    def test_cli_rejects_insufficient_slow_budget_before_docker(self):
        with patch.object(recovery, "command") as command:
            for timeout in ("120", "1800"):
                with self.subTest(timeout=timeout), self.assertRaises(SystemExit) as error:
                    recovery.main([
                        "--image", "candidate", "--python-image", "fixture",
                        "--expected-revision", "12345678", "--evidence-dir", "unused",
                        "--suite", "slow", "--timeout", timeout,
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
