"""Safe parent-owned deletion with a shared runtime credential profile."""
from __future__ import annotations

import json
import secrets
import time
import urllib.parse
from typing import Any


def require_deleted(detail: dict[str, Any], deletion_id: str, incarnation: int) -> None:
    from .runner import require

    require(
        detail["identity"]["status"] == "deleted"
        and detail["identity"]["incarnation"] == incarnation
        and (detail.get("deletion") or {}).get("status") == "completed"
        and detail["deletion"]["deletion_id"] == deletion_id,
        f"deletion did not complete for the captured identity: {detail}",
    )


def run_case(harness: Any, case: dict[str, Any]) -> None:
    from .runner import require, result_value, write_json

    require(harness.tool_assertion_mode == "strict", "shared deletion requires strict tools")
    harness.initialize_workspace()
    harness.start()
    parent_id = harness.agent_id
    token = secrets.token_hex(6)
    profile = f"shared-delete-{token}"
    credential = harness.request(
        "PUT", f"/api/control/runtime/credentials/{profile}",
        {"kind": "api_key", "material": "deterministic-shared-credential"},
    )
    require(credential.get("ok") is True, "shared credential setup failed")
    updates = {
        "providers.openai.auth.source": "credential_profile",
        "providers.openai.auth.kind": "api_key",
        "providers.openai.auth.env": "",
        "providers.openai.auth.profile": profile,
        "providers.openai.auth.external": "",
        "model.default": harness.model,
    }
    configured = harness.request(
        "PATCH", "/api/control/runtime/config",
        {"updates": [{"key": key, "value": value} for key, value in updates.items()]},
    )
    write_json(harness.evidence / "shared-delete-profile.json", {
        "profile": profile, "updates": updates, "receipt": configured,
    })
    require(
        configured.get("changed") is True
        and not [r for r in configured.get("results", []) if r.get("effect") == "rejected"],
        f"shared profile configuration rejected: {configured}",
    )
    # Reload the parent before invoking children; all three resolve the same
    # persisted provider profile, not their independent environment credentials.
    harness.restart()

    def detail(agent_id: str, label: str) -> dict[str, Any]:
        value = harness.request(
            "GET", f"/api/control/agents/{urllib.parse.quote(agent_id, safe='')}/detail"
        )
        write_json(harness.evidence / f"{label}.json", value)
        return value

    def phase(label: str, operation: str, data: dict[str, Any],
              tools: list[str], *, stale: bool = False) -> list[dict[str, Any]]:
        marker = f"SHARED-DELETE-RESULT-{label}-{token}"
        prompt = (
            f"Shared credential deletion acceptance. SHARED-DELETE-OP:{operation} "
            f"SHARED-DELETE-DATA:{json.dumps(data, separators=(',', ':'))}\n"
            f"Reply exactly {marker} after the requested tools. "
            "Do not create files, WorkItems, timers, or other tools. "
            "For spawn invoke a new supervised child with workspace_mode=inherit, "
            "then GetAgent(detail=full) using its actual id. "
            "For delete/stale call DeleteAgent with the supplied captured identity. "
            "A stale rejection is expected; report the marker without retrying."
        )
        baseline, _ = harness.prompt(label, prompt)
        scope = harness.prompt_scope(label)
        if stale:
            events = [
                event for event in harness.events(f"{label}-tools")
                if event.get("type") in {"tool_executed", "tool_execution_failed"}
                and event.get("payload", {}).get("turn_id") == scope["turn_id"]
            ]
            require(
                len(events) == 1
                and events[0]["type"] == "tool_execution_failed"
                and events[0]["payload"].get("tool_name") == "DeleteAgent",
                f"stale deletion did not fail exactly once: {events}",
            )
            failure = harness.tool_detail(events[0], label)
            require(
                "agent_cleanup_stale_incarnation" in json.dumps(failure),
                f"stale deletion returned the wrong error: {failure}",
            )
        else:
            events = harness.assert_tools(
                label, baseline, tools, message_id=scope["message_id"],
            )
            require(
                [event["payload"].get("tool_name") for event in events] == tools,
                f"{label} tool count/order mismatch: {events}",
            )
        briefs = harness.request("GET", harness.agent_path("briefs?limit=30"))
        rows = briefs if isinstance(briefs, list) else briefs.get("briefs", [])
        matching = [
            row for row in rows
            if row.get("turn_id") == scope["turn_id"] and row.get("kind") == "result"
            and (row.get("text") or "").strip() == marker
        ]
        require(len(matching) == 1, f"{label} missing unique independent Result: {rows}")
        write_json(harness.evidence / f"{label}-result.json", matching[0])
        return events

    children: dict[str, dict[str, Any]] = {}
    for role in ("target", "sibling"):
        events = phase(f"shared-spawn-{role}", "spawn", {
            "child_marker": f"SHARED-DELETE-CHILD-{role}-{token}",
        }, ["InvokeAgent", "GetAgent"])
        invocation = result_value(harness.tool_detail(events[0], f"shared-{role}-invoke"))
        summary = result_value(harness.tool_detail(events[1], f"shared-{role}-identity"))
        identity = summary["agent"]["identity"]
        require(
            identity["agent_id"] == invocation["agent_id"]
            and identity["parent_agent_id"] == parent_id
            and identity["status"] == "active" and identity["incarnation"] >= 1,
            f"spawn did not capture the supervised identity: {identity}",
        )
        task_id = invocation["task_handle"]["task_id"]
        deadline = time.monotonic() + harness.timeout_seconds
        task: dict[str, Any] = {}
        while time.monotonic() < deadline:
            task = harness.request("GET", harness.agent_path(f"tasks/{task_id}"))
            if task.get("status") in {"completed", "failed", "cancelled"}:
                break
            time.sleep(0.25)
        require(task.get("status") == "completed",
                f"supervised child task did not complete: {task}")
        write_json(harness.evidence / f"shared-{role}-terminal-task.json", task)
        harness.agent_id = identity["agent_id"]
        try:
            harness.wait_queue_drained()
        finally:
            harness.agent_id = parent_id
        children[role] = identity
    require(children["target"]["agent_id"] != children["sibling"]["agent_id"],
            "target and retained sibling must be distinct")
    target = children["target"]
    captured = {"agent_id": target["agent_id"], "incarnation": target["incarnation"]}
    harness.wait_queue_drained()
    events = phase("shared-delete", "delete", captured, ["DeleteAgent"])
    admitted = result_value(harness.tool_detail(events[0], "shared-delete-admitted"))["deletion"]
    deletion_id = admitted["deletion_id"]
    require(
        admitted["agent_id"] == target["agent_id"]
        and admitted["requested_by"] == f"agent:{parent_id}"
        and admitted["cascade_private_children"] is False,
        f"wrong deletion target or authority: {admitted}",
    )
    deadline = time.monotonic() + harness.timeout_seconds
    completed: dict[str, Any] = {}
    while time.monotonic() < deadline:
        completed = detail(target["agent_id"], "shared-delete-progress")
        if (completed.get("deletion") or {}).get("status") in {"completed", "failed"}:
            break
        time.sleep(0.25)
    require_deleted(completed, deletion_id, target["incarnation"])
    write_json(harness.evidence / "shared-delete-completed.json", completed)

    def alive(label: str, agent_id: str) -> None:
        harness.agent_id = agent_id
        try:
            phase(label, "alive", {}, [])
        finally:
            harness.agent_id = parent_id

    alive("shared-parent-after-delete", parent_id)
    alive("shared-sibling-after-delete", children["sibling"]["agent_id"])
    harness.restart()
    require_deleted(detail(target["agent_id"], "shared-tombstone-restarted"),
                    deletion_id, target["incarnation"])
    alive("shared-parent-restarted", parent_id)
    alive("shared-sibling-restarted", children["sibling"]["agent_id"])
    events = phase("shared-delete-repeat", "delete", captured, ["DeleteAgent"])
    repeated = result_value(harness.tool_detail(events[0], "shared-delete-repeat"))["deletion"]
    require(repeated["deletion_id"] == deletion_id and repeated["status"] == "completed",
            f"same-incarnation replay created another job: {repeated}")

    recreated = harness.request(
        "POST", f"/api/control/agents/{urllib.parse.quote(target['agent_id'], safe='')}/create",
        {"authority_class": "operator_instruction", "template": None},
        expected_status=(200, 202),
    )
    write_json(harness.evidence / "shared-recreated.json", recreated)
    fresh = recreated["identity"]
    require(fresh["agent_id"] == target["agent_id"] and fresh["status"] == "active"
            and fresh["incarnation"] > target["incarnation"],
            f"same-id recreation did not advance incarnation: {recreated}")
    before_stale = detail(target["agent_id"], "shared-before-stale")
    phase("shared-delete-stale", "stale", captured, ["DeleteAgent"], stale=True)
    after_stale = detail(target["agent_id"], "shared-after-stale")
    require(
        after_stale["identity"] == before_stale["identity"]
        and after_stale.get("deletion") == before_stale.get("deletion"),
        f"stale request changed the new identity or admitted a job: {after_stale}",
    )
    harness.restart()
    restarted = detail(target["agent_id"], "shared-fresh-restarted")
    require(restarted["identity"]["incarnation"] == fresh["incarnation"]
            and restarted["identity"]["status"] == "active",
            f"recreated identity did not survive restart: {restarted}")
    alive("shared-fresh-restarted-result", target["agent_id"])
    alive("shared-parent-final", parent_id)
    alive("shared-sibling-final", children["sibling"]["agent_id"])
