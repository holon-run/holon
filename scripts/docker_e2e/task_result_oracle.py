"""Fail-closed task-result acceptance shared by deterministic and live runners."""
from __future__ import annotations

import json
from datetime import datetime
from typing import Any


IDENTITY_FIELDS = (
    "result_identity", "agent_id", "task_id", "message_id",
    "work_item_id", "rejoin_generation", "parent_turn_id",
)


def require(condition: Any, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def payload(row: dict[str, Any]) -> dict[str, Any]:
    return json.loads(row["payload_json"])


def require_terminal_task_receipt(
    detail: dict[str, Any], *, task_id: str, result_message_id: str,
    activation_id: str,
) -> dict[str, Any]:
    """Accept only the canonical tool envelope, never text or recursive matches."""
    output = detail.get("output")
    require(isinstance(output, dict), "missing structured tool output")
    envelope = output.get("envelope")
    require(
        isinstance(envelope, dict) and detail.get("status") == "success"
        and envelope.get("status") == "success"
        and envelope.get("tool_name") == detail.get("tool_name")
        and not output.get("is_error") and not envelope.get("error"),
        "terminal task receipt is not a successful canonical envelope",
    )
    result, args = envelope.get("result"), detail.get("input", {})
    require(isinstance(result, dict), "missing structured tool result")
    if detail["tool_name"] == "TaskStatus":
        task = result.get("task")
        require(
            args.get("task_id") == task_id and isinstance(task, dict)
            and task.get("task_id") == task_id
            and task.get("status") in {"completed", "failed", "cancelled", "interrupted"},
            "TaskStatus did not attest the exact terminal task",
        )
    else:
        require(
            detail["tool_name"] == "WaitFor"
            and args.get("wake") == "task_result" and args.get("resource") == task_id
            and result.get("continuation") == "continue_turn"
            and result.get("task_id") == task_id
            and result.get("result_message_id") == result_message_id
            and (
                result.get("disposition") == "task_result_settled"
                and result.get("settlement_disposition") == "model_delivered"
                or result.get("disposition") == "task_result_already_admitted"
                and result.get("activation_id") == activation_id
            ),
            "WaitFor did not attest this activation's exact terminal task result",
        )
    return result


def task_identity(
    snapshot: dict[str, Any], *, agent_id: str, task_id: str,
    parent_turn_id: str, captured: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    records = [
        row for row in snapshot["task_result_settlements"]
        if row["task_id"] == task_id and row["agent_id"] == agent_id
    ]
    require(
        len(records) == 1 and bool(records[0]["result_identity"])
        and records[0]["parent_turn_id"] == parent_turn_id,
        f"task result identity must occur exactly once under its parent: {records}",
    )
    record = records[0]
    if captured is not None:
        require(
            all(record.get(key) == captured.get(key) for key in IDENTITY_FIELDS),
            f"captured task-result identity/owner/generation drifted: {record}",
        )
    tasks = [payload(row) for row in snapshot["tasks"] if row["task_id"] == task_id]
    require(len(tasks) == 1, f"missing or duplicate real task: {tasks}")
    task = tasks[0]
    detail = task.get("detail", {})
    require(
        task["id"] == task_id
        and task.get("status") in {"completed", "failed", "cancelled", "interrupted"}
        and detail.get("rejoin_obligation_id") == task_id
        and detail.get("rejoin_generation") == record["rejoin_generation"]
        and detail.get("parent_turn_id") == parent_turn_id
        and task.get("work_item_id") == record["work_item_id"],
        f"task/result owner or generation migrated: {task}",
    )
    messages = [
        payload(row) for row in snapshot["messages"]
        if row["message_id"] == record["message_id"]
    ]
    require(
        len(messages) == 1 and messages[0]["kind"] == "task_result"
        and messages[0]["origin"] == {"kind": "task", "task_id": task_id}
        and messages[0]["metadata"]["task_id"] == task_id,
        f"task result identity/provenance mismatch: {messages}",
    )
    metadata = messages[0]["metadata"]
    require(all(
        key not in metadata or metadata[key] == record[key]
        for key in ("result_identity", "rejoin_generation", "parent_turn_id")
    ), f"task-result message metadata disagrees with captured identity: {metadata}")
    return record, task


def require_result_brief(
    snapshot: dict[str, Any], *, agent_id: str, turn_id: str,
    message_id: str, work_item_id: str | None, brief_id: str | None = None,
) -> dict[str, Any]:
    rows = [
        row for row in snapshot["briefs"]
        if (row["evidence_id"] == brief_id if brief_id else
            row["agent_id"] == agent_id and row["turn_id"] == turn_id
            and row["kind"] == "result")
    ]
    require(len(rows) == 1, f"expected exactly one terminal Result brief: {rows}")
    row = rows[0]
    brief = payload(row)
    require(
        row["kind"] == brief.get("kind") == "result"
        and row["agent_id"] == brief.get("agent_id") == agent_id
        and row["turn_id"] == brief.get("turn_id") == turn_id
        and row["message_id"] == brief.get("related_message_id") == message_id
        and row["work_item_id"] == brief.get("work_item_id") == work_item_id,
        f"terminal Result brief kind/agent/owner/turn/message mismatch: {row}",
    )
    if work_item_id is not None:
        items = [
            item for item in snapshot["work_items"]
            if item["work_item_id"] == work_item_id and item["agent_id"] == agent_id
        ]
        require(
            len(items) == 1 and items[0]["state"] == "completed"
            and bool(items[0].get("completed_at"))
            and payload(items[0]).get("result_brief_id") == row["evidence_id"],
            f"completed WorkItem did not bind this exact Result brief: {items}",
        )
    return brief


def require_live_task_owner(
    snapshot: dict[str, Any], *, captured: dict[str, Any],
    activation_id: str | None = None, observed_at: datetime | None = None,
) -> str:
    record, _ = task_identity(
        snapshot, agent_id=captured["agent_id"], task_id=captured["task_id"],
        parent_turn_id=captured["parent_turn_id"], captured=captured,
    )
    require(
        record["state"] == "caller_admitted" and not record.get("settled_at")
        and not record.get("disposition") and bool(record.get("activation_id"))
        and (activation_id is None or activation_id == record["activation_id"]),
        f"task-result owner is no longer admitted and alive: {record}",
    )
    attempts = [
        row for row in snapshot["execution_protocol_attempts"]
        if row["agent_id"] == captured["agent_id"]
        and row["attempt_id"] == record["activation_id"]
    ]
    require(len(attempts) == 1 and attempts[0]["lifecycle_state"] == "open",
            "task-result owner has no live execution attempt")
    attempt = payload(attempts[0])
    require(
        attempt.get("binding") == {
            "kind": "work_item", "work_item_id": captured["work_item_id"],
        },
        "live attempt is not bound to the captured task-result owner",
    )
    turns = [
        row for row in snapshot["turn_records"]
        if row["agent_id"] == captured["agent_id"]
        and row["turn_id"] == attempt.get("turn_id")
    ]
    require(
        len(turns) == 1 and turns[0].get("terminal_kind") is None
        and turns[0].get("current_work_item_id") == captured["work_item_id"],
        "task-result owner turn is terminal or belongs to another WorkItem",
    )
    recovery_ledger(snapshot, record, expected_count=0)
    require(
        not any(payload(row).get("source_refs", {}).get("task_result_identity")
                == captured["result_identity"] for row in snapshot["messages"]),
        "a recovery wake competed with the live exact task-result owner",
    )
    if observed_at is not None:
        require(bool(record.get("next_recheck_at")), "live owner lost its recheck deadline")
        due = datetime.fromisoformat(record["next_recheck_at"].replace("Z", "+00:00"))
        require(observed_at >= due, "live owner was not observed beyond its recheck deadline")
    return record["activation_id"]


def require_task_reentry(
    snapshot: dict[str, Any], *, agent_id: str, task_id: str,
    parent_turn_id: str, captured: dict[str, Any] | None = None,
    require_completion: bool = True,
) -> dict[str, Any]:
    record, task = task_identity(
        snapshot, agent_id=agent_id, task_id=task_id,
        parent_turn_id=parent_turn_id, captured=captured,
    )
    require(
        record["state"] == "settled" and record["disposition"] == "model_delivered"
        and bool(record.get("settled_at")) and bool(record.get("activation_id")),
        f"task result was not durably settled/model_delivered: {record}",
    )
    waits = [
        row for row in snapshot["wait_conditions"]
        if row["agent_id"] == agent_id and row["subject_ref"] == task_id
    ]
    require(
        bool(waits)
        and any(row["last_turn_id"] == parent_turn_id for row in waits)
        and all(
            row["status"] == "resolved"
            and row["trigger_message_id"] == record["message_id"]
            and row["work_item_id"] == record["work_item_id"]
            for row in waits
        ),
        f"exact task waits did not resolve under the captured owner: {waits}",
    )
    attempts = [
        row for row in snapshot["execution_protocol_attempts"]
        if row["agent_id"] == agent_id and row["attempt_id"] == record["activation_id"]
    ]
    require(len(attempts) == 1, f"settlement has no unique admitted activation: {attempts}")
    activation = attempts[0]
    attempt = payload(activation)
    turns = [
        row for row in snapshot["turn_records"]
        if row["agent_id"] == agent_id and row["turn_id"] == attempt.get("turn_id")
    ]
    require(
        activation["lifecycle_state"] == "settled"
        and attempt.get("attempt_id") == activation["attempt_id"]
        and attempt.get("agent_id") == agent_id
        and len(turns) == 1 and turns[0]["turn_id"] != parent_turn_id
        and turns[0]["terminal_kind"] == "completed"
        and turns[0].get("current_work_item_id") == record["work_item_id"],
        f"settlement activation did not execute a distinct completed owner turn: {attempt}",
    )
    turn = turns[0]
    binding = attempt.get("binding", {})
    require(
        (binding.get("kind") == "work_item"
         and binding.get("work_item_id") == record["work_item_id"])
        if record["work_item_id"] is not None else
        binding.get("kind") in {"agent_lifecycle", "conversation"},
        f"settlement activation owner migrated: {binding}",
    )
    outcomes = [
        row for row in snapshot["execution_protocol_outcomes"]
        if row["agent_id"] == agent_id
        and row["outcome_id"] == activation["terminal_outcome_id"]
    ]
    require(
        len(outcomes) == 1
        and outcomes[0]["attempt_id"] == payload(outcomes[0]).get("attempt_id")
        == activation["attempt_id"],
        f"activation terminal outcome is missing or foreign: {outcomes}",
    )
    outcome = payload(outcomes[0])
    terminal = outcome.get("outcome", {})
    require(
        outcome.get("outcome_id") == outcomes[0]["outcome_id"]
        and terminal.get("owner")
        == ("work_item" if record["work_item_id"] is not None else "conversation")
        and terminal.get("outcome", {}).get("kind") in {
            "replied", "complete", "continue", "wait",
            "handoff_to_work_item_continue", "handoff_to_work_item_wait",
        },
        f"activation did not settle a successful typed owner outcome: {outcome}",
    )
    source_message_id = attempt.get("source_message_id")
    require(
        source_message_id == turn["trigger_message_id"],
        f"activation/turn trigger mismatch: {attempt}",
    )
    causal = attempt
    model_input_turns = [(attempt["attempt_id"], turn)]
    seen = {attempt["attempt_id"]}
    while causal.get("source", {}).get("identity", {}).get("kind") == "runtime_recovery":
        predecessor = causal.get("recovery_of_attempt_id")
        require(predecessor and predecessor not in seen, "missing/cyclic replay source")
        seen.add(predecessor)
        parents = [
            payload(row) for row in snapshot["execution_protocol_attempts"]
            if row["agent_id"] == agent_id and row["attempt_id"] == predecessor
        ]
        require(len(parents) == 1 and parents[0].get("binding") == binding
                and parents[0].get("attempt_id") == predecessor
                and parents[0].get("agent_id") == agent_id,
                "replay source changed execution owner")
        causal = parents[0]
        source_turns = [
            row for row in snapshot["turn_records"]
            if row["agent_id"] == agent_id and row["turn_id"] == causal.get("turn_id")
        ]
        require(
            len(source_turns) == 1
            and source_turns[0].get("current_work_item_id") == record["work_item_id"]
            and source_turns[0]["trigger_message_id"] == causal.get("source_message_id"),
            "replay source lost its original owner turn",
        )
        model_input_turns.append((predecessor, source_turns[0]))
    source = causal.get("source", {}).get("identity", {})
    causal_message_id = causal.get("source_message_id")
    direct = source == {
        "kind": "task_result", "task_id": task_id,
        "result_message_id": record["message_id"],
    } and causal_message_id == record["message_id"]
    exact = (
        source.get("kind") == "triggered_wait"
        and source.get("trigger_message_id") == record["message_id"]
        and any(row.get("wait_condition_id") == source.get("wait_id") for row in waits)
    )
    # Scanner wakes are not TaskResults: verify their durable source refs, not text.
    recovery = [
        payload(row) for row in snapshot["messages"]
        if row["message_id"] == causal_message_id
        and row["message_id"] != record["message_id"]
    ]
    refs = recovery[0].get("source_refs", {}) if len(recovery) == 1 else {}
    # Recovery may retain the original model input on an interrupted ancestor,
    # not the final turn. Only the validated execution chain can supply it.
    batches = []
    for activation_id, input_turn in model_input_turns:
        stored_turn = payload(input_turn) if "payload_json" in input_turn else {}
        inputs = list(stored_turn.get("live_input_batches", []))
        if stored_turn.get("idle_input_batch"):
            inputs.append(stored_turn["idle_input_batch"])
        batches.extend((activation_id, batch) for batch in inputs)
    # The scanner wake is the model input for this path. The raw task result
    # stays a promise source and is never admitted as model input, so the
    # durable join is the wake message that references it.
    scanner_wake_in_model_input = any(
        batch.get("activation_id") == activation_id
        and causal_message_id in {item.get("message_id") for item in batch["items"]}
        for activation_id, batch in batches
    ) if len(recovery) == 1 else False
    scanner = (
        source.get("kind") in {"internal_followup", "triggered_wait"}
        and len(recovery) == 1 and scanner_wake_in_model_input
        and recovery[0].get("kind") == "internal_followup"
        and recovery[0].get("authority_class") == "runtime_instruction"
        and recovery[0].get("origin")
        == {"kind": "system", "subsystem": "task_result_recovery"}
        and recovery[0].get("work_item_id") == record["work_item_id"]
        and refs.get("task_result_identity") == record["result_identity"]
        and refs.get("task_result_message_id") == record["message_id"]
        and refs.get("task_result_parent_turn_id") == parent_turn_id
        and refs.get("task_result_rejoin_generation") == str(record["rejoin_generation"])
    )
    require(direct or exact or scanner, f"activation lost task-result source causality: {attempt}")
    # A scanner wake is the only model-delivered carrier of this promise, so a
    # completed turn that consumed the wake consumed the same task result.
    carriers = {
        row["message_id"] for row in snapshot["messages"]
        if payload(row).get("source_refs", {}).get("task_result_identity")
        == record["result_identity"]
    }
    carriers.add(record["message_id"])
    successful = []
    for row in snapshot["turn_records"]:
        if row["agent_id"] != agent_id or row["terminal_kind"] != "completed":
            continue
        stored = payload(row) if "payload_json" in row else {}
        inputs = list(stored.get("input_message_ids", []))
        if stored.get("idle_input_batch"):
            inputs += [
                item["message_id"] for item in stored["idle_input_batch"]["items"]
            ]
        inputs += [
            item["message_id"]
            for batch in stored.get("live_input_batches", []) for item in batch["items"]
        ]
        if (row["turn_id"] == turn["turn_id"]
                or row["trigger_message_id"] in carriers
                or carriers & set(inputs)):
            successful.append(row)
    require(
        len(successful) == 1,
        f"task result caused duplicate successful consumption: {successful}",
    )
    if require_completion:
        require_result_brief(
            snapshot, agent_id=agent_id, turn_id=turn["turn_id"],
            message_id=turn["trigger_message_id"], work_item_id=record["work_item_id"],
        )
    return task


def recovery_root(snapshot: dict[str, Any], agent_id: str, attempt_id: str) -> str:
    attempts = {
        row["attempt_id"]: payload(row)
        for row in snapshot["execution_protocol_attempts"] if row["agent_id"] == agent_id
    }
    seen = set()
    while True:
        require(attempt_id not in seen and attempt_id in attempts,
                f"missing/cyclic recovery attempt ancestry: {attempt_id}")
        seen.add(attempt_id)
        parent = attempts[attempt_id].get("recovery_of_attempt_id")
        if parent is None:
            return attempt_id
        attempt_id = parent


def recovery_ledger(
    snapshot: dict[str, Any], record: dict[str, Any], *,
    expected_count: int | None = None, real_backoff: bool = False,
) -> list[dict[str, Any]]:
    rows = [
        row for row in snapshot["task_result_recovery_admissions"]
        if row["result_identity"] == record["result_identity"]
        and row["recovery_epoch"] == record["recovery_epoch"]
    ]
    roots = {row["root_attempt_id"] for row in rows}
    require(
        len(rows) == len(roots) == record["recovery_attempts"]
        and 0 <= len(roots) <= 5
        and (expected_count is None or len(roots) == expected_count),
        f"recovery budget must charge distinct roots, not requests/fallbacks: {rows}",
    )
    for row in rows:
        require(
            recovery_root(snapshot, record["agent_id"], row["root_attempt_id"])
            == row["root_attempt_id"],
            f"fallback/replay was charged as a new recovery root: {row}",
        )
    rows.sort(key=lambda row: (row["admitted_at"], row["root_attempt_id"]))
    if rows:
        require(
            record["last_recovery_root_attempt_id"] == rows[-1]["root_attempt_id"],
            f"last recovery root disagrees with its durable ledger: {record}",
        )
    if real_backoff:
        for previous, current, minimum in zip(rows, rows[1:], (60, 120, 240, 480)):
            interval = (
                datetime.fromisoformat(current["admitted_at"].replace("Z", "+00:00"))
                - datetime.fromisoformat(previous["admitted_at"].replace("Z", "+00:00"))
            ).total_seconds()
            require(interval >= minimum,
                    f"production recovery backoff shortened: {interval} < {minimum}")
    return rows


def require_recovery_exhausted(
    snapshot: dict[str, Any], *, captured: dict[str, Any],
) -> dict[str, Any]:
    record, _ = task_identity(
        snapshot, agent_id=captured["agent_id"], task_id=captured["task_id"],
        parent_turn_id=captured["parent_turn_id"], captured=captured,
    )
    recovery_ledger(snapshot, record, expected_count=5, real_backoff=True)
    require(
        record["state"] != "settled" and record["disposition"] != "model_delivered"
        and record.get("settled_at") is None
        and record.get("recovery_paused_at") and record.get("recovery_diagnostic_at")
        and record.get("next_recheck_at") is None,
        f"exhausted obligation was consumed, lost, or still automatically scheduled: {record}",
    )
    diagnostics = [
        row for row in snapshot["briefs"]
        if row["evidence_id"]
        == f"brief_task_result_recovery_{record['result_identity']}_{record['recovery_epoch']}"
    ]
    require(
        len(diagnostics) == 1 and diagnostics[0]["kind"] == "failure"
        and diagnostics[0]["message_id"] == record["message_id"]
        and diagnostics[0]["work_item_id"] == record["work_item_id"],
        f"exhaustion must publish exactly one bound Failure diagnostic: {diagnostics}",
    )
    false_success = [
        row for row in snapshot["briefs"]
        if row["agent_id"] == record["agent_id"] and row["kind"] == "result"
        and (row["message_id"] == record["message_id"]
             or row["work_item_id"] == record["work_item_id"])
    ]
    require(not false_success, f"unconsumed exhausted task produced a Result brief: {false_success}")
    return record


def require_restart_preserved(
    before: dict[str, Any], after: dict[str, Any], *,
    captured: dict[str, Any], backoff: bool,
) -> None:
    records = []
    for snapshot in (before, after):
        record, _ = task_identity(
            snapshot, agent_id=captured["agent_id"], task_id=captured["task_id"],
            parent_turn_id=captured["parent_turn_id"], captured=captured,
        )
        records.append(record)
    left, right = records
    fields = ("recovery_epoch", "recovery_attempts", "last_recovery_root_attempt_id")
    if backoff:
        fields += ("next_recheck_at",)
        require(left.get("next_recheck_at"), "restart checkpoint was not in backoff")
    require(all(left.get(key) == right.get(key) for key in fields),
            f"same-volume restart reset recovery state: {left} -> {right}")
    require(recovery_ledger(before, left) == recovery_ledger(after, right),
            "same-volume restart recharged or dropped a recovery root")
