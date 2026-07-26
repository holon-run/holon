#!/usr/bin/env python3
"""Resumable host-side scheduler shadow and cutover drill."""

from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import shutil
import sqlite3
import stat
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .runner import (
    CaseHarness,
    image_identity,
    normalize_model_route,
    parse_env_file,
    require,
    run,
    secret_scan,
    utc_now,
    write_json,
)


ROOT = Path(__file__).resolve().parents[2]
RUN_SCHEMA_VERSION = 1
EVIDENCE_SCHEMA_VERSION = 1
DEFAULT_PRIMARY_MODEL = "volcengine@plan/glm-5.2"
DEFAULT_FALLBACK_MODELS = ["dashscope@token-plan/qwen3.8-max-preview"]
REQUIRED_CREDENTIAL_ENVS = [
    "VOLCENGINE_AGENT_API_KEY",
    "DASHSCOPE_TOKEN_PLAN_API_KEY",
]
PRODUCTION_SCENARIOS = (
    "reducer_only_candidates",
    "work_item_autonomous_continuation",
    "exact_task_rejoin",
    "exact_wait_resume",
    "explicitly_bound_operator_input",
    "operator_interjection",
    "settlement",
    "delivery",
)
RUN_ID_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-]{5,63}$")


@dataclass(frozen=True)
class DrillPaths:
    root: Path
    run_json: Path
    phases: Path
    snapshots: Path
    workspace: Path

    @classmethod
    def from_root(cls, root: Path) -> "DrillPaths":
        resolved = root.resolve()
        return cls(
            root=resolved,
            run_json=resolved / "run.json",
            phases=resolved / "phases",
            snapshots=resolved / "snapshots",
            workspace=resolved / "workspace",
        )


def default_run_id() -> str:
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    return f"drill-{timestamp}-{secrets.token_hex(3)}"


def default_run_root(run_id: str) -> Path:
    return ROOT / "target" / "scheduler-drill" / run_id


def secret_root() -> Path:
    configured = os.environ.get("HOLON_DRILL_SECRET_ROOT", "").strip()
    return (
        Path(configured).expanduser().resolve()
        if configured
        else (ROOT / "target" / "scheduler-drill-secrets").resolve()
    )


def token_path(run_id: str) -> Path:
    return secret_root() / run_id / "control-token"


def write_control_token(run_id: str, token: str) -> None:
    path = token_path(run_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.parent.chmod(0o700)
    path.write_text(token + "\n")
    path.chmod(0o600)


def read_control_token(run_id: str) -> str:
    path = token_path(run_id)
    require(path.is_file(), f"control token is unavailable for {run_id}")
    mode = stat.S_IMODE(path.stat().st_mode)
    require(mode & 0o077 == 0, "control token must not be group/world accessible")
    token = path.read_text().strip()
    require(bool(token), "control token file is empty")
    return token


def delete_control_token(run_id: str) -> None:
    directory = token_path(run_id).parent
    shutil.rmtree(directory, ignore_errors=True)


def load_record(paths: DrillPaths) -> dict[str, Any]:
    require(paths.run_json.is_file(), f"run record does not exist: {paths.run_json}")
    record = json.loads(paths.run_json.read_text())
    require(
        record.get("schema_version") == RUN_SCHEMA_VERSION,
        f"unsupported run schema: {record.get('schema_version')}",
    )
    return record


def save_record(paths: DrillPaths, record: dict[str, Any]) -> None:
    temporary = paths.run_json.with_suffix(".json.tmp")
    write_json(temporary, record)
    temporary.replace(paths.run_json)


def append_phase(
    paths: DrillPaths,
    record: dict[str, Any],
    *,
    action: str,
    status: str,
    detail: dict[str, Any] | None = None,
) -> None:
    record.setdefault("phase_history", []).append(
        {
            "action": action,
            "status": status,
            "at": utc_now(),
            "detail": detail or {},
        }
    )
    record["updated_at"] = utc_now()
    save_record(paths, record)


def credential_env_file(value: str | Path | None) -> Path | None:
    configured = str(value or os.environ.get("HOLON_DRILL_ENV_FILE", "")).strip()
    if not configured:
        return None
    path = Path(configured).expanduser().resolve()
    require(path.is_file(), f"credential env file does not exist: {path}")
    mode = stat.S_IMODE(path.stat().st_mode)
    require(mode & 0o077 == 0, "credential env file must have mode 0600 or stricter")
    values = parse_env_file(path)
    missing = [name for name in REQUIRED_CREDENTIAL_ENVS if not values.get(name)]
    require(not missing, f"credential env file is missing: {', '.join(missing)}")
    return path


def credential_env_names(env_file: Path | None) -> list[str]:
    if env_file is not None:
        return []
    missing = [name for name in REQUIRED_CREDENTIAL_ENVS if not os.environ.get(name)]
    require(
        not missing,
        "set HOLON_DRILL_ENV_FILE or export credentials: " + ", ".join(missing),
    )
    return list(REQUIRED_CREDENTIAL_ENVS)


def validate_record(paths: DrillPaths, record: dict[str, Any]) -> None:
    require(
        Path(record["run_dir"]).resolve() == paths.root,
        "run directory does not match run.json",
    )
    identity = image_identity(record["image"]["ref"])
    require(identity["id"] == record["image"]["id"], "Docker image identity changed")
    require(
        identity["repo_digests"] == record["image"]["repo_digests"],
        "Docker image digest set changed",
    )
    git_sha = run(["git", "rev-parse", "HEAD"]).stdout.strip()
    require(git_sha == record["git_sha"], "Git SHA changed since prepare")
    volume = run(
        ["docker", "volume", "inspect", record["resources"]["volume"]],
        check=False,
    )
    require(volume.returncode == 0, "candidate Docker volume is missing")
    require(
        Path(record["resources"]["workspace_parent"]).resolve() == paths.workspace,
        "candidate workspace path changed",
    )


def resource_names(record: dict[str, Any]) -> dict[str, str]:
    return {
        "volume": record["resources"]["volume"],
        "network": record["resources"]["network"],
        "container": record["resources"]["container"],
        "workspace_parent": record["resources"]["workspace_parent"],
    }


def make_harness(
    paths: DrillPaths,
    record: dict[str, Any],
    *,
    label: str,
    mode: str,
    env_file: Path | None,
    require_credentials: bool = True,
) -> CaseHarness:
    return CaseHarness(
        case_id=label,
        image=record["image"]["ref"],
        model=record["models"]["primary"],
        model_fallbacks=list(record["models"]["fallbacks"]),
        disable_provider_fallback=False,
        credential_envs=(
            credential_env_names(env_file) if require_credentials else []
        ),
        env_file=env_file,
        runtime_env={
            "HOLON_SCHEDULER": mode,
            "HOLON_SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS": "true",
        },
        evidence_root=paths.phases,
        timeout_seconds=int(record["parameters"]["timeout_seconds"]),
        keep=True,
        resource_names=resource_names(record),
        control_token=read_control_token(record["drill_run_id"]),
    )


def attach_running(harness: CaseHarness) -> None:
    result = harness.docker("port", harness.container, "7878/tcp", check=False)
    require(result.returncode == 0 and result.stdout.strip(), "container port is absent")
    port = result.stdout.strip().splitlines()[0].rsplit(":", 1)[-1]
    harness.base_url = f"http://127.0.0.1:{port}"
    harness.wait_readiness()


def wait_for_turn_after(harness: CaseHarness, baseline_turn: int, label: str) -> None:
    deadline = datetime.now(timezone.utc).timestamp() + harness.timeout_seconds
    last_state: dict[str, Any] | None = None
    while datetime.now(timezone.utc).timestamp() < deadline:
        last_state = harness.request("GET", harness.agent_path("state"))
        agent = last_state["agent"]["agent"]
        if (
            int(agent["turn_index"]) > baseline_turn
            and agent["status"] in {"awake_idle", "asleep", "awaiting_task"}
            and agent.get("current_run_id") is None
            and int(last_state["session"]["pending_count"]) == 0
        ):
            write_json(harness.evidence / f"{label}-state.json", last_state)
            return
        import time

        time.sleep(1)
    write_json(harness.evidence / f"{label}-timeout-state.json", last_state)
    raise TimeoutError(f"timed out waiting for {label}")


def wait_for_running_turn(
    harness: CaseHarness,
    *,
    baseline_turn: int,
    label: str,
) -> None:
    import time

    deadline = time.monotonic() + harness.timeout_seconds
    while time.monotonic() < deadline:
        state = harness.request("GET", harness.agent_path("state"))
        agent = state["agent"]["agent"]
        if (
            int(agent["turn_index"]) >= baseline_turn
            and agent.get("current_run_id") is not None
        ):
            write_json(harness.evidence / f"{label}-state.json", state)
            return
        time.sleep(0.5)
    raise TimeoutError(f"timed out waiting for running turn in {label}")


def seed_wait(
    harness: CaseHarness,
    *,
    label: str,
    marker: str,
    wake: str = "external",
    resource: str | None = None,
) -> dict[str, Any]:
    objective = f"DRILL-WAIT-{label}-{marker}"
    completion = f"DRILL-WAIT-COMPLETE-{label}-{marker}"
    resource_argument = (
        f", resource={json.dumps(resource)}"
        if resource is not None
        else ""
    )
    baseline, _ = harness.prompt(
        f"{label}-seed",
        "Scheduler drill. Create exactly one WorkItem with objective "
        f"{objective}, plan_status ready, and one todo named resume pending. "
        f"Pick that WorkItem. Call WaitFor with wake={wake}"
        f"{resource_argument}, and a concrete reason. "
        "Do not complete it in this turn. When this exact WorkItem resumes, call "
        "GetWorkItem, update the existing todo to completed, emit a concise completion "
        f"report containing {completion}, and immediately call CompleteWorkItem for "
        "the exact current WorkItem. Do not create another WorkItem or modify files.",
    )
    item = harness.wait_work_item_scheduling_state(
        objective_marker=objective,
        expected_scheduling_state="waiting_external",
        label=f"{label}-waiting",
    )
    return {
        "baseline_turn": baseline,
        "objective": objective,
        "completion": completion,
        "work_item_id": item["id"],
    }


def wait_seed_completion(
    harness: CaseHarness,
    seed: dict[str, Any],
    label: str,
) -> dict[str, Any]:
    return harness.wait_work_item(
        objective_marker=seed["objective"],
        expected_state="completed",
        label=f"{label}-completed",
    )


def exercise_reducer_ingress(harness: CaseHarness, marker: str) -> None:
    webhook = harness.request(
        "POST",
        f"/api/webhooks/generic/{harness.agent_id}",
        {"drill": marker, "surface": "webhook"},
    )
    write_json(harness.evidence / "reducer-webhook.json", webhook)
    harness.wait_agent_idle()
    channel = harness.request(
        "POST",
        harness.agent_path("enqueue"),
        {
            "kind": "channel_event",
            "json": {"drill": marker, "surface": "channel"},
            "origin": {
                "kind": "channel",
                "channel_id": f"drill-{marker}",
                "sender_id": "scheduler-drill",
            },
        },
    )
    write_json(harness.evidence / "reducer-channel.json", channel)
    harness.wait_agent_idle()


def exercise_wait_triggers(harness: CaseHarness, marker: str) -> None:
    callback_seed = seed_wait(
        harness,
        label="callback",
        marker=marker,
        resource=f"drill:callback:{marker}",
    )
    callback = harness.reset_callback("wait-callback-capability")
    harness.fire_callback(
        "wait-callback-trigger",
        callback["trigger_url"],
        {"drill": marker, "trigger": "callback"},
    )
    wait_seed_completion(harness, callback_seed, "callback")

    webhook_seed = seed_wait(
        harness,
        label="webhook",
        marker=marker,
        resource=f"drill:webhook:{marker}",
    )
    harness.request(
        "POST",
        f"/api/webhooks/generic/{harness.agent_id}",
        {"drill": marker, "trigger": "webhook"},
    )
    wait_seed_completion(harness, webhook_seed, "webhook")

    channel_seed = seed_wait(
        harness,
        label="channel",
        marker=marker,
        resource=f"drill:channel:{marker}",
    )
    harness.request(
        "POST",
        harness.agent_path("enqueue"),
        {
            "kind": "channel_event",
            "json": {"drill": marker, "trigger": "channel"},
            "origin": {
                "kind": "channel",
                "channel_id": f"drill-wait-{marker}",
                "sender_id": "scheduler-drill",
            },
        },
    )
    wait_seed_completion(harness, channel_seed, "channel")

    timer = harness.request(
        "POST",
        harness.agent_path("timers", control=True),
        {
            "duration_ms": 5000,
            "summary": f"scheduler drill timer {marker}",
        },
    )
    write_json(harness.evidence / "wait-timer.json", timer)
    timer_seed = seed_wait(
        harness,
        label="timer",
        marker=marker,
        wake="timer",
        resource=timer["id"],
    )
    wait_seed_completion(harness, timer_seed, "timer")

    wake_seed = seed_wait(
        harness,
        label="wake-hint",
        marker=marker,
        wake="system",
    )
    wake = harness.request(
        "POST",
        harness.agent_path("wake", control=True),
        {
            "reason": f"scheduler drill wake {marker}",
            "source": "scheduler-drill",
        },
    )
    write_json(harness.evidence / "wait-wake-hint.json", wake)
    wait_seed_completion(harness, wake_seed, "wake-hint")


def exercise_continuations(harness: CaseHarness, marker: str) -> None:
    objective_marker = f"DRILL-CONTINUATIONS-{marker}"
    completion_marker = f"DRILL-CONTINUATIONS-COMPLETE-{marker}"
    objective = (
        f"{objective_marker}. On the first autonomous WorkItem turn, call ExecCommand "
        f"with cmd `sleep 2; printf DRILL-TASK-{marker}`, yield_time_ms=50, and a "
        "bounded max_output_tokens. Call WaitFor with wake=task_result and the exact "
        "promoted task_id. Do not poll. On task-result rejoin, call GetWorkItem and "
        f"WaitFor with wake=external, resource=drill:continuation:{marker}. On the "
        "external resume, call GetWorkItem, update the two existing todos to completed, "
        f"emit a concise result containing {completion_marker}, and immediately call "
        "CompleteWorkItem for the exact current WorkItem. Do not create another item."
    )
    harness.prompt(
        "continuation-seed",
        "Scheduler drill. Create exactly one WorkItem whose objective is "
        f"{json.dumps(objective)}, plan_status ready, with exactly two todos: "
        "task-rejoin pending and external-resume pending. Do not PickWorkItem, "
        "ExecCommand, WaitFor, UpdateWorkItem, or CompleteWorkItem in this "
        "operator-triggered turn. End after CreateWorkItem succeeds.",
    )
    task_waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_marker,
        expected_scheduling_state="waiting_task",
        label="continuation-task-wait",
    )
    external_waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_marker,
        expected_scheduling_state="waiting_external",
        label="continuation-external-wait",
    )
    require(
        task_waiting["id"] == external_waiting["id"],
        "continuation WorkItem identity changed",
    )
    callback = harness.reset_callback("continuation-callback")
    harness.fire_callback(
        "continuation-external-resume",
        callback["trigger_url"],
        {"drill": marker, "trigger": "continuation"},
    )
    item = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="continuation-completed",
    )
    require(
        item.get("result_brief_id"),
        "continuation WorkItem did not produce a result brief",
    )


def exercise_bound_operator(harness: CaseHarness, marker: str) -> None:
    objective = f"DRILL-BOUND-OPERATOR-{marker}"
    completion = f"DRILL-BOUND-OPERATOR-COMPLETE-{marker}"
    harness.prompt(
        "bound-operator-seed",
        "Scheduler drill. Create exactly one WorkItem with objective "
        f"{objective}, plan_status ready, and one todo named bound-input pending. "
        "Pick it and call WaitFor with wake=operator_input and a concrete reason. "
        "Do not complete it in this turn. When resumed, call GetWorkItem, update the "
        f"todo to completed, emit a concise report containing {completion}, and "
        "immediately call CompleteWorkItem for the exact current WorkItem.",
    )
    waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective,
        expected_scheduling_state="waiting_for_operator",
        label="bound-operator-waiting",
    )
    response = harness.request(
        "POST",
        harness.agent_path("prompt", control=True),
        {
            "text": f"Resume the exact bound WorkItem and follow its objective. {marker}",
            "work_item_id": waiting["id"],
        },
    )
    write_json(harness.evidence / "bound-operator-response.json", response)
    harness.wait_work_item(
        objective_marker=objective,
        expected_state="completed",
        label="bound-operator-completed",
    )


def exercise_interjection(harness: CaseHarness, marker: str) -> None:
    before = harness.state("interjection-before")
    baseline = int(before["agent"]["agent"]["turn_index"])
    first = harness.request(
        "POST",
        harness.agent_path("prompt", control=True),
        {
            "text": "Scheduler drill interjection boundary. Call ExecCommand with "
            f"cmd `sleep 4; printf DRILL-INTERJECTION-{marker}`, yield_time_ms=10000, "
            "and bounded output. After the tool result, answer with "
            f"DRILL-INTERJECTION-DONE-{marker}.",
        },
    )
    write_json(harness.evidence / "interjection-first.json", first)
    wait_for_running_turn(
        harness,
        baseline_turn=baseline,
        label="interjection-tool-start",
    )
    second = harness.request(
        "POST",
        harness.agent_path("prompt", control=True),
        {
            "text": f"Operator interjection {marker}: keep the requested marker and finish.",
        },
    )
    write_json(harness.evidence / "interjection-second.json", second)
    wait_for_turn_after(harness, baseline, "interjection-finished")


def exercise_scenarios(args: argparse.Namespace) -> int:
    paths = DrillPaths.from_root(args.run_dir)
    record = load_record(paths)
    validate_record(paths, record)
    require(container_running(record["resources"]["container"]), "candidate is not running")
    harness = make_harness(
        paths,
        record,
        label=f"exercise-{len(record['phase_history']) + 1}",
        mode=record.get("last_mode") or "shadow",
        env_file=None,
        require_credentials=False,
    )
    attach_running(harness)
    selected = set(args.scenario or record["parameters"]["scenarios"])
    marker = secrets.token_hex(5)
    if "reducer_only_candidates" in selected:
        exercise_reducer_ingress(harness, marker)
    if selected.intersection(
        {
            "exact_wait_resume",
            "settlement",
            "delivery",
        }
    ):
        exercise_wait_triggers(harness, marker)
    if selected.intersection(
        {
            "work_item_autonomous_continuation",
            "exact_task_rejoin",
            "settlement",
            "delivery",
        }
    ):
        exercise_continuations(harness, marker)
    if "explicitly_bound_operator_input" in selected:
        exercise_bound_operator(harness, marker)
    if "operator_interjection" in selected:
        exercise_interjection(harness, marker)
    harness.capture_context("exercise-final")
    append_phase(
        paths,
        record,
        action="exercise",
        status="completed",
        detail={"scenarios": sorted(selected), "marker": marker},
    )
    return 0


def container_running(container: str) -> bool:
    result = run(
        ["docker", "inspect", "--format", "{{.State.Running}}", container],
        check=False,
    )
    return result.returncode == 0 and result.stdout.strip() == "true"


def remove_stopped_container(container: str) -> None:
    if container_running(container):
        raise AssertionError(f"container is already running: {container}")
    run(["docker", "rm", "-f", container], check=False)


def copy_stopped_volume(
    record: dict[str, Any],
    destination: Path,
) -> Path:
    require(
        not container_running(record["resources"]["container"]),
        "stop or kill the candidate before taking a final snapshot",
    )
    destination.mkdir(parents=True, exist_ok=False)
    destination.chmod(0o700)
    run(
        [
            "docker",
            "run",
            "--rm",
            "--volume",
            f"{record['resources']['volume']}:/var/lib/holon:ro",
            "--volume",
            f"{destination}:/snapshot",
            "--entrypoint",
            "bash",
            record["image"]["ref"],
            "-lc",
            "set -euo pipefail; cp -a /var/lib/holon/state /snapshot/state",
        ]
    )
    database = destination / "state" / "runtime.sqlite"
    require(database.is_file(), "runtime.sqlite is missing from the snapshot")
    return database


def query_rows(
    connection: sqlite3.Connection,
    query: str,
    parameters: tuple[Any, ...] = (),
) -> list[dict[str, Any]]:
    return [dict(row) for row in connection.execute(query, parameters).fetchall()]


def table_exists(connection: sqlite3.Connection, table: str) -> bool:
    row = connection.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
        (table,),
    ).fetchone()
    return row is not None


def optional_rows(
    connection: sqlite3.Connection,
    table: str,
    query: str,
) -> list[dict[str, Any]]:
    return query_rows(connection, query) if table_exists(connection, table) else []


def collect_database(database: Path) -> dict[str, Any]:
    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        connection.execute("BEGIN")
        migrations = optional_rows(
            connection,
            "schema_migrations",
            "SELECT version, name, applied_at FROM schema_migrations ORDER BY version",
        )
        schema_revision = max(
            (int(row["version"]) for row in migrations),
            default=int(connection.execute("PRAGMA user_version").fetchone()[0]),
        )
        evidence = {
            "schema_revision": schema_revision,
            "schema_migrations": migrations,
            "protocol_config": optional_rows(
                connection,
                "scheduler_protocol_config",
                "SELECT protocol_mode, config_revision, latest_preflight_revision, "
                "updated_at FROM scheduler_protocol_config",
            ),
            "scenario_authorities": optional_rows(
                connection,
                "scheduler_scenario_authorities",
                "SELECT scenario_class, mode, rollback_target, manifest_revision, "
                "preflight_revision, updated_at FROM scheduler_scenario_authorities "
                "ORDER BY scenario_class",
            ),
            "shadow_comparisons": optional_rows(
                connection,
                "scheduler_shadow_comparisons",
                "SELECT agent_id, scenario_class, boundary, comparison_identity, "
                "comparison_outcome, divergence_code, authority_mode, input_identity, "
                "created_at FROM scheduler_shadow_comparisons ORDER BY created_at",
            ),
            "hard_blockers": optional_rows(
                connection,
                "scheduler_scenario_hard_blockers",
                "SELECT scenario_class, blocker_code, config_revision, trigger_kind, "
                "manifest_revision, preflight_revision, action_json, created_at "
                "FROM scheduler_scenario_hard_blockers "
                "ORDER BY created_at",
            ),
            "work_demands": optional_rows(
                connection,
                "scheduler_work_demands",
                "SELECT agent_id, work_item_id, scheduling_generation, status, "
                "status_reference_id, payload_json FROM scheduler_work_demands "
                "ORDER BY agent_id, work_item_id",
            ),
            "activations": optional_rows(
                connection,
                "scheduler_activations",
                "SELECT agent_id, activation_id, work_item_id, admission_kind, "
                "lifecycle_state, admitted_generation, idempotency_key, payload_json "
                "FROM scheduler_activations ORDER BY activation_id",
            ),
            "settlements": optional_rows(
                connection,
                "scheduler_activation_settlements",
                "SELECT agent_id, settlement_id, activation_id, payload_json "
                "FROM scheduler_activation_settlements ORDER BY settlement_id",
            ),
            "missing_settlements": optional_rows(
                connection,
                "scheduler_missing_settlements",
                "SELECT agent_id, missing_settlement_id, activation_id, payload_json "
                "FROM scheduler_missing_settlements ORDER BY missing_settlement_id",
            ),
            "slots": optional_rows(
                connection,
                "scheduler_agent_slots",
                "SELECT agent_id, slot_kind, activation_id, work_item_id, "
                "admitted_generation FROM scheduler_agent_slots ORDER BY agent_id",
            ),
            "dispatch": optional_rows(
                connection,
                "scheduler_agent_dispatch",
                "SELECT agent_id, dispatch_kind, wait_id, wait_generation, "
                "dispatch_revision FROM scheduler_agent_dispatch ORDER BY agent_id",
            ),
            "wait_generations": optional_rows(
                connection,
                "scheduler_wait_generations",
                "SELECT agent_id, wait_id, generation, owner_work_item_id, "
                "lifecycle_state, trigger_id, trigger_generation, "
                "consuming_activation_id FROM scheduler_wait_generations "
                "ORDER BY wait_id, generation",
            ),
            "protocol_conflicts": optional_rows(
                connection,
                "scheduler_protocol_command_results",
                "SELECT agent_id, command_kind, command_identity, decision, "
                "conflict_kind, conflict_code FROM scheduler_protocol_command_results "
                "WHERE conflict_kind IS NOT NULL ORDER BY created_at",
            ),
            "queue_status": optional_rows(
                connection,
                "queue_entries",
                "SELECT status, COUNT(*) AS count FROM queue_entries GROUP BY status "
                "ORDER BY status",
            ),
            "incomplete_turns": optional_rows(
                connection,
                "turn_records",
                "SELECT turn_id, agent_id, current_work_item_id, trigger_message_id, "
                "terminal_kind, created_at FROM turn_records "
                "WHERE completed_at IS NULL OR terminal_kind IS NULL ORDER BY created_at",
            ),
            "delivery_summaries": optional_rows(
                connection,
                "delivery_summaries",
                "SELECT evidence_id, agent_id, turn_id, message_id, task_id, "
                "work_item_id, kind, created_at FROM delivery_summaries "
                "ORDER BY created_at",
            ),
            "briefs": optional_rows(
                connection,
                "briefs",
                "SELECT evidence_id, agent_id, turn_id, message_id, task_id, "
                "work_item_id, kind, preview, payload_json FROM briefs "
                "ORDER BY created_at",
            ),
            "operator_deliveries": optional_rows(
                connection,
                "operator_delivery_records",
                "SELECT delivery_intent_id, agent_id, created_at, payload_json "
                "FROM operator_delivery_records ORDER BY created_at",
            ),
            "outbox": optional_rows(
                connection,
                "runtime_index_outbox",
                "SELECT agent_id, operation, COUNT(*) AS count, "
                "MIN(change_seq) AS first_seq, MAX(change_seq) AS last_seq "
                "FROM runtime_index_outbox GROUP BY agent_id, operation "
                "ORDER BY agent_id, operation",
            ),
        }
        connection.execute("COMMIT")
        return evidence
    except Exception:
        connection.execute("ROLLBACK")
        raise
    finally:
        connection.close()


def parse_json_columns(evidence: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    for collection in (
        "hard_blockers",
        "work_demands",
        "activations",
        "settlements",
        "missing_settlements",
        "briefs",
        "operator_deliveries",
    ):
        for index, row in enumerate(evidence[collection]):
            for key, value in list(row.items()):
                if not key.endswith("_json"):
                    continue
                try:
                    row[f"{key}_value"] = json.loads(value)
                except (TypeError, json.JSONDecodeError):
                    failures.append(f"{collection}[{index}].{key}")
    return failures


def current_hard_blockers(evidence: dict[str, Any]) -> list[dict[str, Any]]:
    if not evidence["protocol_config"]:
        return []
    config_revision = evidence["protocol_config"][0]["config_revision"]
    authorities = {
        row["scenario_class"]: row for row in evidence["scenario_authorities"]
    }
    return [
        row
        for row in evidence["hard_blockers"]
        if row["config_revision"] == config_revision
        and row["scenario_class"] in authorities
        and row["manifest_revision"]
        == authorities[row["scenario_class"]]["manifest_revision"]
        and row["preflight_revision"]
        == authorities[row["scenario_class"]]["preflight_revision"]
    ]


def evidence_summary(evidence: dict[str, Any]) -> dict[str, Any]:
    json_failures = parse_json_columns(evidence)
    counts = {scenario: 0 for scenario in PRODUCTION_SCENARIOS}
    divergences: list[dict[str, Any]] = []
    for row in evidence["shadow_comparisons"]:
        scenario = row["scenario_class"]
        if scenario in counts:
            counts[scenario] += 1
        if row["comparison_outcome"] != "matched":
            divergences.append(row)
    active_activations = [
        row
        for row in evidence["activations"]
        if row["lifecycle_state"] in {"admitted", "running", "settlement_missing"}
    ]
    occupied_slots = [row for row in evidence["slots"] if row["slot_kind"] != "idle"]
    active_waits = [
        row
        for row in evidence["wait_generations"]
        if row["lifecycle_state"] in {"triggered", "consumed"}
    ]
    needs_settlement = [
        row for row in evidence["work_demands"] if row["status"] == "needs_settlement"
    ]
    settlement_by_activation = {
        row["activation_id"]: row for row in evidence["settlements"]
    }
    missing_by_activation = {
        row["activation_id"]: row for row in evidence["missing_settlements"]
    }
    settlement_inconsistencies = []
    for activation in evidence["activations"]:
        activation_id = activation["activation_id"]
        has_settlement = activation_id in settlement_by_activation
        has_missing = activation_id in missing_by_activation
        expected_settlement = activation["lifecycle_state"] == "settled"
        expected_missing = activation["lifecycle_state"] == "settlement_missing"
        if (
            has_settlement and has_missing
            or expected_settlement != has_settlement
            or expected_missing != has_missing
        ):
            settlement_inconsistencies.append(
                {
                    "activation_id": activation_id,
                    "lifecycle_state": activation["lifecycle_state"],
                    "has_settlement": has_settlement,
                    "has_missing_settlement": has_missing,
                }
            )
    brief_ids = {row["evidence_id"] for row in evidence["briefs"]}
    delivery_inconsistencies = []
    for settlement in evidence["settlements"]:
        payload = settlement.get("payload_json_value") or {}
        brief_id = payload.get("operator_delivery")
        if brief_id is not None and brief_id not in brief_ids:
            delivery_inconsistencies.append(
                {
                    "activation_id": settlement["activation_id"],
                    "operator_delivery": brief_id,
                    "reason": "brief_missing",
                }
            )
    blockers = current_hard_blockers(evidence)
    queue_tail = [
        row for row in evidence["queue_status"] if row["status"] != "processed"
    ]
    checks = {
        "all_scenarios_observed": all(counts.values()),
        "json_columns_valid": not json_failures,
        "no_divergence": not divergences,
        "no_current_hard_blocker": not blockers,
        "no_active_activation": not active_activations,
        "no_occupied_slot": not occupied_slots,
        "no_active_wait": not active_waits,
        "no_needs_settlement_demand": not needs_settlement,
        "settlement_state_consistent": not settlement_inconsistencies,
        "delivery_binding_consistent": not delivery_inconsistencies,
        "no_protocol_conflict": not evidence["protocol_conflicts"],
        "no_incomplete_turn": not evidence["incomplete_turns"],
        "no_queue_tail": not queue_tail,
    }
    return {
        "status": "go" if all(checks.values()) else "no-go",
        "checks": checks,
        "scenario_counts": counts,
        "divergences": divergences,
        "json_failures": json_failures,
        "current_hard_blockers": blockers,
        "active_activations": active_activations,
        "occupied_slots": occupied_slots,
        "active_waits": active_waits,
        "needs_settlement": needs_settlement,
        "settlement_inconsistencies": settlement_inconsistencies,
        "delivery_inconsistencies": delivery_inconsistencies,
        "queue_tail": queue_tail,
    }


def render_report(
    record: dict[str, Any],
    snapshot_label: str,
    evidence: dict[str, Any],
    summary: dict[str, Any],
) -> str:
    lines = [
        f"# Scheduler drill report: {record['drill_run_id']}",
        "",
        f"- Snapshot: `{snapshot_label}`",
        f"- Collected at: `{utc_now()}`",
        f"- Requested mode: `{record.get('last_mode') or 'not-started'}`",
        f"- Schema revision: `{evidence['schema_revision']}`",
        f"- Decision: **{summary['status'].upper()}**",
        "",
        "## Scenario coverage",
        "",
        "| Scenario | Comparisons |",
        "|---|---:|",
    ]
    lines.extend(
        f"| `{scenario}` | {summary['scenario_counts'][scenario]} |"
        for scenario in PRODUCTION_SCENARIOS
    )
    lines.extend(
        [
            "",
            "## Go / No-Go checks",
            "",
            "| Check | Result |",
            "|---|---|",
        ]
    )
    lines.extend(
        f"| `{name}` | {'PASS' if passed else 'FAIL'} |"
        for name, passed in summary["checks"].items()
    )
    lines.extend(
        [
            "",
            "## Tail state",
            "",
            f"- Divergences: {len(summary['divergences'])}",
            f"- Current hard blockers: {len(summary['current_hard_blockers'])}",
            f"- Historical hard blockers: {len(evidence['hard_blockers'])}",
            f"- Active activations: {len(summary['active_activations'])}",
            f"- Occupied slots: {len(summary['occupied_slots'])}",
            f"- Active waits: {len(summary['active_waits'])}",
            f"- Needs-settlement demands: {len(summary['needs_settlement'])}",
            f"- Settlement inconsistencies: "
            f"{len(summary['settlement_inconsistencies'])}",
            f"- Delivery binding inconsistencies: "
            f"{len(summary['delivery_inconsistencies'])}",
            f"- Protocol conflicts: {len(evidence['protocol_conflicts'])}",
            f"- Incomplete turns: {len(evidence['incomplete_turns'])}",
            f"- Queue tail groups: {len(summary['queue_tail'])}",
            "",
            "This report is host-side evidence only. It is not imported into the runtime.",
            "",
        ]
    )
    return "\n".join(lines)


def prepare(args: argparse.Namespace) -> int:
    require(shutil.which("docker") is not None, "docker is required")
    run_id = args.run_id or default_run_id()
    require(RUN_ID_PATTERN.match(run_id) is not None, "invalid drill run id")
    require(args.iterations > 0, "--iterations must be positive")
    require(args.concurrency > 0, "--concurrency must be positive")
    require(
        0.0 <= args.duplicate_ratio <= 1.0,
        "--duplicate-ratio must be between 0 and 1",
    )
    require(
        0.0 <= args.stale_ratio <= 1.0,
        "--stale-ratio must be between 0 and 1",
    )
    paths = DrillPaths.from_root(args.run_dir or default_run_root(run_id))
    require(not paths.root.exists(), f"run directory already exists: {paths.root}")
    git_status = run(["git", "status", "--porcelain"]).stdout.strip()
    require(not git_status, "prepare requires a clean Git worktree")
    if not args.skip_build:
        run(["docker", "build", "--tag", args.image, str(ROOT)], capture=False)
    identity = image_identity(args.image)
    require(identity["id"] is not None, f"Docker image is unavailable: {args.image}")
    paths.phases.mkdir(parents=True)
    paths.snapshots.mkdir(parents=True)
    suffix = re.sub(r"[^a-z0-9-]", "-", run_id.lower())[-48:]
    resources = {
        "volume": f"holon-drill-{suffix}",
        "network": f"holon-drill-{suffix}",
        "container": f"holon-drill-{suffix}",
        "workspace_parent": str(paths.workspace),
    }
    token = secrets.token_urlsafe(32)
    write_control_token(run_id, token)
    record = {
        "schema_version": RUN_SCHEMA_VERSION,
        "drill_run_id": run_id,
        "run_dir": str(paths.root),
        "created_at": utc_now(),
        "updated_at": utc_now(),
        "git_sha": run(["git", "rev-parse", "HEAD"]).stdout.strip(),
        "image": identity,
        "models": {
            "primary": args.primary_model,
            "fallbacks": list(args.fallback_model),
            "provider_fallback_disabled": False,
        },
        "credential_env_names": list(REQUIRED_CREDENTIAL_ENVS),
        "env_file_used": bool(
            args.env_file or os.environ.get("HOLON_DRILL_ENV_FILE", "").strip()
        ),
        "resources": resources,
        "parameters": {
            "scenarios": list(args.scenario or PRODUCTION_SCENARIOS),
            "iterations": args.iterations,
            "concurrency": args.concurrency,
            "duplicate_ratio": args.duplicate_ratio,
            "stale_ratio": args.stale_ratio,
            "timeout_seconds": args.timeout_seconds,
        },
        "phase_history": [],
        "last_mode": None,
        "last_snapshot": None,
        "schema_revision": None,
    }
    save_record(paths, record)
    harness = make_harness(
        paths,
        record,
        label="prepare",
        mode="shadow",
        env_file=credential_env_file(args.env_file),
        require_credentials=False,
    )
    harness.initialize_workspace()
    harness.docker("volume", "create", harness.volume)
    if harness.docker("network", "inspect", harness.network, check=False).returncode != 0:
        harness.docker("network", "create", harness.network)
    append_phase(paths, record, action="prepare", status="completed")
    print(paths.root)
    return 0


def start_candidate(args: argparse.Namespace) -> int:
    paths = DrillPaths.from_root(args.run_dir)
    record = load_record(paths)
    validate_record(paths, record)
    remove_stopped_container(record["resources"]["container"])
    env_file = credential_env_file(args.env_file)
    harness = make_harness(
        paths,
        record,
        label=f"start-{args.mode}-{len(record['phase_history']) + 1}",
        mode=args.mode,
        env_file=env_file,
    )
    harness.start()
    record["last_mode"] = args.mode
    append_phase(
        paths,
        record,
        action="start",
        status="completed",
        detail={"mode": args.mode, "agent_id": harness.agent_id},
    )
    print(harness.base_url)
    return 0


def stop_candidate(args: argparse.Namespace) -> int:
    paths = DrillPaths.from_root(args.run_dir)
    record = load_record(paths)
    validate_record(paths, record)
    require(container_running(record["resources"]["container"]), "candidate is not running")
    harness = make_harness(
        paths,
        record,
        label=f"stop-{record.get('last_mode')}-{len(record['phase_history']) + 1}",
        mode=record.get("last_mode") or "shadow",
        env_file=None,
        require_credentials=False,
    )
    attach_running(harness)
    harness.stop()
    append_phase(paths, record, action="stop", status="completed")
    return 0


def kill_candidate(args: argparse.Namespace) -> int:
    paths = DrillPaths.from_root(args.run_dir)
    record = load_record(paths)
    validate_record(paths, record)
    container = record["resources"]["container"]
    require(container_running(container), "candidate is not running")
    phase = paths.phases / f"kill-{len(record['phase_history']) + 1}"
    phase.mkdir(parents=True, exist_ok=True)
    logs = run(["docker", "logs", container], check=False)
    (phase / "container.log").write_text(logs.stdout + logs.stderr)
    run(["docker", "kill", "--signal", "KILL", container])
    run(["docker", "rm", "-f", container], check=False)
    append_phase(paths, record, action="kill", status="completed")
    return 0


def collect(args: argparse.Namespace) -> int:
    paths = DrillPaths.from_root(args.run_dir)
    record = load_record(paths)
    validate_record(paths, record)
    label = args.label or datetime.now(timezone.utc).strftime("snapshot-%Y%m%d-%H%M%S")
    destination = paths.snapshots / label
    database = copy_stopped_volume(record, destination)
    evidence = collect_database(database)
    shutil.rmtree(destination / "state")
    summary = evidence_summary(evidence)
    payload = {
        "schema_version": EVIDENCE_SCHEMA_VERSION,
        "drill_run_id": record["drill_run_id"],
        "snapshot": label,
        "collected_at": utc_now(),
        "evidence": evidence,
        "summary": summary,
    }
    write_json(destination / "evidence.json", payload)
    (destination / "report.md").write_text(
        render_report(record, label, evidence, summary)
    )
    if record["schema_revision"] is None:
        record["schema_revision"] = evidence["schema_revision"]
    require(
        record["schema_revision"] == evidence["schema_revision"],
        "schema revision changed during the drill",
    )
    record["last_snapshot"] = label
    append_phase(
        paths,
        record,
        action="collect",
        status="completed",
        detail={"snapshot": label, "decision": summary["status"]},
    )
    scan = secret_scan(destination, [])
    require(scan["status"] == "pass", "snapshot evidence contains a secret")
    print(destination / "report.md")
    return 0 if summary["status"] == "go" else 1


def status(args: argparse.Namespace) -> int:
    paths = DrillPaths.from_root(args.run_dir)
    record = load_record(paths)
    result = {
        "drill_run_id": record["drill_run_id"],
        "run_dir": str(paths.root),
        "last_mode": record.get("last_mode"),
        "last_snapshot": record.get("last_snapshot"),
        "schema_revision": record.get("schema_revision"),
        "container_running": container_running(record["resources"]["container"]),
        "resources": record["resources"],
        "models": record["models"],
        "parameters": record["parameters"],
        "last_phase": (record.get("phase_history") or [None])[-1],
    }
    print(json.dumps(result, indent=2, ensure_ascii=False))
    return 0


def cleanup(args: argparse.Namespace) -> int:
    paths = DrillPaths.from_root(args.run_dir)
    record = load_record(paths)
    container = record["resources"]["container"]
    run(["docker", "rm", "-f", container], check=False)
    run(["docker", "network", "rm", record["resources"]["network"]], check=False)
    run(["docker", "volume", "rm", record["resources"]["volume"]], check=False)
    delete_control_token(record["drill_run_id"])
    append_phase(paths, record, action="cleanup", status="completed")
    return 0


def preflight(args: argparse.Namespace) -> int:
    require(shutil.which("docker") is not None, "docker is required")
    env_file = credential_env_file(args.env_file)
    names = credential_env_names(env_file)
    if not args.skip_build:
        run(["docker", "build", "--tag", args.image, str(ROOT)], capture=False)
    evidence_root = (
        Path(args.evidence_dir).resolve()
        if args.evidence_dir
        else ROOT
        / "target"
        / "scheduler-drill-preflight"
        / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    )
    models = [args.primary_model, *args.fallback_model]
    results: list[dict[str, Any]] = []
    secrets_to_scan: list[str] = []
    if env_file is not None:
        secrets_to_scan.extend(parse_env_file(env_file).values())
    else:
        secrets_to_scan.extend(os.environ[name] for name in names)
    for index, model in enumerate(models):
        label = f"model-{index + 1}-{model.split('/', 1)[0].replace('@', '-')}"
        harness = CaseHarness(
            case_id=label,
            image=args.image,
            model=model,
            credential_envs=names,
            env_file=env_file,
            runtime_env={"HOLON_SCHEDULER": "legacy"},
            evidence_root=evidence_root,
            timeout_seconds=args.timeout_seconds,
            keep=False,
        )
        try:
            harness.initialize_workspace()
            harness.start()
            provider = model.split("/", 1)[0].split("@", 1)[0]
            marker = f"MODEL-PREFLIGHT-{secrets.token_hex(6)}"
            baseline, _ = harness.prompt(
                "tool-round",
                "Scheduler drill model preflight. Call AgentGet, ListModelProviders, "
                f"and ListProviderModels for provider {provider} with limit 5. "
                f"Then answer with the literal marker {marker}.",
            )
            harness.assert_tools(
                "tool-round",
                baseline,
                ["AgentGet", "ListModelProviders", "ListProviderModels"],
            )
            events = harness.events("provider")
            provider_events = [
                event
                for event in events
                if event["type"] == "provider_round_completed"
                and int(event["payload"].get("turn_index", 0)) > baseline
            ]
            require(provider_events, "provider_round_completed event is missing")
            winning = (
                provider_events[-1]["payload"]
                .get("provider_attempt_timeline", {})
                .get("winning_model_ref")
            )
            require(
                normalize_model_route(str(winning)) == normalize_model_route(model),
                f"winning model mismatch: {winning}",
            )
            results.append({"model": model, "status": "pass"})
        except Exception as error:
            results.append(
                {"model": model, "status": "fail", "error": f"{type(error).__name__}: {error}"}
            )
        finally:
            harness.cleanup()
    write_json(
        evidence_root / "summary.json",
        {
            "schema_version": EVIDENCE_SCHEMA_VERSION,
            "created_at": utc_now(),
            "image": image_identity(args.image),
            "models": results,
        },
    )
    scan = secret_scan(evidence_root, secrets_to_scan)
    require(scan["status"] == "pass", "preflight evidence contains a secret")
    print(evidence_root)
    return 0 if all(result["status"] == "pass" for result in results) else 1


def add_model_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--image", default="holon:scheduler-drill")
    parser.add_argument("--primary-model", default=DEFAULT_PRIMARY_MODEL)
    parser.add_argument(
        "--fallback-model",
        action="append",
        default=None,
    )
    parser.add_argument("--env-file")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--timeout-seconds", type=int, default=900)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    preflight_parser = subparsers.add_parser("preflight")
    add_model_arguments(preflight_parser)
    preflight_parser.add_argument("--evidence-dir")
    preflight_parser.set_defaults(handler=preflight)

    prepare_parser = subparsers.add_parser("prepare")
    add_model_arguments(prepare_parser)
    prepare_parser.add_argument("--run-id")
    prepare_parser.add_argument("--run-dir", type=Path)
    prepare_parser.add_argument("--scenario", action="append", choices=PRODUCTION_SCENARIOS)
    prepare_parser.add_argument("--iterations", type=int, default=1)
    prepare_parser.add_argument("--concurrency", type=int, default=1)
    prepare_parser.add_argument("--duplicate-ratio", type=float, default=0.0)
    prepare_parser.add_argument("--stale-ratio", type=float, default=0.0)
    prepare_parser.set_defaults(handler=prepare)

    for command, handler in (
        ("start", start_candidate),
        ("exercise", exercise_scenarios),
        ("stop", stop_candidate),
        ("kill", kill_candidate),
        ("collect", collect),
        ("status", status),
        ("cleanup", cleanup),
    ):
        command_parser = subparsers.add_parser(command)
        command_parser.add_argument("--run-dir", type=Path, required=True)
        if command == "start":
            command_parser.add_argument(
                "--mode",
                choices=["legacy", "shadow", "authoritative"],
                required=True,
            )
            command_parser.add_argument("--env-file")
        if command == "exercise":
            command_parser.add_argument(
                "--scenario",
                action="append",
                choices=PRODUCTION_SCENARIOS,
            )
        if command == "collect":
            command_parser.add_argument("--label")
        command_parser.set_defaults(handler=handler)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if hasattr(args, "fallback_model") and args.fallback_model is None:
        args.fallback_model = list(DEFAULT_FALLBACK_MODELS)
    return int(args.handler(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2)
