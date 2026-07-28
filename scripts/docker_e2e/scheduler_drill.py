#!/usr/bin/env python3
"""Resumable host-side scheduler shadow and cutover drill."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import secrets
import shutil
import sqlite3
import stat
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from datetime import datetime, timezone
from math import ceil
from pathlib import Path
from typing import Any, Callable

from .runner import (
    CaseHarness,
    DockerCircuitBreakerOpen,
    image_identity,
    normalize_model_route,
    parse_env_file,
    require,
    run,
    secret_scan,
    utc_now,
    write_json,
)
#
# Prefix prepended to every drill operator prompt so the requested fixture
# actions are clearly scoped to the current operator turn.
DRILL_PREFIX = (
    "Scheduler drill fixture request for the current operator turn.\n"
    "Perform the numbered steps in order using the named tools. Keep the "
    "response concise and do not perform unrelated actions.\n\n"
)


ROOT = Path(__file__).resolve().parents[2]
RUN_SCHEMA_VERSION = 1
EVIDENCE_SCHEMA_VERSION = 1
DEFAULT_PRIMARY_MODEL = "volcengine@plan/glm-5.2"
DEFAULT_FALLBACK_MODELS = ["dashscope@token-plan/qwen3.8-max-preview"]
DEFAULT_NATIVE_DOCKER_HOST = "unix:///var/run/docker.sock"
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
EXACT_WAIT_RESUME_TRIGGERS = (
    "callback",
    "webhook",
    "channel",
    "timer",
    "system",
    "operator_wake",
)
RESTART_CHECKPOINTS = (
    "ingress_queue_admission",
    "queue_claim_activation_admission",
    "wait_trigger_consume_admission",
    "turn_terminal_settlement",
    "settlement_delivery",
    "post_commit_notification",
    "targeted_yield_return",
    "legacy_adoption_atomicity",
    "preclaim_hard_blocker_fallback",
    "authority_rollback",
)
RESTART_CHECKPOINT_CUT_KINDS = {
    "ingress_queue_admission": "atomic_rollback",
    "queue_claim_activation_admission": "atomic_rollback",
    "wait_trigger_consume_admission": "atomic_rollback",
    "turn_terminal_settlement": "durable_recovery",
    "settlement_delivery": "durable_recovery",
    "post_commit_notification": "post_commit_recovery",
    "targeted_yield_return": "durable_recovery",
    "legacy_adoption_atomicity": "atomic_rollback",
    "preclaim_hard_blocker_fallback": "durable_recovery",
    "authority_rollback": "atomic_rollback",
}
RUN_ID_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-]{5,63}$")
FAULT_SCENARIOS = {
    "exact_wait_resume": "stale",
    "settlement": "stale",
    "delivery": "stale",
    "reducer_only_candidates": "out_of_order",
    "explicitly_bound_operator_input": "wrong_fence",
}


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


@dataclass(frozen=True)
class StressOperation:
    sequence: int
    iteration: int
    worker: int
    scenario: str
    marker: str
    duplicate: bool
    fault: str | None

    def as_dict(self) -> dict[str, Any]:
        return {
            "sequence": self.sequence,
            "iteration": self.iteration,
            "worker": self.worker,
            "scenario": self.scenario,
            "marker": self.marker,
            "duplicate": self.duplicate,
            "fault": self.fault,
        }


def build_stress_plan(
    *,
    scenarios: list[str],
    iterations: int,
    concurrency: int,
    duplicate_ratio: float,
    stale_ratio: float,
    seed: str,
) -> list[StressOperation]:
    require(iterations > 0, "stress iterations must be positive")
    require(concurrency > 0, "stress concurrency must be positive")
    require(bool(scenarios), "stress plan requires at least one scenario")
    require(
        0.0 <= duplicate_ratio <= 1.0,
        "stress duplicate ratio must be between 0 and 1",
    )
    require(
        0.0 <= stale_ratio <= 1.0,
        "stress stale ratio must be between 0 and 1",
    )
    unknown = sorted(set(scenarios) - set(PRODUCTION_SCENARIOS))
    require(not unknown, f"unknown stress scenarios: {', '.join(unknown)}")
    blueprints = [
        (iteration, scenario)
        for iteration in range(1, iterations + 1)
        for scenario in scenarios
    ]
    fault_candidates: dict[str, list[int]] = {
        fault: [] for fault in dict.fromkeys(FAULT_SCENARIOS.values())
    }
    for sequence, (_, scenario) in enumerate(blueprints):
        if scenario in FAULT_SCENARIOS:
            fault_candidates[FAULT_SCENARIOS[scenario]].append(sequence)
    for fault, sequences in fault_candidates.items():
        sequences.sort(
            key=lambda sequence: hashlib.sha256(
                f"{seed}:{fault}:{sequence}".encode()
            ).digest()
        )
    available_fault_types = sum(bool(sequences) for sequences in fault_candidates.values())
    fault_target = min(
        sum(len(sequences) for sequences in fault_candidates.values()),
        max(
            available_fault_types if stale_ratio > 0 else 0,
            ceil(
                sum(len(sequences) for sequences in fault_candidates.values())
                * stale_ratio
            ),
        ),
    )
    selected_faults: dict[int, str] = {}
    fault_order = list(fault_candidates)
    while len(selected_faults) < fault_target:
        advanced = False
        for fault in fault_order:
            if fault_candidates[fault]:
                selected_faults[fault_candidates[fault].pop(0)] = fault
                advanced = True
                if len(selected_faults) == fault_target:
                    break
        if not advanced:
            break
    duplicate_candidates = [
        sequence
        for sequence, (_, scenario) in enumerate(blueprints)
        if scenario in {"exact_wait_resume", "settlement", "delivery"}
        and sequence not in selected_faults
    ]
    duplicate_eligible_count = sum(
        scenario in {"exact_wait_resume", "settlement", "delivery"}
        for _, scenario in blueprints
    )
    duplicate_target = min(
        duplicate_eligible_count,
        ceil(duplicate_eligible_count * duplicate_ratio),
    )
    require(
        len(duplicate_candidates) >= duplicate_target,
        "stress plan requires independent operations for duplicate and fault "
        "injections; increase iterations or reduce duplicate/stale ratios",
    )
    duplicates = set(
        sorted(
            duplicate_candidates,
            key=lambda sequence: hashlib.sha256(
                f"{seed}:duplicate:{sequence}".encode()
            ).digest(),
        )[:duplicate_target]
    )
    plan: list[StressOperation] = []
    for sequence, (iteration, scenario) in enumerate(blueprints):
        digest = hashlib.sha256(
            f"{seed}:{scenario}:{iteration}".encode()
        ).hexdigest()
        plan.append(
            StressOperation(
                sequence=sequence,
                iteration=iteration,
                worker=sequence % concurrency,
                scenario=scenario,
                marker=digest[:10],
                duplicate=sequence in duplicates,
                fault=selected_faults.get(sequence),
            )
        )
    return plan


def execute_stress_plan(
    plan: list[StressOperation],
    *,
    concurrency: int,
    run_operation: Callable[[StressOperation], dict[str, Any] | None],
) -> list[dict[str, Any]]:
    require(concurrency > 0, "stress concurrency must be positive")
    worker_plans: dict[int, list[StressOperation]] = {
        worker: [] for worker in range(concurrency)
    }
    for operation in plan:
        worker_plans[operation.worker].append(operation)

    infrastructure_abort = threading.Event()

    def run_worker(operations: list[StressOperation]) -> list[dict[str, Any]]:
        results = []
        for operation in operations:
            if infrastructure_abort.is_set():
                results.append(
                    {
                        **operation.as_dict(),
                        "status": "aborted",
                        "duration_seconds": 0.0,
                        "error": "infrastructure circuit breaker opened",
                    }
                )
                continue
            started = time.monotonic()
            try:
                detail = run_operation(operation) or {}
                result = {
                    **operation.as_dict(),
                    "status": "completed",
                    "duration_seconds": round(time.monotonic() - started, 3),
                    "detail": detail,
                }
            except Exception as error:
                if isinstance(error, DockerCircuitBreakerOpen):
                    infrastructure_abort.set()
                result = {
                    **operation.as_dict(),
                    "status": "failed",
                    "duration_seconds": round(time.monotonic() - started, 3),
                    "error": f"{type(error).__name__}: {error}",
                }
            results.append(result)
        return results

    results: list[dict[str, Any]] = []
    with ThreadPoolExecutor(max_workers=concurrency) as executor:
        futures = [
            executor.submit(run_worker, operations)
            for operations in worker_plans.values()
            if operations
        ]
        for future in as_completed(futures):
            results.extend(future.result())
    return sorted(results, key=lambda result: result["sequence"])


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


def docker_engine_identity() -> dict[str, Any]:
    docker_host = os.environ.get("DOCKER_HOST", "").strip()
    require(
        docker_host == DEFAULT_NATIVE_DOCKER_HOST,
        "scheduler drill requires native Docker Engine at "
        f"{DEFAULT_NATIVE_DOCKER_HOST}; got {docker_host or 'default context'}",
    )
    result = run(
        [
            "docker",
            "info",
            "--format",
            '{"server_version":{{json .ServerVersion}},'
            '"driver":{{json .Driver}},'
            '"docker_root_dir":{{json .DockerRootDir}},'
            '"operating_system":{{json .OperatingSystem}}}',
        ],
        timeout=15,
    )
    identity = json.loads(result.stdout)
    identity["docker_host"] = docker_host
    require(
        identity.get("docker_root_dir") == "/var/lib/docker",
        "scheduler drill refused a non-native Docker data root: "
        f"{identity.get('docker_root_dir')}",
    )
    return identity


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
    require(
        docker_engine_identity() == record["docker_engine"],
        "Docker Engine identity changed since prepare",
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
            "HOLON_SCHEDULER_ACCEPTANCE_FIXTURES": "true",
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
    expected_scheduling_state: str = "waiting_external",
) -> dict[str, Any]:
    objective = f"DRILL-WAIT-{label}-{marker}"
    completion = f"DRILL-WAIT-COMPLETE-{label}-{marker}"
    resource_argument = (
        f", resource={json.dumps(resource)}"
        if resource is not None
        else ""
    )
    harness.prompt(
        f"{label}-create",
        DRILL_PREFIX
        + "1. Call CreateWorkItem with objective "
        f"{json.dumps(objective)}, plan_status needs_input, and one todo named resume pending.\n"
        "2. STOP immediately after CreateWorkItem succeeds. Do not call any other tool.",
    )
    created = harness.wait_work_item(
        objective_marker=objective,
        expected_state="open",
        label=f"{label}-created",
    )
    baseline, _ = harness.prompt(
        f"{label}-seed",
        DRILL_PREFIX
        + f"This prompt is explicitly bound to WorkItem {created['id']}.\n"
        "1. Call UpdateWorkItem for this exact WorkItem with plan_status ready and preserve "
        "the existing pending todo.\n"
        f"2. Call WaitFor with wake={wake}{resource_argument} and a concrete reason. "
        "The wait trigger has NOT fired yet. You MUST call WaitFor even if you believe "
        "the trigger has already fired. Do NOT skip WaitFor.\n"
        "3. STOP. Do not complete the WorkItem in this turn.\n"
        "Do NOT call PickWorkItem, CreateWorkItem, or CompleteWorkItem in this turn. "
        "The WorkItem MUST remain open and waiting after this turn.\n"
        "When this WorkItem resumes later: call GetWorkItem, update the todo to "
        f"completed, emit a report containing {completion}, and call CompleteWorkItem.",
        work_item_id=created["id"],
    )
    # Detect premature completion from a spurious queued message.
    early_items = harness.work_items(f"{label}-early-check")
    early_matches = [
        item for item in early_items if objective in item.get("objective", "")
    ]
    if early_matches and early_matches[0].get("state") == "completed":
        raise AssertionError(
            f"WorkItem {objective} completed before {label} wait was observed; "
            "a spurious queued message likely woke the agent."
        )
    item = harness.wait_work_item_scheduling_state(
        objective_marker=objective,
        expected_scheduling_state=expected_scheduling_state,
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


def exercise_reducer_ingress(
    harness: CaseHarness,
    marker: str,
    *,
    duplicate: bool = False,
    out_of_order: bool = False,
) -> dict[str, Any]:
    webhook_body = {"drill": marker, "surface": "webhook"}
    webhook = harness.request(
        "POST",
        f"/api/webhooks/generic/{harness.agent_id}",
        webhook_body,
    )
    write_json(harness.evidence / "reducer-webhook.json", webhook)
    if duplicate:
        write_json(
            harness.evidence / "reducer-webhook-duplicate.json",
            harness.request(
                "POST",
                f"/api/webhooks/generic/{harness.agent_id}",
                webhook_body,
            ),
        )
    channel_body = {
        "kind": "channel_event",
        "json": {"drill": marker, "surface": "channel"},
        "origin": {
            "kind": "channel",
            "channel_id": f"drill-{marker}",
            "sender_id": "scheduler-drill",
        },
    }
    channel = harness.request(
        "POST",
        harness.agent_path("enqueue"),
        channel_body,
    )
    write_json(harness.evidence / "reducer-channel.json", channel)
    if duplicate:
        write_json(
            harness.evidence / "reducer-channel-duplicate.json",
            harness.request(
                "POST",
                harness.agent_path("enqueue"),
                channel_body,
            ),
        )
    if out_of_order:
        ordered_responses = []
        for sequence in (2, 1):
            response = harness.request(
                "POST",
                harness.agent_path("enqueue"),
                {
                    "kind": "channel_event",
                    "json": {
                        "drill": marker,
                        "surface": "out_of_order",
                        "sequence": sequence,
                    },
                    "origin": {
                        "kind": "channel",
                        "channel_id": f"drill-order-{marker}",
                        "sender_id": "scheduler-drill",
                    },
                },
            )
            ordered_responses.append(response)
            write_json(
                harness.evidence / f"reducer-out-of-order-{sequence}.json",
                response,
            )
    harness.wait_queue_drained()
    out_of_order_ids = []
    if out_of_order:
        out_of_order_ids = [response["message_id"] for response in ordered_responses]
        if harness.claim_checkpoint("reducer-out-of-order-db"):
            snapshot = harness.runtime_db_snapshot("reducer-out-of-order")
            queue_rows = [
                row
                for row in snapshot["queue_entries"]
                if row["message_id"] in set(out_of_order_ids)
            ]
            require(
                len(queue_rows) == 2
                and all(row["status"] == "processed" for row in queue_rows),
                f"out-of-order ingress was not fully processed: {queue_rows}",
            )
            queue_by_id = {row["message_id"]: row for row in queue_rows}
            require(
                queue_by_id[out_of_order_ids[0]]["created_at"]
                <= queue_by_id[out_of_order_ids[1]]["created_at"],
                "out-of-order ingress persistence order changed",
            )
    return {
        "duplicate_requests": 4 if duplicate else 0,
        "out_of_order_requests": 2 if out_of_order else 0,
        "out_of_order_message_ids": out_of_order_ids,
    }


def wait_for_rearmed_work_item(
    harness: CaseHarness,
    *,
    objective: str,
    previous_revision: int,
    label: str,
) -> dict[str, Any]:
    deadline = time.monotonic() + harness.timeout_seconds
    matches: list[dict[str, Any]] = []
    while time.monotonic() < deadline:
        items = harness.request("GET", harness.agent_path("work-items?limit=50"))
        matches = [
            item for item in items if objective in item.get("objective", "")
        ]
        if matches and matches[0].get("state") == "completed":
            write_json(harness.evidence / f"{label}-premature.json", items)
            raise AssertionError(
                "duplicate trigger consumed the rearmed wait generation"
            )
        if (
            len(matches) == 1
            and matches[0].get("scheduling_state") == "waiting_external"
            and int(matches[0].get("revision", 0)) > previous_revision
        ):
            write_json(harness.evidence / f"{label}.json", items)
            harness.wait_agent_asleep()
            return matches[0]
        time.sleep(0.5)
    write_json(harness.evidence / f"{label}-timeout.json", matches)
    raise TimeoutError("timed out waiting for duplicate-trigger wait rearm")


def exercise_wait_rearm_race(
    harness: CaseHarness,
    marker: str,
) -> dict[str, Any]:
    objective = f"DRILL-WAIT-REARM-{marker}"
    completion = f"DRILL-WAIT-REARM-COMPLETE-{marker}"
    callback = harness.reset_callback("wait-rearm-callback")
    barrier = harness.request(
        "POST",
        harness.agent_path("tasks", control=True),
        {
            "summary": f"scheduler drill wait-rearm barrier {marker}",
            "cmd": "read -r scheduler_drill_release",
            "login": False,
            "accepts_input": True,
            "yield_time_ms": 1,
        },
    )
    write_json(harness.evidence / "wait-rearm-barrier-task.json", barrier)
    barrier_task_id = barrier["id"]
    harness.prompt(
        "wait-rearm-seed",
        DRILL_PREFIX
        + "1. Call CreateWorkItem with objective "
        f"{json.dumps(objective)}, plan_status ready, and exactly two todos: "
        "rearm pending and final-resume pending.\n"
        "2. Call PickWorkItem for that WorkItem.\n"
        "3. Call WaitFor with wake=external and a concrete reason. Do not pass a resource.\n"
        "4. STOP without completing the WorkItem.\n"
        "On the first later resume: call GetWorkItem, mark only rearm completed, "
        "call WaitFor with wake=task_result, "
        f"resource={json.dumps(barrier_task_id)}, and STOP.\n"
        "On the task-result resume: call GetWorkItem, call WaitFor with wake=external "
        "again without a resource, and STOP.\n"
        "On the next external resume: call GetWorkItem, mark both todos completed, "
        f"emit a report containing {completion}, and call CompleteWorkItem.",
    )
    initial = harness.wait_work_item_scheduling_state(
        objective_marker=objective,
        expected_scheduling_state="waiting_external",
        label="wait-rearm-initial",
    )
    deep_checkpoint = harness.claim_checkpoint("wait-rearm-db")
    initial_wait_id = None
    initial_generation = None
    if deep_checkpoint:
        initial_snapshot = harness.runtime_db_snapshot(
            "wait-rearm-initial-generation"
        )
        initial_waits = [
            row
            for row in initial_snapshot["wait_conditions"]
            if row["work_item_id"] == initial["id"] and row["status"] == "active"
        ]
        require(
            len(initial_waits) == 1,
            f"expected one initial active external wait: {initial_waits}",
        )
        initial_wait_id = initial_waits[0]["wait_condition_id"]
        initial_generations = [
            row
            for row in initial_snapshot["scheduler_wait_generations"]
            if row["owner_work_item_id"] == initial["id"]
            and row["wait_id"] == initial_wait_id
        ]
        require(
            len(initial_generations) == 1
            and initial_generations[0]["lifecycle_state"] == "active",
            f"initial canonical wait generation is not active: {initial_generations}",
        )
        initial_generation = initial_generations[0]["generation"]
    harness.wait_agent_asleep()
    before = harness.state("wait-rearm-before-first")
    baseline = int(before["agent"]["agent"]["turn_index"])
    first = harness.fire_callback(
        "wait-rearm-first",
        callback["trigger_url"],
        {"drill": marker, "trigger": "duplicate-first"},
    )
    wait_for_running_turn(
        harness,
        baseline_turn=baseline,
        label="wait-rearm-first-running",
    )
    duplicate = harness.fire_callback(
        "wait-rearm-duplicate",
        callback["trigger_url"],
        {"drill": marker, "trigger": "duplicate-second"},
    )
    require(
        first.get("disposition") == "triggered",
        f"first callback was not a fresh trigger: {first}",
    )
    require(
        duplicate.get("disposition") == "coalesced",
        f"duplicate callback did not land in the running generation: {duplicate}",
    )
    task_waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective,
        expected_scheduling_state="waiting_task",
        label="wait-rearm-task-barrier",
    )
    require(
        task_waiting["id"] == initial["id"],
        "wait-rearm WorkItem identity changed at the task barrier",
    )
    harness.wait_queue_drained()
    if deep_checkpoint:
        duplicate_snapshot = harness.runtime_db_snapshot(
            "wait-rearm-duplicate-processed"
        )
        duplicate_messages = [
            row
            for row in duplicate_snapshot["messages"]
            if row["kind"] == "system_tick"
            and marker in row.get("payload_json", "")
            and "duplicate-second" in row.get("payload_json", "")
        ]
        require(
            len(duplicate_messages) == 1,
            "coalesced duplicate did not produce one durable wake message: "
            f"{duplicate_messages}",
        )
        duplicate_message_id = duplicate_messages[0]["message_id"]
        duplicate_queue = [
            row
            for row in duplicate_snapshot["queue_entries"]
            if row["message_id"] == duplicate_message_id
        ]
        require(
            len(duplicate_queue) == 1
            and duplicate_queue[0]["status"] == "processed",
            f"coalesced duplicate was not processed before rearm: {duplicate_queue}",
        )
        old_generation = [
            row
            for row in duplicate_snapshot["scheduler_wait_generations"]
            if row["wait_id"] == initial_wait_id
            and row["generation"] == initial_generation
        ]
        require(
            len(old_generation) == 1
            and old_generation[0]["lifecycle_state"] in {"consumed", "resolved"}
            and old_generation[0]["trigger_generation"] is not None,
            f"initial wait generation lacks consumed trigger evidence: {old_generation}",
        )
        active_waits = [
            row
            for row in duplicate_snapshot["wait_conditions"]
            if row["work_item_id"] == initial["id"] and row["status"] == "active"
        ]
        require(
            len(active_waits) == 1 and active_waits[0]["kind"] == "task",
            "duplicate was not drained while only the task barrier was active: "
            f"{active_waits}",
        )
    task_input = harness.request(
        "POST",
        harness.agent_path(f"tasks/{barrier_task_id}/input", control=True),
        {"text": "release\n"},
    )
    write_json(harness.evidence / "wait-rearm-barrier-release.json", task_input)
    require(
        task_input.get("accepted_input") is True,
        f"wait-rearm barrier rejected input: {task_input}",
    )
    rearmed = wait_for_rearmed_work_item(
        harness,
        objective=objective,
        previous_revision=int(task_waiting.get("revision", 0)),
        label="wait-rearm-second-generation",
    )
    final = harness.fire_callback(
        "wait-rearm-final",
        callback["trigger_url"],
        {"drill": marker, "trigger": "fresh-after-rearm"},
    )
    require(
        final.get("disposition") == "triggered",
        f"fresh callback did not trigger the rearmed wait: {final}",
    )
    harness.wait_work_item(
        objective_marker=objective,
        expected_state="completed",
        label="wait-rearm-completed",
    )
    return {
        "initial_wait_id": initial_wait_id,
        "initial_wait_generation": initial_generation,
        "duplicate_message_id": duplicate_message_id,
        "initial_revision": int(initial.get("revision", 0)),
        "rearmed_revision": int(rearmed.get("revision", 0)),
        "first_disposition": first.get("disposition"),
        "duplicate_disposition": duplicate.get("disposition"),
        "final_disposition": final.get("disposition"),
    }


def exercise_wrong_fence(harness: CaseHarness, marker: str) -> dict[str, Any]:
    objective = f"DRILL-WRONG-FENCE-TARGET-{marker}"
    completion = f"DRILL-WRONG-FENCE-COMPLETE-{marker}"
    harness.prompt(
        "wrong-fence-seed",
        DRILL_PREFIX
        + "1. Call CreateWorkItem with objective "
        f"{json.dumps(objective)}, plan_status ready, and one todo named bound-input pending.\n"
        "2. Call PickWorkItem for that WorkItem.\n"
        "3. Call WaitFor with wake=operator_input and a concrete reason.\n"
        "4. STOP without completing the WorkItem.\n"
        "When correctly resumed later: call GetWorkItem, mark the todo completed, "
        f"emit a report containing {completion}, and call CompleteWorkItem.",
    )
    target = harness.wait_work_item_scheduling_state(
        objective_marker=objective,
        expected_scheduling_state="waiting_operator",
        label="wrong-fence-target-waiting",
    )
    bogus_work_item_id = f"work-drill-missing-{marker}"
    before = harness.state("wrong-fence-before")
    baseline = int(before["agent"]["agent"]["turn_index"])
    response = harness.request(
        "POST",
        harness.agent_path("prompt", control=True),
        {
            "text": (
                f"Scheduler drill wrong WorkItem fence {marker}. "
                "Do not create or modify WorkItems; answer with the marker only."
            ),
            "work_item_id": bogus_work_item_id,
        },
    )
    write_json(harness.evidence / "wrong-fence-response.json", response)
    wait_for_turn_after(harness, baseline, "wrong-fence-finished")
    items = harness.work_items("wrong-fence-check")
    matches = [
        item
        for item in items
        if item.get("id") == bogus_work_item_id
    ]
    require(not matches, "wrong WorkItem fence unexpectedly created a WorkItem")
    target_matches = [
        item for item in items if item.get("id") == target["id"]
    ]
    require(
        len(target_matches) == 1
        and target_matches[0].get("scheduling_state") == "waiting_operator",
        "wrong WorkItem fence resumed the target WorkItem",
    )
    correct = harness.request(
        "POST",
        harness.agent_path("prompt", control=True),
        {
            "text": f"Resume the exact bound WorkItem and follow its objective. {marker}",
            "work_item_id": target["id"],
        },
    )
    write_json(harness.evidence / "right-fence-response.json", correct)
    harness.wait_work_item(
        objective_marker=objective,
        expected_state="completed",
        label="right-fence-completed",
    )
    return {
        "wrong_work_item_id": bogus_work_item_id,
        "target_work_item_id": target["id"],
        "target_remained_waiting": True,
        "correct_fence_completed": True,
    }


def exercise_wait_triggers(harness: CaseHarness, marker: str) -> None:
    callback_seed = seed_wait(
        harness,
        label="callback",
        marker=marker,
    )
    callback = harness.reset_callback("callback-trigger")
    # Ensure the agent has fully transitioned to asleep before firing the
    # callback; firing during the awake_idle→asleep window loses the wake.
    harness.wait_agent_asleep()
    harness.fire_callback(
        "wait-callback-trigger",
        callback["trigger_url"],
        {"drill": marker, "trigger": "callback"},
    )
    wait_seed_completion(harness, callback_seed, "callback")

    harness.wait_queue_drained()
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

    harness.wait_queue_drained()
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

    harness.wait_queue_drained()
    timer = harness.request(
        "POST",
        harness.agent_path("timers", control=True),
        {
            "duration_ms": 120_000,
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
        expected_scheduling_state="waiting_timer",
    )
    wait_seed_completion(harness, timer_seed, "timer")

    harness.wait_queue_drained()
    system_seed = seed_wait(
        harness,
        label="system",
        marker=marker,
        wake="system",
        expected_scheduling_state="waiting_system",
    )
    system_wake = harness.request(
        "POST",
        harness.agent_path("wake", control=True),
        {"reason": f"scheduler drill system wake {marker}", "source": "scheduler-drill"},
    )
    write_json(harness.evidence / "wait-system.json", system_wake)
    wait_seed_completion(harness, system_seed, "system")

    harness.wait_queue_drained()
    wake_seed = seed_wait(
        harness,
        label="wake-hint",
        marker=marker,
        wake="system",
        expected_scheduling_state="waiting_system",
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
        f"with cmd `sleep 15; printf DRILL-TASK-{marker}`, yield_time_ms=50, and a "
        "bounded max_output_tokens. Call WaitFor with wake=task_result and the exact "
        "promoted task_id. Do not poll. On task-result rejoin, call GetWorkItem and "
        f"WaitFor with wake=external, resource=drill:continuation:{marker}. On the "
        "external resume, call GetWorkItem, update the two existing todos to completed, "
        f"emit a concise result containing {completion_marker}, and immediately call "
        "CompleteWorkItem for the exact current WorkItem. Do not create another item."
    )
    harness.prompt(
        "continuation-seed",
        DRILL_PREFIX
        + "1. Call CreateWorkItem with objective "
        f"{json.dumps(objective)}, plan_status ready, with exactly two todos: "
        "task-rejoin pending and external-resume pending.\n"
        "2. STOP immediately after CreateWorkItem succeeds. Do NOT call PickWorkItem, "
        "ExecCommand, WaitFor, UpdateWorkItem, or CompleteWorkItem in this turn.",
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
    harness.wait_agent_asleep()
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
        DRILL_PREFIX
        + "1. Call CreateWorkItem with objective "
        f"{json.dumps(objective)}, plan_status ready, and one todo named bound-input pending.\n"
        "2. Call PickWorkItem for that WorkItem.\n"
        "3. Call WaitFor with wake=operator_input and a concrete reason.\n"
        "4. STOP. Do not complete the WorkItem in this turn.\n"
        "When resumed later: call GetWorkItem, update the todo to completed, emit a "
        f"report containing {completion}, and call CompleteWorkItem.",
    )
    waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective,
        expected_scheduling_state="waiting_operator",
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
            "text": DRILL_PREFIX
            + "1. Call ExecCommand with "
            f"cmd `sleep 4; printf DRILL-INTERJECTION-{marker}`, yield_time_ms=10000, "
            "and bounded output.\n"
            "2. After the tool result, answer with "
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


def stress_agent_id(run_id: str, worker: int) -> str:
    digest = hashlib.sha256(run_id.encode()).hexdigest()[:12]
    return f"drill-agent-{digest}-w{worker + 1}"


def prepare_stress_harnesses(
    harness: CaseHarness,
    *,
    run_id: str,
    concurrency: int,
) -> dict[int, CaseHarness]:
    workers = {}
    for worker in range(concurrency):
        agent_id = stress_agent_id(run_id, worker)
        harness.request(
            "POST",
            f"/api/control/agents/{agent_id}/create",
            {},
        )
        worker_harness = copy.copy(harness)
        worker_harness.agent_id = agent_id
        worker_harness.evidence = harness.evidence / f"worker-{worker + 1}"
        worker_harness.evidence.mkdir(parents=True, exist_ok=True)
        worker_harness.wait_agent_idle()
        workers[worker] = worker_harness
    return workers


def exercise_scenario_operation(
    harness: CaseHarness,
    operation: StressOperation,
) -> dict[str, Any]:
    harness.wait_queue_drained()
    injection: dict[str, Any] = {}
    if operation.duplicate:
        injection["duplicate"] = exercise_wait_rearm_race(
            harness,
            f"{operation.marker}-duplicate",
        )
    if operation.fault == "stale":
        injection["stale"] = exercise_wait_rearm_race(
            harness,
            f"{operation.marker}-stale",
        )
    if operation.fault == "wrong_fence":
        injection["wrong_fence"] = exercise_wrong_fence(harness, operation.marker)

    if operation.scenario == "reducer_only_candidates":
        detail = exercise_reducer_ingress(
            harness,
            operation.marker,
            duplicate=False,
            out_of_order=operation.fault == "out_of_order",
        )
        if operation.fault == "out_of_order":
            injection["out_of_order"] = {
                "requests": detail["out_of_order_requests"],
            }
    elif operation.scenario == "exact_wait_resume":
        exercise_wait_triggers(harness, operation.marker)
    elif operation.scenario in {
        "work_item_autonomous_continuation",
        "exact_task_rejoin",
    }:
        exercise_continuations(harness, operation.marker)
    elif operation.scenario == "explicitly_bound_operator_input":
        exercise_bound_operator(harness, operation.marker)
    elif operation.scenario == "operator_interjection":
        exercise_interjection(harness, operation.marker)
    elif operation.scenario in {"settlement", "delivery"}:
        exercise_wait_triggers(harness, f"{operation.marker}-wait")
        exercise_continuations(harness, f"{operation.marker}-continuation")
    else:
        raise AssertionError(f"unsupported scenario: {operation.scenario}")

    harness.capture_context("operation-final", include_conversation=False)
    return {"injections": injection}


def stress_result_summary(
    plan: list[StressOperation],
    results: list[dict[str, Any]],
) -> dict[str, Any]:
    scenario_planned = {scenario: 0 for scenario in PRODUCTION_SCENARIOS}
    scenario_completed = {scenario: 0 for scenario in PRODUCTION_SCENARIOS}
    injection_planned = {
        "duplicate": 0,
        "stale": 0,
        "out_of_order": 0,
        "wrong_fence": 0,
    }
    injection_completed = dict.fromkeys(injection_planned, 0)
    for operation in plan:
        scenario_planned[operation.scenario] += 1
        if operation.duplicate:
            injection_planned["duplicate"] += 1
        if operation.fault is not None:
            injection_planned[operation.fault] += 1
    for result in results:
        if result["status"] != "completed":
            continue
        scenario_completed[result["scenario"]] += 1
        injections = result.get("detail", {}).get("injections", {})
        for injection in injection_completed:
            if injection in injections:
                injection_completed[injection] += 1
    failures = [result for result in results if result["status"] != "completed"]
    return {
        "operation_count": len(plan),
        "completed_count": len(plan) - len(failures),
        "failed_count": len(failures),
        "max_workers": len({operation.worker for operation in plan}),
        "scenario_planned": scenario_planned,
        "scenario_completed": scenario_completed,
        "injection_planned": injection_planned,
        "injection_completed": injection_completed,
        "failures": failures,
    }


def exercise_scenarios(args: argparse.Namespace) -> int:
    paths = DrillPaths.from_root(args.run_dir)
    record = load_record(paths)
    validate_record(paths, record)
    require(container_running(record["resources"]["container"]), "candidate is not running")
    phase_label = f"exercise-{len(record['phase_history']) + 1}"
    selected = list(
        dict.fromkeys(args.scenario or record["parameters"]["scenarios"])
    )
    parameters = record["parameters"]
    evidence_path = paths.phases / phase_label
    plan: list[StressOperation] = []
    try:
        harness = make_harness(
            paths,
            record,
            label=phase_label,
            mode=record.get("last_mode") or "shadow",
            env_file=None,
            require_credentials=False,
        )
        attach_running(harness)
        plan = build_stress_plan(
            scenarios=selected,
            iterations=int(parameters["iterations"]),
            concurrency=int(parameters["concurrency"]),
            duplicate_ratio=float(parameters["duplicate_ratio"]),
            stale_ratio=float(parameters["stale_ratio"]),
            seed=f"{record['drill_run_id']}:{phase_label}",
        )
        write_json(
            harness.evidence / "stress-plan.json",
            [item.as_dict() for item in plan],
        )
        workers = prepare_stress_harnesses(
            harness,
            run_id=record["drill_run_id"],
            concurrency=int(parameters["concurrency"]),
        )
        harness.check_docker_health("stress-start")
        harness.resource_telemetry("stress-start")
        telemetry_lock = threading.Lock()
        completed_operations = 0

        def run_operation(operation: StressOperation) -> dict[str, Any]:
            nonlocal completed_operations
            operation_harness = copy.copy(workers[operation.worker])
            operation_harness.evidence = (
                harness.evidence
                / f"operation-{operation.sequence + 1:06d}-{operation.scenario}"
            )
            operation_harness.evidence.mkdir(parents=True, exist_ok=False)
            detail = exercise_scenario_operation(operation_harness, operation)
            with telemetry_lock:
                completed_operations += 1
                should_checkpoint = completed_operations % max(
                    1,
                    int(parameters["concurrency"]) * 8,
                ) == 0
                checkpoint = completed_operations
            if should_checkpoint:
                harness.check_docker_health(f"stress-{checkpoint:06d}")
                harness.resource_telemetry(f"stress-{checkpoint:06d}")
            return detail

        results = execute_stress_plan(
            plan,
            concurrency=int(parameters["concurrency"]),
            run_operation=run_operation,
        )
        harness.check_docker_health("stress-final")
        harness.resource_telemetry("stress-final")
    except Exception as error:
        setup_error = f"{type(error).__name__}: {error}"
        evidence_path.mkdir(parents=True, exist_ok=True)
        results = [
            {
                **operation.as_dict(),
                "status": "failed",
                "duration_seconds": 0.0,
                "error": f"stress setup failed: {setup_error}",
            }
            for operation in plan
        ]
        write_json(evidence_path / "stress-results.json", results)
        summary = stress_result_summary(plan, results)
        summary["setup_error"] = setup_error
        write_json(evidence_path / "stress-summary.json", summary)
        append_phase(
            paths,
            record,
            action="exercise",
            status="failed",
            detail={
                "mode": record.get("last_mode"),
                "mode_session": int(record.get("mode_session", 0)),
                "scenarios": selected,
                "stress": summary,
                "evidence": str(evidence_path),
            },
        )
        raise AssertionError(
            f"stress setup failed; see {evidence_path}: {setup_error}"
        ) from error
    write_json(harness.evidence / "stress-results.json", results)
    summary = stress_result_summary(plan, results)
    write_json(harness.evidence / "stress-summary.json", summary)
    status = "completed" if summary["failed_count"] == 0 else "failed"
    append_phase(
        paths,
        record,
        action="exercise",
        status=status,
        detail={
            "mode": record.get("last_mode"),
            "mode_session": int(record.get("mode_session", 0)),
            "scenarios": selected,
            "stress": summary,
            "evidence": str(harness.evidence),
        },
    )
    require(
        summary["failed_count"] == 0,
        f"{summary['failed_count']} stress operations failed; see {harness.evidence}",
    )
    return 0


def exercise_restart_checkpoint(args: argparse.Namespace) -> int:
    paths = DrillPaths.from_root(args.run_dir)
    record = load_record(paths)
    validate_record(paths, record)
    require(
        not container_running(record["resources"]["container"]),
        "restart checkpoint requires the candidate container to be stopped",
    )
    checkpoint = args.checkpoint
    mode = record.get("last_mode") or "shadow"
    completed_restart_checkpoints = {
        phase.get("detail", {}).get("restart", {}).get("checkpoint")
        for phase in record.get("phase_history", [])
        if phase.get("action") == "restart_checkpoint"
        and phase.get("status") == "completed"
    }
    require(
        "authority_rollback" not in completed_restart_checkpoints,
        "authority_rollback must be the final restart checkpoint",
    )
    if checkpoint == "authority_rollback":
        require(mode == "authoritative", "authority_rollback requires authoritative mode")
    phase_label = (
        f"restart-{checkpoint}-{len(record.get('phase_history', [])) + 1}"
    )
    evidence_path = paths.phases / phase_label
    mode_session = int(record.get("mode_session", 0))
    agent = re.sub(
        r"[^a-z0-9-]",
        "-",
        f"drill-restart-{mode_session}-{checkpoint}".lower(),
    )[-63:]
    objective = (
        f"scheduler restart checkpoint {checkpoint} "
        f"for {record['drill_run_id']} mode-session {mode_session}"
    )
    try:
        harness = make_harness(
            paths,
            record,
            label=phase_label,
            mode=mode,
            env_file=None,
            require_credentials=False,
        )
        prepare = harness.seed_scheduler_restart_fixture(
            "prepare",
            agent=agent,
            checkpoint=checkpoint,
            stage="prepare",
            objective=objective,
        )
        replay = harness.seed_scheduler_restart_fixture(
            "replay",
            agent=agent,
            checkpoint=checkpoint,
            stage="replay",
            objective=objective,
        )
        verify = harness.seed_scheduler_restart_fixture(
            "verify",
            agent=agent,
            checkpoint=checkpoint,
            stage="verify",
            objective=objective,
        )
        identity_fields = (
            "message_id",
            "work_item_id",
            "activation_id",
            "command_identity",
        )
        identity_stable = all(
            prepare.get(field) is None
            or (
                prepare.get(field) == replay.get(field)
                and replay.get(field) == verify.get(field)
            )
            for field in identity_fields
        )
        restart = {
            "checkpoint": checkpoint,
            "cut_kind": RESTART_CHECKPOINT_CUT_KINDS[checkpoint],
            "agent_id": agent,
            "first_restart_recovered": replay.get("replay_exactly_once") is True,
            "second_restart_idempotent": (
                verify.get("replay_exactly_once") is True and identity_stable
            ),
            "replay_exactly_once": (
                replay.get("replay_exactly_once") is True
                and verify.get("replay_exactly_once") is True
                and identity_stable
            ),
            "subsequent_progress": replay.get("replay_applied") is True,
            "prepare": prepare,
            "replay": replay,
            "verify": verify,
        }
        require(
            all(
                restart[field] is True
                for field in (
                    "first_restart_recovered",
                    "second_restart_idempotent",
                    "replay_exactly_once",
                    "subsequent_progress",
                )
            ),
            f"restart checkpoint verification failed: {restart}",
        )
        write_json(evidence_path / "restart-summary.json", restart)
        append_phase(
            paths,
            record,
            action="restart_checkpoint",
            status="completed",
            detail={
                "mode": mode,
                "mode_session": mode_session,
                "evidence": str(evidence_path),
                "restart": restart,
            },
        )
        return 0
    except Exception as error:
        evidence_path.mkdir(parents=True, exist_ok=True)
        restart = {
            "checkpoint": checkpoint,
            "cut_kind": RESTART_CHECKPOINT_CUT_KINDS[checkpoint],
            "first_restart_recovered": False,
            "second_restart_idempotent": False,
            "replay_exactly_once": False,
            "subsequent_progress": False,
            "error": f"{type(error).__name__}: {error}",
        }
        write_json(evidence_path / "restart-summary.json", restart)
        append_phase(
            paths,
            record,
            action="restart_checkpoint",
            status="failed",
            detail={
                "mode": mode,
                "mode_session": mode_session,
                "evidence": str(evidence_path),
                "restart": restart,
            },
        )
        raise AssertionError(
            f"restart checkpoint failed; see {evidence_path}: {error}"
        ) from error


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
                "legacy_observation_json, shadow_candidate_json, "
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
        "shadow_comparisons",
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


def wait_resume_trigger_coverage(evidence: dict[str, Any]) -> dict[str, int]:
    coverage = {trigger: 0 for trigger in EXACT_WAIT_RESUME_TRIGGERS}
    for row in evidence["shadow_comparisons"]:
        if row["scenario_class"] != "exact_wait_resume":
            continue
        observation = row.get("legacy_observation_json_value") or {}
        input_kind = observation.get("input_kind")
        wake_source = observation.get("wake_source")
        trigger = {
            "callback_event": "callback",
            "webhook_event": "webhook",
            "channel_event": "channel",
            "timer_tick": "timer",
        }.get(input_kind)
        if input_kind == "system_tick":
            trigger = (
                "operator_wake"
                if wake_source == "operator_wake_hint"
                else "system"
            )
        if trigger is not None:
            coverage[trigger] += 1
    return coverage


def scenario_mode_mismatches(
    evidence: dict[str, Any],
    expected_mode: str | None,
) -> list[dict[str, Any]]:
    if expected_mode not in {"shadow", "authoritative"}:
        return []
    authorities = {
        row["scenario_class"]: row for row in evidence["scenario_authorities"]
    }
    return [
        {
            "scenario_class": scenario,
            "expected_mode": expected_mode,
            "actual_mode": authorities.get(scenario, {}).get("mode"),
        }
        for scenario in PRODUCTION_SCENARIOS
        if authorities.get(scenario, {}).get("mode") != expected_mode
    ]


def aggregate_stress_coverage(
    record: dict[str, Any],
    *,
    expected_mode: str | None,
    expected_mode_session: int,
) -> dict[str, Any]:
    scenarios = list(record["parameters"]["scenarios"])
    iterations = int(record["parameters"]["iterations"])
    scenario_planned = {scenario: 0 for scenario in PRODUCTION_SCENARIOS}
    scenario_completed = {scenario: 0 for scenario in PRODUCTION_SCENARIOS}
    injection_planned = {
        "duplicate": 0,
        "stale": 0,
        "out_of_order": 0,
        "wrong_fence": 0,
    }
    injection_completed = dict.fromkeys(injection_planned, 0)
    operation_count = 0
    completed_count = 0
    failed_count = 0
    phases = []
    failed_phases = []
    latest_phase_status = None
    for phase in record.get("phase_history", []):
        stress = phase.get("detail", {}).get("stress")
        if phase.get("action") != "exercise" or not isinstance(stress, dict):
            continue
        phase_mode = phase.get("detail", {}).get("mode")
        phase_mode_session = int(
            phase.get("detail", {}).get("mode_session", 0)
        )
        if (
            phase_mode != expected_mode
            or phase_mode_session != expected_mode_session
        ):
            continue
        latest_phase_status = phase.get("status")
        phases.append(
            {
                "at": phase.get("at"),
                "status": phase.get("status"),
                "mode": phase_mode,
                "mode_session": phase_mode_session,
                "evidence": phase.get("detail", {}).get("evidence"),
            }
        )
        if phase.get("status") != "completed":
            failed_phases.append(phases[-1])
            continue
        operation_count += int(stress.get("operation_count", 0))
        completed_count += int(stress.get("completed_count", 0))
        failed_count += int(stress.get("failed_count", 0))
        for scenario in scenario_planned:
            scenario_planned[scenario] += int(
                stress.get("scenario_planned", {}).get(scenario, 0)
            )
            scenario_completed[scenario] += int(
                stress.get("scenario_completed", {}).get(scenario, 0)
            )
        for injection in injection_planned:
            injection_planned[injection] += int(
                stress.get("injection_planned", {}).get(injection, 0)
            )
            injection_completed[injection] += int(
                stress.get("injection_completed", {}).get(injection, 0)
            )
    scenario_shortfalls = {
        scenario: {
            "required": iterations,
            "completed": scenario_completed[scenario],
        }
        for scenario in scenarios
        if scenario_completed[scenario] < iterations
    }
    injection_shortfalls = {
        injection: {
            "planned": injection_planned[injection],
            "completed": injection_completed[injection],
        }
        for injection in injection_planned
        if injection_completed[injection] < injection_planned[injection]
    }
    required_injections = [
        injection
        for injection, planned in injection_planned.items()
        if planned > 0
    ]
    missing_required_injections = [
        injection
        for injection in required_injections
        if injection_completed[injection] == 0
    ]
    return {
        "phases": phases,
        "failed_phases": failed_phases,
        "latest_phase_status": latest_phase_status,
        "operation_count": operation_count,
        "completed_count": completed_count,
        "failed_count": failed_count,
        "scenario_planned": scenario_planned,
        "scenario_completed": scenario_completed,
        "scenario_shortfalls": scenario_shortfalls,
        "injection_planned": injection_planned,
        "injection_completed": injection_completed,
        "injection_shortfalls": injection_shortfalls,
        "required_injections": required_injections,
        "missing_required_injections": missing_required_injections,
    }


def aggregate_restart_coverage(
    record: dict[str, Any],
    *,
    expected_mode: str | None,
    expected_mode_session: int,
) -> dict[str, Any]:
    latest_by_checkpoint: dict[str, dict[str, Any]] = {}
    for phase in record.get("phase_history", []):
        detail = phase.get("detail", {})
        restart = detail.get("restart")
        if phase.get("action") != "restart_checkpoint" or not isinstance(restart, dict):
            continue
        if (
            detail.get("mode") != expected_mode
            or int(detail.get("mode_session", 0)) != expected_mode_session
        ):
            continue
        checkpoint = restart.get("checkpoint")
        if checkpoint not in RESTART_CHECKPOINTS:
            continue
        latest_by_checkpoint[checkpoint] = {
            "at": phase.get("at"),
            "status": phase.get("status"),
            "mode": detail.get("mode"),
            "mode_session": int(detail.get("mode_session", 0)),
            "evidence": detail.get("evidence"),
            **restart,
        }

    missing = [
        checkpoint
        for checkpoint in RESTART_CHECKPOINTS
        if checkpoint not in latest_by_checkpoint
    ]
    failed = {
        checkpoint: phase
        for checkpoint, phase in latest_by_checkpoint.items()
        if phase.get("status") != "completed"
    }
    cut_kind_mismatches = {
        checkpoint: {
            "expected": RESTART_CHECKPOINT_CUT_KINDS[checkpoint],
            "actual": phase.get("cut_kind"),
        }
        for checkpoint, phase in latest_by_checkpoint.items()
        if phase.get("cut_kind") != RESTART_CHECKPOINT_CUT_KINDS[checkpoint]
    }
    verification_fields = (
        "first_restart_recovered",
        "second_restart_idempotent",
        "replay_exactly_once",
        "subsequent_progress",
    )
    verification_failures = {
        checkpoint: [
            field for field in verification_fields if phase.get(field) is not True
        ]
        for checkpoint, phase in latest_by_checkpoint.items()
        if any(phase.get(field) is not True for field in verification_fields)
    }
    return {
        "required_checkpoints": list(RESTART_CHECKPOINTS),
        "completed_checkpoints": [
            checkpoint
            for checkpoint in RESTART_CHECKPOINTS
            if checkpoint in latest_by_checkpoint
            and checkpoint not in failed
            and checkpoint not in cut_kind_mismatches
            and checkpoint not in verification_failures
        ],
        "latest_by_checkpoint": latest_by_checkpoint,
        "missing_checkpoints": missing,
        "failed_checkpoints": failed,
        "cut_kind_mismatches": cut_kind_mismatches,
        "verification_failures": verification_failures,
    }


def evidence_summary(
    evidence: dict[str, Any],
    *,
    expected_mode: str | None = None,
    stress: dict[str, Any] | None = None,
    restart: dict[str, Any] | None = None,
) -> dict[str, Any]:
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
        if row["lifecycle_state"] in {"active", "triggered", "consumed"}
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
    wait_trigger_counts = wait_resume_trigger_coverage(evidence)
    mode_mismatches = scenario_mode_mismatches(evidence, expected_mode)
    queue_tail = [
        row
        for row in evidence["queue_status"]
        if row["status"] in {"queued", "dequeued"}
    ]
    checks = {
        "all_scenarios_observed": all(counts.values()),
        "all_wait_resume_triggers_observed": all(wait_trigger_counts.values()),
        "scenario_modes_match_requested_mode": not mode_mismatches,
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
    if stress is not None:
        checks.update(
            {
                "stress_scenarios_executed": not stress["scenario_shortfalls"],
                "stress_operations_completed": stress["failed_count"] == 0,
                "latest_stress_phase_completed": stress["latest_phase_status"]
                == "completed",
                "stress_injections_completed": not stress["injection_shortfalls"],
                "required_stress_injections_observed": not stress[
                    "missing_required_injections"
                ],
            }
        )
    if restart is not None:
        checks.update(
            {
                "all_restart_checkpoints_observed": not restart[
                    "missing_checkpoints"
                ],
                "restart_checkpoints_completed": not restart[
                    "failed_checkpoints"
                ],
                "restart_cut_kinds_match_contract": not restart[
                    "cut_kind_mismatches"
                ],
                "restart_recovery_and_replay_verified": not restart[
                    "verification_failures"
                ],
            }
        )
    return {
        "status": "go" if all(checks.values()) else "no-go",
        "checks": checks,
        "scenario_counts": counts,
        "wait_resume_trigger_counts": wait_trigger_counts,
        "scenario_mode_mismatches": mode_mismatches,
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
        "stress": stress,
        "restart": restart,
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
    stress = summary.get("stress")
    if stress is not None:
        lines.extend(
            [
                "",
                "## Stress execution",
                "",
                f"- Operations: {stress['completed_count']} completed / "
                f"{stress['operation_count']} planned; {stress['failed_count']} failed",
                f"- Scenario shortfalls: {len(stress['scenario_shortfalls'])}",
                f"- Injection shortfalls: {len(stress['injection_shortfalls'])}",
                f"- Missing required injection types: "
                f"{', '.join(stress['missing_required_injections']) or 'none'}",
                "",
                "| Injection | Planned | Completed |",
                "|---|---:|---:|",
            ]
        )
        lines.extend(
            f"| `{injection}` | {stress['injection_planned'][injection]} | "
            f"{stress['injection_completed'][injection]} |"
            for injection in stress["injection_planned"]
        )
    restart = summary.get("restart")
    if restart is not None:
        lines.extend(
            [
                "",
                "## Restart checkpoints",
                "",
                f"- Completed: {len(restart['completed_checkpoints'])} / "
                f"{len(restart['required_checkpoints'])}",
                f"- Missing: {', '.join(restart['missing_checkpoints']) or 'none'}",
                f"- Failed: {', '.join(restart['failed_checkpoints']) or 'none'}",
                f"- Cut-kind mismatches: "
                f"{', '.join(restart['cut_kind_mismatches']) or 'none'}",
                f"- Verification failures: "
                f"{', '.join(restart['verification_failures']) or 'none'}",
                "",
                "| Checkpoint | Cut kind | Status |",
                "|---|---|---|",
            ]
        )
        lines.extend(
            f"| `{checkpoint}` | "
            f"`{restart['latest_by_checkpoint'].get(checkpoint, {}).get('cut_kind', 'missing')}` | "
            f"{restart['latest_by_checkpoint'].get(checkpoint, {}).get('status', 'missing')} |"
            for checkpoint in restart["required_checkpoints"]
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
    engine_identity = docker_engine_identity()
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
    tracked_status = run(
        ["git", "status", "--porcelain", "--untracked-files=no"]
    ).stdout.strip()
    require(not tracked_status, "prepare requires no tracked Git changes")
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
        "docker_engine": engine_identity,
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
        "mode_session": 0,
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
    if record.get("last_mode") != args.mode:
        record["mode_session"] = int(record.get("mode_session", 0)) + 1
    record["last_mode"] = args.mode
    append_phase(
        paths,
        record,
        action="start",
        status="completed",
        detail={
            "mode": args.mode,
            "mode_session": record["mode_session"],
            "agent_id": harness.agent_id,
        },
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
    summary = evidence_summary(
        evidence,
        expected_mode=record.get("last_mode"),
        stress=aggregate_stress_coverage(
            record,
            expected_mode=record.get("last_mode"),
            expected_mode_session=int(record.get("mode_session", 0)),
        ),
        restart=aggregate_restart_coverage(
            record,
            expected_mode=record.get("last_mode"),
            expected_mode_session=int(record.get("mode_session", 0)),
        ),
    )
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
        "mode_session": int(record.get("mode_session", 0)),
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
                DRILL_PREFIX
                + "1. Call AgentGet.\n"
                "2. Call ListModelProviders.\n"
                f"3. Call ListProviderModels for provider {provider} with limit 5.\n"
                f"4. Answer with the literal marker {marker}.",
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
        ("restart-checkpoint", exercise_restart_checkpoint),
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
        if command == "restart-checkpoint":
            command_parser.add_argument(
                "--checkpoint",
                choices=RESTART_CHECKPOINTS,
                required=True,
            )
        if command == "collect":
            command_parser.add_argument("--label")
        command_parser.set_defaults(handler=handler)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    os.environ["DOCKER_HOST"] = os.environ.get(
        "HOLON_DRILL_DOCKER_HOST",
        DEFAULT_NATIVE_DOCKER_HOST,
    )
    os.environ.pop("DOCKER_CONTEXT", None)
    if hasattr(args, "fallback_model") and args.fallback_model is None:
        args.fallback_model = list(DEFAULT_FALLBACK_MODELS)
    return int(args.handler(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2)
