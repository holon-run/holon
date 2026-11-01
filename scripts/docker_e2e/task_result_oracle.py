"""Fail-closed task-result acceptance shared by deterministic and live runners."""
from __future__ import annotations

import json
import re
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


def rfc3339_order_key(value: str) -> tuple[datetime, int]:
    """Preserve Rust ACK nanoseconds without relying on Python's ISO fraction support."""
    parts = re.fullmatch(
        r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})",
        value,
    )
    require(parts is not None, "invalid RFC3339 timestamp")
    seconds = datetime.fromisoformat(parts[1] + parts[3].replace("Z", "+00:00"))
    return seconds, int((parts[2] or "").ljust(9, "0"))


def same_durable_instant(left: str, right: str) -> bool:
    """Compare instants whose durable column may truncate below milliseconds."""
    left_seconds, left_nanos = rfc3339_order_key(left)
    right_seconds, right_nanos = rfc3339_order_key(right)
    return left_seconds == right_seconds and left_nanos // 1_000_000 == right_nanos // 1_000_000


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
    require_completion: bool = True,
) -> dict[str, Any]:
    rows = [
        row for row in snapshot["briefs"]
        if (row["evidence_id"] == brief_id if brief_id else
            row["agent_id"] == agent_id and row.get("turn_id") == turn_id
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
    if work_item_id is not None and require_completion:
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


def task_result_consuming_turns(
    snapshot, record, *, admitted_turn_id=None, carrier_message_id=None,
):
    carriers = {
        row["message_id"] for row in snapshot["messages"]
        if payload(row).get("source_refs", {}).get("task_result_identity")
        == record["result_identity"]
    }
    carriers.add(record["message_id"])
    if carrier_message_id is not None:
        carriers.add(carrier_message_id)
    turns = []
    for row in snapshot["turn_records"]:
        if row["agent_id"] != record["agent_id"] or row["terminal_kind"] != "completed":
            continue
        stored = payload(row) if "payload_json" in row else {}
        terminal = stored.get("terminal") or {}
        # A suppressed source can finish without any model re-entry.
        no_brief = terminal.get("no_brief_reason")
        reducer_reason = no_brief.get("reason") if isinstance(no_brief, dict) else None
        if (
            row["turn_id"] != admitted_turn_id
            and terminal.get("kind") == "completed"
            and reducer_reason in {"parent_turn_already_delivered", "model_reentry_suppressed"}
            and terminal.get("reason") == f"reducer_only/{reducer_reason}"
            and terminal.get("no_brief_reason") == {
                "kind": "reducer_only", "reason": reducer_reason,
            }
            and stored.get("external_side_effect_possible") is False
            and not any(stored.get(key) for key in (
                "idle_input_batch", "live_input_batches",
                "produced_brief_ids", "completed_work_item_ids",
            ))
            and not any(brief.get("turn_id") == row["turn_id"]
                        for brief in snapshot.get("briefs", []))
        ):
            continue
        inputs = list(stored.get("input_message_ids", []))
        if stored.get("idle_input_batch"):
            inputs += [item["message_id"] for item in stored["idle_input_batch"]["items"]]
        inputs += [
            item["message_id"]
            for batch in stored.get("live_input_batches", []) for item in batch["items"]
        ]
        if (row["turn_id"] == admitted_turn_id
                or row["trigger_message_id"] in carriers or carriers & set(inputs)):
            turns.append(row)
    return turns


def require_task_reentry(
    snapshot: dict[str, Any], *, agent_id: str, task_id: str,
    parent_turn_id: str, captured: dict[str, Any] | None = None,
    require_completion: bool = True, wait_policy: str = "exact",
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
    require(wait_policy in {"exact", "none"}, "unknown task-result wait policy")
    require(
        not waits if wait_policy == "none" else bool(waits)
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
    hinted_turns = [
        row for row in snapshot["turn_records"]
        if row["agent_id"] == agent_id and row["turn_id"] == attempt.get("turn_id")
    ]
    # Replay retains the ingress attachment on the attempt, but executes a new
    # turn. Its input batch must name this exact activation, not just the source.
    replays = []
    for row in snapshot["turn_records"]:
        if row["agent_id"] != agent_id or row.get("terminal_kind") != "completed":
            continue
        stored = payload(row) if "payload_json" in row else {}
        replay = stored.get("replay") or {}
        batch = stored.get("idle_input_batch") or {}
        if (
            attempt.get("turn_id") is not None
            and replay.get("source_turn_id") == attempt.get("turn_id")
            and replay.get("source_message_id") == attempt.get("source_message_id")
            and replay.get("reason") == "interrupted_queue_claim_reentry"
            and (replay.get("prior_terminal") or {}).get("kind") == "interrupted"
            and batch.get("activation_id") == record["activation_id"]
            and attempt.get("source_message_id")
            in {item.get("message_id") for item in batch.get("items", [])}
            and row.get("trigger_message_id") == attempt.get("source_message_id")
        ):
            replays.append(row)
    if replays:
        require(
            len(hinted_turns) == 1 and hinted_turns[0].get("terminal_kind") == "interrupted"
            and hinted_turns[0].get("current_work_item_id") == record["work_item_id"]
            and hinted_turns[0].get("trigger_message_id") == attempt.get("source_message_id")
            and attempt.get("recovery_of_attempt_id"),
            "replay execution has no unique interrupted ingress attachment",
        )
        turns = replays
    else:
        # Recovery attempts need not retain turn/run hints. The durable source,
        # owner and completed turn are authoritative; present hints must agree.
        turns = [
            row for row in task_result_consuming_turns(
                snapshot, record, carrier_message_id=attempt.get("source_message_id"),
            )
            if row.get("trigger_message_id") == attempt.get("source_message_id")
            and row.get("current_work_item_id") == record["work_item_id"]
        ]
    require(
        len(turns) <= 1,
        f"task result caused duplicate successful consumption: {turns}",
    )
    hint_targets = hinted_turns if replays else turns
    require(
        (attempt.get("turn_id") is None or (
            len(hint_targets) == 1 and hint_targets[0]["turn_id"] == attempt["turn_id"]
        ))
        and (attempt.get("run_id") is None or (
            len(hint_targets) == 1 and hint_targets[0].get("run_id") == attempt["run_id"]
        )),
        f"activation turn/run hint disagrees with its actual owner turn: {attempt}",
    )
    require(
        activation["lifecycle_state"] == "settled"
        and attempt.get("attempt_id") == activation["attempt_id"]
        and attempt.get("agent_id") == agent_id
        and bool(attempt.get("source_message_id"))
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
            and source_turns[0]["trigger_message_id"] == causal.get("source_message_id")
            and (causal.get("run_id") is None
                 or source_turns[0].get("run_id") == causal["run_id"]),
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
    require(wait_policy == "exact" or scanner,
            "unwaited consumption requires the validated scanner source and model input")
    # Validated recovery wakes also carry this promise; their completed model
    # turns must not hide duplicate consumption of the same task result.
    successful = task_result_consuming_turns(
        snapshot, record, admitted_turn_id=turn["turn_id"],
        carrier_message_id=source_message_id,
    )
    require(
        len(successful) == 1,
        f"task result caused duplicate successful consumption: {successful}",
    )
    if require_completion:
        require_result_brief(
            snapshot, agent_id=agent_id, turn_id=turn["turn_id"],
            message_id=turn["trigger_message_id"], work_item_id=record["work_item_id"],
            brief_id=terminal.get("outcome", {}).get("completion"),
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
    carriers = {
        row["message_id"] for row in snapshot["messages"]
        if payload(row).get("source_refs", {}).get("task_result_identity")
        == record["result_identity"]
    } | {record["message_id"]}
    false_success = [
        row for row in snapshot["briefs"]
        if row["agent_id"] == record["agent_id"] and row["kind"] == "result"
        and row["message_id"] in carriers
    ]
    require(not false_success and not task_result_consuming_turns(snapshot, record),
            f"unconsumed exhausted task produced successful consumption: {false_success}")
    return record


def require_first_admission(before, *, captured, order, held, scanner, released,
                            request_id, expected_build, after=None):
    """The held loser and durable canonical winner, not HTTP order, prove the race."""
    require(order in {"exact-first", "scanner-first"}, "unknown first-admission order")
    require("[recovery-acceptance-test]" in expected_build,
            "controlled order requires the separately identified test binary")
    record, _ = task_identity(
        before, agent_id=captured["agent_id"], task_id=captured["task_id"],
        parent_turn_id=captured["parent_turn_id"], captured=captured,
    )
    fence = {
        "result_message_id": record["message_id"],
        **{key: record[key] for key in (
            "result_identity", "rejoin_generation", "parent_turn_id", "recovery_epoch",
        )},
    }
    scanner_first = order == "scanner-first"
    events = [(held, "task-result-held" if scanner_first else "scanner-held")]
    if scanner_first:
        events.append((scanner, "scanner-enqueued"))
    if released is not None:
        events.append((released, "task-result-released" if scanner_first else "scanner-released"))
    for event, name in events:
        require(
            isinstance(event, dict) and event.get("protocol") == 1
            and event.get("event") == name and event.get("request_id") == request_id
            and event.get("build") == expected_build and event.get("fence") == fence
            and all(event.get(key) == record[key]
                    for key in ("agent_id", "task_id", "work_item_id")),
            f"missing or incorrectly fenced controlled-order ACK: {name}",
        )
    require(held.get("recovery_attempts") == 0
            and not held.get("last_recovery_root_attempt_id"),
            "gate was not armed before first recovery admission")
    if released is not None:
        require(rfc3339_order_key(released["observed_at"])
                >= rfc3339_order_key(held["observed_at"]),
                "losing source released before it was held")
    source_id = scanner["source_message_id"] if scanner_first else record["message_id"]
    attempt_rows = [row for row in before["execution_protocol_attempts"]
                    if row["agent_id"] == record["agent_id"]
                    and row["attempt_id"] == record["activation_id"]]
    require(record["state"] == "caller_admitted" and not record.get("settled_at")
            and len(attempt_rows) == 1 and attempt_rows[0].get("lifecycle_state") == "open",
            "selected first canonical attempt is not actually open and admitted")
    attempts = [payload(row) for row in attempt_rows]
    winners = [attempt for attempt in attempts if attempt["attempt_id"] == record["activation_id"]]
    require(len(winners) == 1 and winners[0].get("source_message_id") == source_id
            and not winners[0].get("recovery_of_attempt_id")
            and winners[0].get("binding") == {
                "kind": "work_item", "work_item_id": captured["work_item_id"],
            }
            and recovery_root(before, record["agent_id"], record["activation_id"])
            == record["activation_id"],
            "first canonical claim was not won by the selected immutable source")
    expected_count = int(scanner_first)
    ledger = recovery_ledger(before, record, expected_count=expected_count)
    if scanner_first:
        sources = [payload(row) for row in before["messages"] if row["message_id"] == source_id]
        require(len(sources) == 1 and scanner.get("source") == sources[0],
                "scanner ACK is not backed by its durable runtime source")
        source = sources[0]
        require(
            source.get("kind") == "internal_followup"
            and source.get("origin") == {"kind": "system", "subsystem": "task_result_recovery"}
            and source.get("authority_class") == "runtime_instruction"
            and source.get("admission_context") == "runtime_owned"
            and source.get("agent_id") == record["agent_id"]
            and source.get("work_item_id") == record["work_item_id"]
            and source.get("delivery_surface") == "runtime_system"
            and all(source.get("source_refs", {}).get(key) == value for key, value in {
                "task_result_message_id": record["message_id"],
                "task_result_identity": record["result_identity"],
                "task_result_rejoin_generation": str(record["rejoin_generation"]),
                "task_result_parent_turn_id": record["parent_turn_id"],
            }.items()),
            "first scanner source has invalid provenance or result fence",
        )
        queues = [row for row in before["queue_entries"]
                  if row["message_id"] == record["message_id"]]
        require(len(queues) == 1 and queues[0]["status"] == "queued"
                and held.get("source_message_id") == record["message_id"]
                and held.get("queue_entry", {}).get("status") == "queued"
                and ledger[0]["root_attempt_id"] == record["activation_id"]
                and scanner.get("queue_entry", {}).get("message_id") == source_id
                and scanner.get("queue_entry", {}).get("status") == "queued",
                "original result was not held while the scanner root was charged")
    else:
        require(winners[0].get("source", {}).get("identity", {}).get("kind") == "task_result",
                "exact-first winner was not the actual TaskResult")
    if after is not None:
        require(released is not None, "controlled-order release checkpoint was skipped")
        final, _ = task_identity(
            after, agent_id=record["agent_id"], task_id=record["task_id"],
            parent_turn_id=record["parent_turn_id"], captured=captured,
        )
        require(final["recovery_epoch"] == record["recovery_epoch"]
                and recovery_ledger(after, final, expected_count=expected_count) == ledger
                and recovery_root(after, final["agent_id"], final["activation_id"])
                == record["activation_id"],
                "losing source created a second root or changed the charged identity")
    return source_id


def require_sixth_rejected(before, after, *, captured, enqueued, rejected,
                          request_id, expected_build):
    """Attest a runtime-owned, correctly fenced source reached budget rejection."""
    left = require_recovery_exhausted(before, captured=captured)
    right = require_recovery_exhausted(after, captured=captured)
    require("[recovery-acceptance-test]" in expected_build,
            "sixth-source evidence requires the separately identified test binary")
    require(
        all(left[key] == right[key] for key in (
            *IDENTITY_FIELDS, "recovery_epoch", "last_recovery_root_attempt_id",
        ))
        and all(same_durable_instant(left[key], right[key]) for key in (
            "recovery_paused_at", "recovery_diagnostic_at",
        ))
        and recovery_ledger(before, left) == recovery_ledger(after, right),
        "sixth-source rejection changed the exhausted obligation or ledger",
    )
    fence = {
        "result_message_id": right["message_id"],
        **{key: right[key] for key in (
            "result_identity", "rejoin_generation", "parent_turn_id", "recovery_epoch",
        )},
    }
    source_id = f"msg_task_result_recovery_acceptance_{request_id}"
    for event, name in ((enqueued, "sixth-enqueued"), (rejected, "budget-rejected")):
        require(
            event.get("protocol") == 1 and event.get("event") == name
            and event.get("request_id") == request_id
            and event.get("build") == expected_build and event.get("fence") == fence
            and all(event.get(key) == right[key]
                    for key in ("agent_id", "task_id", "work_item_id"))
            and event.get("source_message_id") == source_id
            and event.get("recovery_attempts") == 5
            and event.get("last_recovery_root_attempt_id")
            == right["last_recovery_root_attempt_id"]
            and same_durable_instant(event.get("recovery_paused_at") or "",
                                     right["recovery_paused_at"]),
            f"missing or incorrectly fenced private acceptance event: {name}",
        )
    require(
        rejected.get("rejection_stage") == "scheduler_canonical_claim"
        and rejected.get("reason") == "recovery_budget_exhausted",
        "sixth source did not reach canonical budget rejection",
    )
    messages = [payload(row) for row in after["messages"]
                if row["message_id"] == source_id]
    require(len(messages) == 1, "sixth source is missing from durable runtime messages")
    source = messages[0]
    require(
        source.get("id") == source_id and source.get("agent_id") == right["agent_id"]
        and source.get("work_item_id") == right["work_item_id"]
        and source.get("kind") == "internal_followup"
        and source.get("origin") == {"kind": "system", "subsystem": "task_result_recovery"}
        and source.get("authority_class") == "runtime_instruction"
        and source.get("admission_context") == "runtime_owned"
        and source.get("delivery_surface") == "runtime_system"
        and all(source.get("source_refs", {}).get(key) == value for key, value in {
            "task_result_message_id": right["message_id"],
            "task_result_identity": right["result_identity"],
            "task_result_rejoin_generation": str(right["rejoin_generation"]),
            "task_result_parent_turn_id": right["parent_turn_id"],
            "recovery_acceptance_request_id": request_id,
        }.items()),
        "sixth source is not the correctly fenced runtime-owned recovery envelope",
    )
    for event in (enqueued, rejected):
        require(event.get("source") == source
                and event.get("queue_entry", {}).get("message_id") == source_id,
                "private acceptance ACK is not backed by the durable queued envelope")
    queues = [row for row in after["queue_entries"] if row["message_id"] == source_id]
    require(len(queues) == 1 and queues[0]["status"] in {"processed", "dropped"},
            "sixth source never completed normal queue processing")
    require(not any(
        payload(row).get("source_message_id") == source_id
        or payload(row).get("source", {}).get("source_message_id") == source_id
        for row in after["execution_protocol_attempts"]
    ), "rejected sixth source opened an execution attempt")


def require_coexisting_fairness(before, after, *, captured, fresh):
    left = require_recovery_exhausted(before, captured=captured)
    right = require_recovery_exhausted(after, captured=captured)
    fields = (
        "state", "disposition", "settled_at", "recovery_epoch", "recovery_attempts",
        "last_recovery_root_attempt_id", "next_recheck_at",
        "recovery_paused_at", "recovery_diagnostic_at",
    )
    require(all(left.get(key) == right.get(key) for key in fields)
            and recovery_ledger(before, left) == recovery_ledger(after, right),
            "fairness drill changed or rearmed the exhausted obligation")
    require(
        fresh["agent_id"] == captured["agent_id"]
        and fresh["work_item_id"] == captured["work_item_id"]
        and fresh["work_item_id"] is not None
        and all(fresh[key] != captured[key]
                for key in ("task_id", "result_identity", "message_id", "parent_turn_id"))
        and not any(row["task_id"] == fresh["task_id"] for row in before["tasks"]),
        "fairness requires a new real task under the same captured owner",
    )
    owners = [
        row for row in after["work_items"]
        if row["agent_id"] == captured["agent_id"]
        and row["work_item_id"] == captured["work_item_id"]
    ]
    require(len(owners) == 1 and owners[0]["state"] == "open"
            and not owners[0].get("completed_at"),
            "fairness must not complete the shared owner and revoke the exhausted result")
    require_task_reentry(
        after, agent_id=fresh["agent_id"], task_id=fresh["task_id"],
        parent_turn_id=fresh["parent_turn_id"], captured=fresh, require_completion=False,
    )
    record, _ = task_identity(
        after, agent_id=fresh["agent_id"], task_id=fresh["task_id"],
        parent_turn_id=fresh["parent_turn_id"], captured=fresh,
    )
    recovery_ledger(after, record, expected_count=0)
    activation = next(payload(row) for row in after["execution_protocol_attempts"]
                      if row["agent_id"] == fresh["agent_id"]
                      and row["attempt_id"] == record["activation_id"])
    require_result_brief(
        after, agent_id=fresh["agent_id"], turn_id=activation["turn_id"],
        message_id=activation["source_message_id"], work_item_id=fresh["work_item_id"],
        require_completion=False,
    )


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
