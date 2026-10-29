"""Opt-in timer acceptance through the real daemon's provider admission path."""
from __future__ import annotations

import json
import re
import secrets
import sqlite3
import time
from typing import Any

from .runner import require, sqlite_rows, write_json


def snapshot(harness: Any, label: str, *, stopped: bool = False) -> dict[str, Any]:
    value = (
        harness.offline_runtime_db_snapshot(label)
        if stopped else harness.runtime_db_snapshot(label)
    )
    database = harness.evidence / f"{label}-runtime-state" / "runtime.sqlite"
    with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
        connection.row_factory = sqlite3.Row
        for table in (
            "timers", "timer_wakes", "model_requests", "work_items",
            "wait_conditions", "execution_protocol_work_items",
            "execution_protocol_outcomes",
        ):
            value[table] = sqlite_rows(connection, f"SELECT * FROM {table}")
    write_json(harness.evidence / f"{label}-timer-db.json", value)
    return value


def payload(row: dict[str, Any]) -> dict[str, Any]:
    return json.loads(row["payload_json"])


def provider_requests(path: Any, message_id: str, work_item_id: str) -> list[int]:
    """Link requests received by the stub, independently of optional trace rows."""
    matches = []
    for index, line in enumerate(path.read_text().splitlines(), 1):
        for item in json.loads(line).get("input", []):
            for content in item.get("content", []):
                text = content.get("text", "")
                if "## current_input\n" not in text:
                    continue
                current = text.split("## current_input\n", 1)[1]
                for rendered in re.findall(r"(?m)^  provenance: ([^\n]+)$", current):
                    provenance = json.loads(rendered)
                    if (
                        provenance.get("id") == message_id
                        and provenance.get("work_item_id") == work_item_id
                        and provenance.get("kind") == "timer_tick"
                        and provenance.get("authority_class") == "runtime_instruction"
                    ):
                        matches.append(index)
    require(matches, f"owned timer message {message_id} never reached provider transport")
    return sorted(set(matches))


def owned_wait(
    value: dict[str, Any], agent_id: str, work_item_id: str, timer_id: str,
    wait_id: str, status: str,
) -> dict[str, Any]:
    rows = [
        row for row in value["wait_conditions"]
        if row["wait_condition_id"] == wait_id
    ]
    require(len(rows) == 1, f"missing or duplicate exact timer wait: {rows}")
    row = rows[0]
    require(
        row["agent_id"] == agent_id and row["work_item_id"] == work_item_id
        and row["kind"] == "timer" and row["subject_ref"] == timer_id
        and row["status"] == status,
        f"timer wait ownership/status mismatch: {row}",
    )
    return row


def wait_rearmed(harness: Any, marker: str, previous_wait_id: str) -> dict[str, Any]:
    deadline = time.monotonic() + harness.timeout_seconds
    while time.monotonic() < deadline:
        matches = [
            item for item in harness.request(
                "GET", harness.agent_path("work-items?limit=50")
            ) if marker in item.get("objective", "")
        ]
        require(len(matches) == 1, f"timer WorkItem identity changed: {matches}")
        item = matches[0]
        require(item["state"] != "completed", "timer WorkItem skipped rearm barrier")
        wait_id = item.get("blocked_by_wait_id")
        if (
            item.get("scheduling_state") == "waiting_timer" and wait_id
            and wait_id != previous_wait_id
        ):
            require(
                [todo["state"] for todo in item["todo_list"]]
                == ["completed", "pending"],
                f"first timer dispatch did not advance exactly one todo: {item}",
            )
            write_json(harness.evidence / "timer-rearmed-work-item.json", item)
            return item
        # Condition-based polling; no fixed delay is used to manufacture a fire.
        time.sleep(0.1)
    raise TimeoutError("timer dispatch did not register a new exact timer wait")


def require_dispatch(
    before: dict[str, Any], after: dict[str, Any], *,
    agent_id: str, work_item_id: str, timer_id: str, wait_id: str,
) -> dict[str, Any]:
    owned_wait(before, agent_id, work_item_id, timer_id, wait_id, "active")
    wait = owned_wait(after, agent_id, work_item_id, timer_id, wait_id, "resolved")
    trigger = wait["trigger_message_id"]
    messages = [
        payload(row) for row in after["messages"] if row["message_id"] == trigger
    ]
    require(len(messages) == 1, "exact timer wait omitted its trigger message")
    message = messages[0]
    require(
        message["kind"] == "timer_tick"
        and message["origin"] == {"kind": "timer", "timer_id": timer_id}
        and message.get("work_item_id") == work_item_id,
        f"wait was not resumed by its owned timer: {message}",
    )
    attempts = [
        (row, payload(row)) for row in after["execution_protocol_attempts"]
        if payload(row).get("source_message_id") == trigger
    ]
    require(len(attempts) == 1, f"timer trigger was not admitted exactly once: {attempts}")
    row, attempt = attempts[0]
    identity = attempt["source"]["identity"]
    require(
        row["agent_id"] == agent_id and row["lifecycle_state"] == "settled"
        and attempt["binding"] == {"kind": "work_item", "work_item_id": work_item_id}
        and identity.get("kind") == "triggered_wait"
        and identity.get("wait_id") == wait_id
        and identity.get("trigger_message_id") == trigger,
        f"canonical timer attempt identity/binding mismatch: {attempt}",
    )
    prior_work = [
        work for work in before["work_items"] if work["work_item_id"] == work_item_id
    ]
    prior_protocol = [
        work for work in before["execution_protocol_work_items"]
        if work["agent_id"] == agent_id and work["work_item_id"] == work_item_id
    ]
    require(len(prior_work) == len(prior_protocol) == 1, "missing waiting owner fences")
    fences = attempt["admitted_fences"]
    require(
        fences.get("work_item_source_revision") == prior_work[0]["revision"] + 1
        and fences.get("work_item_generation") == prior_protocol[0]["generation"]
        and fences.get("source_revision") == attempt["source"]["generation"]
        == message["message_seq"]
        and fences.get("agent_control_revision", 0) > 0
        and fences.get("host_registry_revision", 0) > 0,
        f"timer admission retained stale/mismatched fences: {attempt}",
    )
    outcomes = [
        payload(outcome) for outcome in after["execution_protocol_outcomes"]
        if outcome["outcome_id"] == row["terminal_outcome_id"]
    ]
    require(
        len(outcomes) == 1 and outcomes[0]["attempt_id"] == row["attempt_id"],
        "timer attempt omitted its canonical terminal outcome",
    )
    requests = [
        request for request in after["model_requests"]
        if request["agent_id"] == agent_id and request["turn_id"] == attempt["turn_id"]
        and request["work_item_id"] == work_item_id
    ]
    wakes = [
        wake for wake in after["timer_wakes"]
        if wake["timer_id"] == timer_id and wake["message_id"] == trigger
    ]
    require(
        len(wakes) == 1 and wakes[0]["status"] == "incorporated",
        f"timer dispatch wake was not consumed by admission: {wakes}",
    )
    return {
        "wait_id": wait_id, "message_id": trigger, "attempt_id": row["attempt_id"],
        "turn_id": attempt["turn_id"], "admitted_fences": fences,
        "provider_request_ids": [request["evidence_id"] for request in requests],
    }


def run_case(harness: Any, case: dict[str, Any]) -> None:
    require(harness.provider_mode == "stub", "timer admission case is stub-only")
    harness.initialize_workspace()
    harness.start()
    marker = f"SCHEDULER-TIMER-ADMISSION-{secrets.token_hex(4)}"
    harness.prompt("timer-seed", case["phases"][0]["prompt"].format(
        case_id=case["id"], marker=marker,
    ))
    first = harness.wait_work_item_scheduling_state(
        objective_marker=marker, expected_scheduling_state="waiting_timer",
        label="timer-first-wait",
    )
    harness.wait_agent_idle()
    require(
        [todo["state"] for todo in first["todo_list"]] == ["pending", "pending"],
        "timer business progress preceded the first exact wait",
    )
    timers = harness.request("GET", harness.agent_path("timers?limit=50"))
    require(len(timers) == 1, f"expected one repeating timer: {timers}")
    timer = timers[0]
    require(
        timer["agent_id"] == harness.agent_id and timer["repeat"]
        and timer["interval_ms"] == 30000 and timer["fire_count"] == 0,
        f"first exact wait missed the pre-fire restart barrier: {timer}",
    )
    timer_id, wid, wait1 = timer["id"], first["id"], first.get("blocked_by_wait_id")
    require(bool(wait1), f"first timer wait omitted blocker ownership: {first}")
    # Inspect the stopped database so a fire during graceful shutdown cannot
    # accidentally count as the required post-restart dispatch.
    harness.stop()
    initial = snapshot(harness, "timer-before-restart", stopped=True)
    stopped_timers = [row for row in initial["timers"] if row["timer_id"] == timer_id]
    require(
        len(stopped_timers) == 1 and stopped_timers[0]["fire_count"] == 0,
        "timer fired before the daemon reached the stopped restart barrier",
    )
    owned_wait(initial, harness.agent_id, wid, timer_id, wait1, "active")
    require(
        len([row for row in initial["work_items"] if row["agent_id"] == harness.agent_id]) == 1,
        "timer scenario created more than one WorkItem",
    )
    harness.start(wait_idle=False)
    second = wait_rearmed(harness, marker, wait1)
    require(second["id"] == wid, "restart changed timer WorkItem owner")
    harness.wait_agent_idle()
    middle = snapshot(harness, "timer-after-dispatch-1")
    wait2 = second["blocked_by_wait_id"]
    owned_wait(middle, harness.agent_id, wid, timer_id, wait2, "active")
    dispatch1 = require_dispatch(
        initial, middle, agent_id=harness.agent_id, work_item_id=wid,
        timer_id=timer_id, wait_id=wait1,
    )
    completed = harness.wait_work_item(
        objective_marker=marker, expected_state="completed", label="timer-completed",
    )
    harness.wait_agent_idle()
    final = snapshot(harness, "timer-after-dispatch-2")
    dispatch2 = require_dispatch(
        middle, final, agent_id=harness.agent_id, work_item_id=wid,
        timer_id=timer_id, wait_id=wait2,
    )
    for dispatch in (dispatch1, dispatch2):
        dispatch["stub_request_lines"] = provider_requests(
            harness.evidence / "stub-requests.jsonl", dispatch["message_id"], wid,
        )
    require(
        completed["id"] == wid and all(
            todo["state"] == "completed" for todo in completed["todo_list"]
        ) and len(completed["todo_list"]) == 2,
        f"second timer dispatch did not complete both business todos: {completed}",
    )
    final_timer = harness.request("GET", harness.agent_path(f"timers/{timer_id}"))
    require(final_timer["status"] == "cancelled", "repeating timer was not cancelled")
    require(
        not [row for row in final["execution_protocol_attempts"]
             if row["agent_id"] == harness.agent_id and row["lifecycle_state"] == "open"],
        "timer case retained an open execution attempt",
    )
    harness.assert_stub_complete()
    status = json.loads((harness.evidence / "stub-status.json").read_text())
    require(
        [(entry["phase"], entry["message_id"]) for entry in status["timer_dispatch_requests"]]
        == [(4, dispatch1["message_id"]), (7, dispatch2["message_id"])],
        f"provider transcript did not match both canonical dispatches: {status}",
    )
    brief = harness.brief(completed["result_brief_id"], "timer-result")
    require(f"{marker}-COMPLETE" in json.dumps(brief), "timer completion brief is missing")
    write_json(harness.evidence / "timer-admission-oracle.json", {
        "agent_id": harness.agent_id, "work_item_id": wid, "timer_id": timer_id,
        "restart_after_wait_id": wait1, "rearmed_wait_id": wait2,
        "dispatches": [dispatch1, dispatch2], "stub_status": status,
        "business_todos": completed["todo_list"], "timer_status": final_timer["status"],
    })
