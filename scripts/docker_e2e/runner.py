#!/usr/bin/env python3
"""Release Docker E2E against a real LLM and the public Holon HTTP API."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import shutil
import sqlite3
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from xml.etree import ElementTree


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = ROOT / "tests/e2e/docker/manifest.json"
DEFAULT_MODEL = "deepseek/deepseek-v4-flash"
OFFLINE_MODEL_CREDENTIAL_ENV = "DEEPSEEK_API_KEY"
OFFLINE_MODEL_CREDENTIAL = "docker-e2e-offline-provider-unused"
EVIDENCE_SCHEMA_VERSION = 1
TERMINAL_STATUSES = {"awake_idle", "asleep", "awaiting_task"}


def run(
    args: list[str],
    *,
    check: bool = True,
    capture: bool = True,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        check=check,
        text=True,
        capture_output=capture,
        env=env,
    )


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(redact_evidence(value), indent=2, ensure_ascii=False) + "\n"
    )


CALLBACK_CAPABILITY_PATTERN = re.compile(
    r"(/api/callbacks/(?:wake|enqueue)/)[A-Za-z0-9_-]+"
)
CALLBACK_CAPABILITY_SCAN_PATTERN = re.compile(
    r"/api/callbacks/(?:wake|enqueue)/(?!<redacted>)[A-Za-z0-9_-]+"
)
BEARER_SECRET_PATTERN = re.compile(
    r"(?:Authorization:\s*Bearer\s+|\"authorization\"\s*:\s*\"Bearer\s+)"
    r"(?!<token>)[A-Za-z0-9._~+/=-]{8,}",
    re.IGNORECASE,
)


def redact_evidence(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: redact_evidence(item) for key, item in value.items()}
    if isinstance(value, list):
        return [redact_evidence(item) for item in value]
    if isinstance(value, str):
        return CALLBACK_CAPABILITY_PATTERN.sub(r"\1<redacted>", value)
    return value


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def first_env(*names: str, default: str = "") -> str:
    for name in names:
        value = os.environ.get(name, "").strip()
        if value:
            return value
    return default


def env_flag(*names: str) -> bool:
    return first_env(*names).lower() in {"1", "true", "yes", "on"}


def inferred_credential_env(model: str) -> str | None:
    provider = model.split("/", 1)[0].split("@", 1)[0]
    return {
        "openai": "OPENAI_API_KEY",
        "anthropic": "ANTHROPIC_AUTH_TOKEN",
        "deepseek": "DEEPSEEK_API_KEY",
        "deepseek-anthropic": "DEEPSEEK_API_KEY",
        "xai": "XAI_API_KEY",
    }.get(provider)


def normalize_model_route(model: str) -> str:
    if "/" not in model:
        return model
    provider, name = model.split("/", 1)
    if "@" not in provider:
        provider = f"{provider}@default"
    return f"{provider}/{name}"


class CaseHarness:
    def __init__(
        self,
        *,
        case_id: str,
        image: str,
        model: str,
        requires_model: bool = True,
        credential_envs: list[str],
        env_file: Path | None,
        runtime_env: dict[str, str],
        evidence_root: Path,
        timeout_seconds: int,
        keep: bool,
    ) -> None:
        suffix = secrets.token_hex(4)
        self.case_id = case_id
        self.image = image
        self.model = model if requires_model else DEFAULT_MODEL
        self.credential_envs = credential_envs if requires_model else []
        self.env_file = env_file if requires_model else None
        self.runtime_env = dict(runtime_env)
        if not requires_model:
            self.runtime_env.setdefault(
                OFFLINE_MODEL_CREDENTIAL_ENV,
                OFFLINE_MODEL_CREDENTIAL,
            )
        self.evidence = evidence_root / case_id
        self.timeout_seconds = timeout_seconds
        self.keep = keep
        self.volume = f"holon-live-{case_id}-{suffix}"
        self.network = f"holon-live-{case_id}-{suffix}"
        self.container = f"holon-live-{case_id}-{suffix}"
        self.token = secrets.token_urlsafe(24)
        self.base_url = ""
        self.agent_id = ""
        self.workspace_parent = self.evidence / "workspace"
        self.log_index = 0
        self.evidence.mkdir(parents=True, exist_ok=True)

    def docker(self, *args: str, **kwargs: Any) -> subprocess.CompletedProcess[str]:
        return run(["docker", *args], **kwargs)

    def offline_debug(
        self, label: str, *args: str, expect_success: bool = True
    ) -> dict[str, Any]:
        self.docker("volume", "create", self.volume)
        command = [
            "run",
            "--rm",
            "--volume",
            f"{self.volume}:/var/lib/holon",
            "--volume",
            f"{self.evidence}:/acceptance-evidence:ro",
        ]
        for name, value in sorted(self.runtime_env.items()):
            command.extend(["--env", f"{name}={value}"])
        command.extend([self.image, "debug", *args, "--json"])
        result = self.docker(*command, check=False)
        (self.evidence / f"{label}-stdout.json").write_text(result.stdout)
        (self.evidence / f"{label}-stderr.log").write_text(result.stderr)
        if not expect_success:
            require(
                result.returncode != 0,
                f"offline debug command unexpectedly succeeded for {label}",
            )
            return {
                "returncode": result.returncode,
                "stderr": result.stderr.strip(),
            }
        require(
            result.returncode == 0,
            f"offline debug command failed for {label}: {result.stderr.strip()}",
        )
        value = json.loads(result.stdout)
        write_json(self.evidence / f"{label}.json", value)
        return value

    def apply_scheduler_rollout(
        self, label: str, commands: list[dict[str, Any]]
    ) -> dict[str, Any]:
        path = self.evidence / f"{label}-input.json"
        write_json(path, {"commands": commands})
        return self.offline_debug(
            label,
            "scheduler-rollout-apply",
            "--input",
            f"/acceptance-evidence/{path.name}",
        )

    def reject_scheduler_rollout(
        self, label: str, commands: list[dict[str, Any]]
    ) -> dict[str, Any]:
        path = self.evidence / f"{label}-input.json"
        write_json(path, {"commands": commands})
        return self.offline_debug(
            label,
            "scheduler-rollout-apply",
            "--input",
            f"/acceptance-evidence/{path.name}",
            expect_success=False,
        )

    def seed_scheduler_recovery_fixture(
        self, label: str, objective: str
    ) -> dict[str, Any]:
        return self.offline_debug(
            label,
            "scheduler-recovery-fixture",
            "--objective",
            objective,
        )

    def initialize_workspace(self) -> None:
        self.workspace_parent.mkdir(parents=True, exist_ok=True)
        self.workspace_parent.chmod(0o777)
        self.docker(
            "run",
            "--rm",
            "--volume",
            f"{self.workspace_parent}:/acceptance",
            "--entrypoint",
            "bash",
            self.image,
            "-lc",
            "set -euo pipefail; "
            "mkdir -p /acceptance/repo; cd /acceptance/repo; "
            "git init -b main; "
            "git config user.email holon-live@example.invalid; "
            "git config user.name 'Holon Live Acceptance'; "
            "printf 'holon live acceptance\\n' > README.md; "
            "git add README.md; git commit -m 'acceptance fixture'",
        )

    def start(self, *, wait_idle: bool = True) -> None:
        self.docker("volume", "create", self.volume)
        if self.docker("network", "inspect", self.network, check=False).returncode != 0:
            self.docker("network", "create", self.network)
        args = [
            "run",
            "--detach",
            "--name",
            self.container,
            "--network",
            self.network,
            "--env",
            f"HOLON_CONTROL_TOKEN={self.token}",
            "--env",
            f"HOLON_MODEL={self.model}",
            "--env",
            "HOLON_DISABLE_PROVIDER_FALLBACK=true",
            "--publish",
            "127.0.0.1::7878",
            "--volume",
            f"{self.volume}:/var/lib/holon",
            "--volume",
            f"{self.workspace_parent}:/acceptance",
        ]
        for name in self.credential_envs:
            args.extend(["--env", name])
        for name, value in sorted(self.runtime_env.items()):
            args.extend(["--env", f"{name}={value}"])
        if self.env_file is not None:
            args.extend(["--env-file", str(self.env_file)])
        args.append(self.image)
        self.docker(*args)

        port = ""
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and not port:
            result = self.docker("port", self.container, "7878/tcp", check=False)
            if result.returncode == 0:
                lines = result.stdout.strip().splitlines()
                if lines:
                    port = lines[0].rsplit(":", 1)[-1]
            if not port:
                state = self.docker(
                    "inspect",
                    "--format",
                    "{{.State.Running}}",
                    self.container,
                    check=False,
                )
                if state.returncode == 0 and state.stdout.strip() == "false":
                    logs = self.docker("logs", self.container, check=False)
                    detail = (logs.stdout + logs.stderr).strip()
                    raise AssertionError(
                        "Holon container exited before publishing its port"
                        + (f": {detail}" if detail else "")
                    )
                time.sleep(0.25)
        require(bool(port), "failed to resolve the container's published port")
        self.base_url = f"http://127.0.0.1:{port}"
        self.wait_readiness()
        if wait_idle:
            self.wait_agent_idle()

    def stop(self) -> None:
        shutdown_error = ""
        if self.base_url:
            try:
                self.request("POST", "/api/control/runtime/shutdown", {})
            except Exception as error:
                shutdown_error = str(error)

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            state = self.docker(
                "inspect",
                "--format",
                "{{.State.Running}}",
                self.container,
                check=False,
            )
            if state.returncode != 0 or state.stdout.strip() == "false":
                break
            time.sleep(0.25)

        state = self.docker(
            "inspect",
            "--format",
            "{{.State.Running}}",
            self.container,
            check=False,
        )
        require(
            state.returncode != 0 or state.stdout.strip() == "false",
            "Holon container did not stop after the graceful shutdown request"
            + (f": {shutdown_error}" if shutdown_error else ""),
        )
        self.capture_logs()
        self.docker("rm", "-f", self.container, check=False)
        self.base_url = ""

    def restart(self, *, wait_idle: bool = True) -> None:
        self.stop()
        self.start(wait_idle=wait_idle)

    def reset_callback(self, label: str) -> dict[str, Any]:
        value = self.request(
            "POST",
            self.agent_path("reset-callback", control=True),
        )
        write_json(self.evidence / f"{label}.json", value)
        return value

    def fire_callback(
        self, label: str, trigger_url: str, body: dict[str, Any]
    ) -> dict[str, Any]:
        path = urllib.parse.urlparse(trigger_url).path
        require(path.startswith("/api/callbacks/wake/"), "unexpected callback path")
        value = self.request(
            "POST",
            path,
            body,
            authenticated=False,
        )
        write_json(self.evidence / f"{label}.json", value)
        return value

    def cleanup(self) -> dict[str, Any]:
        if self.keep:
            print(
                f"Keeping container resources for {self.case_id}: "
                f"container={self.container} volume={self.volume} network={self.network}",
                file=sys.stderr,
            )
            return {"status": "retained", "errors": []}
        errors: list[str] = []
        self.docker("rm", "-f", self.container, check=False)
        self.docker("volume", "rm", self.volume, check=False)
        self.docker("network", "rm", self.network, check=False)
        residuals = [
            (
                "container",
                self.container,
                self.docker("inspect", self.container, check=False),
            ),
            (
                "volume",
                self.volume,
                self.docker("volume", "inspect", self.volume, check=False),
            ),
            (
                "network",
                self.network,
                self.docker("network", "inspect", self.network, check=False),
            ),
        ]
        for kind, name, result in residuals:
            if result.returncode == 0:
                errors.append(f"{kind} still exists after cleanup: {name}")
        return {"status": "fail" if errors else "completed", "errors": errors}

    def capture_logs(self) -> None:
        result = self.docker("logs", self.container, check=False)
        self.log_index += 1
        path = self.evidence / f"container-{self.log_index}.log"
        path.write_text(redact_evidence(result.stdout + result.stderr))

    def request(
        self,
        method: str,
        path: str,
        body: Any | None = None,
        *,
        expected_status: int = 200,
        authenticated: bool = True,
    ) -> Any:
        data = None
        headers = {"Accept": "application/json"}
        if authenticated:
            headers["Authorization"] = f"Bearer {self.token}"
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=data,
            headers=headers,
            method=method,
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                status = response.status
                payload = response.read()
        except urllib.error.HTTPError as error:
            status = error.code
            payload = error.read()
        if status != expected_status:
            raise AssertionError(
                f"{method} {path} returned {status}, expected {expected_status}: "
                f"{payload.decode(errors='replace')}"
            )
        if not payload.strip():
            return None
        return json.loads(payload)

    def agent_path(self, suffix: str, *, control: bool = False) -> str:
        require(bool(self.agent_id), "default agent id is unavailable")
        agent_id = urllib.parse.quote(self.agent_id, safe="")
        prefix = "/api/control/agents" if control else "/api/agents"
        return f"{prefix}/{agent_id}/{suffix}"

    def wait_readiness(self) -> None:
        deadline = time.monotonic() + 90
        last_error = ""
        while time.monotonic() < deadline:
            running = self.docker(
                "inspect",
                "--format",
                "{{.State.Running}}",
                self.container,
                check=False,
            )
            require(
                running.returncode == 0 and running.stdout.strip() == "true",
                f"container exited before readiness; see {self.evidence}",
            )
            try:
                readiness = self.request("GET", "/api/control/runtime/readiness")
                agent_id = readiness["startup_surface"]["default_agent_id"]
                require(
                    isinstance(agent_id, str) and agent_id,
                    f"readiness response omitted default_agent_id: {readiness}",
                )
                self.agent_id = agent_id
                return
            except Exception as error:  # readiness is intentionally polled
                last_error = str(error)
                time.sleep(1)
        self.capture_logs()
        raise TimeoutError(f"Holon did not become ready: {last_error}")

    def wait_agent_idle(self) -> None:
        deadline = time.monotonic() + 90
        last_state: dict[str, Any] | None = None
        while time.monotonic() < deadline:
            last_state = self.request("GET", self.agent_path("state"))
            agent = last_state["agent"]["agent"]
            if (
                agent["status"] in TERMINAL_STATUSES
                and agent.get("current_run_id") is None
                and int(last_state["session"]["pending_count"]) == 0
            ):
                return
            time.sleep(1)
        write_json(self.evidence / "startup-idle-timeout-state.json", last_state)
        self.capture_logs()
        raise TimeoutError("default agent did not become idle after readiness")

    def state(self, label: str) -> dict[str, Any]:
        value = self.request("GET", self.agent_path("state"))
        write_json(self.evidence / f"{label}-state.json", value)
        return value

    def work_items(self, label: str) -> list[dict[str, Any]]:
        value = self.request("GET", self.agent_path("work-items?limit=50"))
        write_json(self.evidence / f"{label}-work-items.json", value)
        return value

    def wait_work_item(
        self,
        *,
        objective_marker: str,
        expected_state: str,
        label: str,
    ) -> dict[str, Any]:
        deadline = time.monotonic() + self.timeout_seconds
        matches: list[dict[str, Any]] = []
        while time.monotonic() < deadline:
            items = self.request("GET", self.agent_path("work-items?limit=50"))
            matches = [
                item for item in items if objective_marker in item.get("objective", "")
            ]
            if len(matches) == 1 and matches[0].get("state") == expected_state:
                write_json(self.evidence / f"{label}-work-items.json", items)
                self.wait_agent_idle()
                return matches[0]
            time.sleep(1)
        write_json(self.evidence / f"{label}-timeout-work-items.json", matches)
        self.capture_context(f"{label}-timeout")
        raise TimeoutError(
            f"timed out waiting for WorkItem {objective_marker} to reach {expected_state}"
        )

    def wait_work_item_scheduling_state(
        self,
        *,
        objective_marker: str,
        expected_scheduling_state: str,
        label: str,
    ) -> dict[str, Any]:
        deadline = time.monotonic() + self.timeout_seconds
        matches: list[dict[str, Any]] = []
        while time.monotonic() < deadline:
            items = self.request("GET", self.agent_path("work-items?limit=50"))
            matches = [
                item for item in items if objective_marker in item.get("objective", "")
            ]
            if (
                len(matches) == 1
                and matches[0].get("scheduling_state")
                == expected_scheduling_state
            ):
                write_json(self.evidence / f"{label}-work-items.json", items)
                self.wait_agent_idle()
                return matches[0]
            time.sleep(1)
        write_json(self.evidence / f"{label}-timeout-work-items.json", matches)
        self.capture_context(f"{label}-timeout")
        raise TimeoutError(
            f"timed out waiting for WorkItem {objective_marker} to reach "
            f"{expected_scheduling_state}"
        )

    def brief(self, brief_id: str, label: str) -> dict[str, Any]:
        encoded_id = urllib.parse.quote(brief_id, safe="")
        value = self.request("GET", self.agent_path(f"briefs/{encoded_id}"))
        write_json(self.evidence / f"{label}-brief.json", value)
        return value

    def events(self, label: str) -> list[dict[str, Any]]:
        page = self.request("GET", self.agent_path("events?limit=500&order=asc"))
        write_json(self.evidence / f"{label}-events.json", page)
        return page["events"]

    def capture_context(self, label: str) -> None:
        write_json(
            self.evidence / f"{label}-briefs.json",
            self.request("GET", self.agent_path("briefs?limit=50")),
        )
        write_json(
            self.evidence / f"{label}-transcript.json",
            self.request("GET", self.agent_path("transcript?limit=200")),
        )
        self.state(label)
        self.work_items(label)
        self.events(label)

    def prompt(self, label: str, text: str) -> tuple[int, dict[str, Any]]:
        before = self.state(f"{label}-before")
        baseline = int(before["agent"]["agent"]["turn_index"])
        response = self.request(
            "POST",
            self.agent_path("prompt", control=True),
            {"text": text},
        )
        write_json(self.evidence / f"{label}-prompt-response.json", response)
        (self.evidence / f"{label}-prompt.txt").write_text(text + "\n")

        deadline = time.monotonic() + self.timeout_seconds
        last_state = before
        while time.monotonic() < deadline:
            last_state = self.request("GET", self.agent_path("state"))
            agent = last_state["agent"]["agent"]
            if (
                int(agent["turn_index"]) > baseline
                and agent["status"] in TERMINAL_STATUSES
                and agent.get("current_run_id") is None
                and int(last_state["session"]["pending_count"]) == 0
            ):
                write_json(self.evidence / f"{label}-after-state.json", last_state)
                self.capture_context(label)
                return baseline, last_state
            time.sleep(1)
        write_json(self.evidence / f"{label}-timeout-state.json", last_state)
        self.capture_logs()
        raise TimeoutError(
            f"timed out after {self.timeout_seconds}s waiting for phase {label}"
        )

    def successful_tool_events(
        self, label: str, baseline_turn: int
    ) -> list[dict[str, Any]]:
        events = self.events(f"{label}-tool-check")
        turn_indexes = {
            event["payload"].get("turn_id"): int(
                event["payload"].get("turn_index", 0)
            )
            for event in events
            if event["type"] == "turn_started"
        }
        runtime_failures = [
            event
            for event in events
            if event["type"] == "runtime_error"
            and turn_indexes.get(event["payload"].get("turn_id"), 0)
            > baseline_turn
        ]
        if runtime_failures:
            failure = runtime_failures[-1]["payload"]
            source_chain = failure.get("source_chain") or []
            detail = source_chain[0] if source_chain else failure.get("error", "unknown")
            raise AssertionError(
                f"runtime failure occurred in {label}: "
                f"{failure.get('domain', 'unknown')}: {detail}"
            )
        failures = [
            event
            for event in events
            if event["type"] == "tool_execution_failed"
            and int(event["payload"].get("turn_index", 0)) > baseline_turn
        ]
        require(not failures, f"tool failures occurred in {label}: {failures}")
        return [
            event
            for event in events
            if event["type"] == "tool_executed"
            and event["payload"].get("status") == "success"
            and int(event["payload"].get("turn_index", 0)) > baseline_turn
        ]

    def assert_tools(
        self,
        label: str,
        baseline_turn: int,
        expected: list[str],
        forbidden: list[str] | None = None,
    ) -> list[dict[str, Any]]:
        events = self.successful_tool_events(label, baseline_turn)
        actual = [event["payload"].get("tool_name") for event in events]
        missing = [name for name in expected if name not in actual]
        require(not missing, f"{label} missing successful tools {missing}; got {actual}")
        forbidden_actual = [name for name in (forbidden or []) if name in actual]
        require(
            not forbidden_actual,
            f"{label} used forbidden tools {forbidden_actual}; got {actual}",
        )
        return events

    def tool_detail(self, event: dict[str, Any], label: str) -> dict[str, Any]:
        execution_id = event["payload"]["tool_execution_id"]
        detail = self.request(
            "GET",
            self.agent_path(f"tool-executions/{execution_id}"),
        )
        write_json(self.evidence / f"{label}-{execution_id}.json", detail)
        return detail

    def agent_home_file(self, relative_path: str, label: str) -> dict[str, Any]:
        encoded_path = "/".join(
            urllib.parse.quote(part, safe="") for part in relative_path.split("/")
        )
        workspace_id = urllib.parse.quote(f"agent_home:{self.agent_id}", safe="")
        value = self.request(
            "GET",
            f"/api/workspaces/{workspace_id}/files/{encoded_path}",
        )
        write_json(self.evidence / f"{label}.json", value)
        return value

    def runtime_db_snapshot(self, label: str) -> dict[str, Any]:
        snapshot_dir = self.evidence / f"{label}-runtime-state"
        if snapshot_dir.exists():
            shutil.rmtree(snapshot_dir)
        snapshot_dir.mkdir(parents=True)
        self.docker(
            "cp",
            f"{self.container}:/var/lib/holon/state/.",
            str(snapshot_dir),
        )
        database = snapshot_dir / "runtime.sqlite"
        require(database.is_file(), "runtime database snapshot is missing")
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
        connection.row_factory = sqlite3.Row
        try:
            snapshot = {
                "work_items": sqlite_rows(
                    connection,
                    "SELECT work_item_id, agent_id, state, objective, revision, "
                    "current_focus, completed_at, payload_json "
                    "FROM work_items ORDER BY created_at",
                ),
                "messages": sqlite_rows(
                    connection,
                    "SELECT message_id, agent_id, turn_id, work_item_id, kind, "
                    "created_at, payload_json FROM messages ORDER BY created_at",
                ),
                "queue_entries": sqlite_rows(
                    connection,
                    "SELECT message_id, agent_id, priority, status, created_at, "
                    "updated_at, payload_json FROM queue_entries "
                    "ORDER BY created_at, updated_at",
                ),
                "turn_records": sqlite_rows(
                    connection,
                    "SELECT turn_id, turn_index, agent_id, run_id, "
                    "current_work_item_id, trigger_message_id, terminal_kind, "
                    "created_at, completed_at, payload_json "
                    "FROM turn_records ORDER BY turn_index, created_at",
                ),
                "audit_events": sqlite_rows(
                    connection,
                    "SELECT audit_event_id, event_seq, agent_id, kind, created_at, "
                    "data_json FROM audit_events ORDER BY event_seq",
                ),
                "briefs": sqlite_rows(
                    connection,
                    "SELECT evidence_id, agent_id, turn_id, message_id, task_id, "
                    "work_item_id, kind, preview, payload_json "
                    "FROM briefs ORDER BY created_at",
                ),
                "wait_conditions": sqlite_rows(
                    connection,
                    "SELECT wait_condition_id, agent_id, work_item_id, status, kind, "
                    "subject_ref, waiting_for, last_turn_id, payload_json "
                    "FROM wait_conditions ORDER BY created_at",
                ),
                "scheduler_protocol_config": sqlite_rows(
                    connection,
                    "SELECT protocol_mode, config_revision, latest_preflight_revision "
                    "FROM scheduler_protocol_config",
                ),
                "scheduler_rollout_preflights": sqlite_rows(
                    connection,
                    "SELECT preflight_revision, manifest_revision, state, manifest_json "
                    "FROM scheduler_rollout_preflights ORDER BY preflight_revision",
                ),
                "scheduler_rollout_manifests": sqlite_rows(
                    connection,
                    "SELECT manifest_revision, preflight_revision, payload_json "
                    "FROM scheduler_rollout_manifests ORDER BY manifest_revision",
                ),
                "scheduler_scenario_authorities": sqlite_rows(
                    connection,
                    "SELECT scenario_class, mode, rollback_target, manifest_revision, "
                    "preflight_revision FROM scheduler_scenario_authorities "
                    "ORDER BY scenario_class",
                ),
                "scheduler_rollout_command_results": sqlite_rows(
                    connection,
                    "SELECT command_kind, command_identity, decision, conflict_kind, "
                    "conflict_code, result_references_json, pre_state_fence_json, "
                    "post_state_fence_json FROM scheduler_rollout_command_results "
                    "ORDER BY created_at",
                ),
                "scheduler_work_demands": sqlite_rows(
                    connection,
                    "SELECT agent_id, work_item_id, scheduling_generation, status, "
                    "status_reference_id, payload_json FROM scheduler_work_demands",
                ),
                "scheduler_agent_slots": sqlite_rows(
                    connection,
                    "SELECT agent_id, slot_kind, activation_id, work_item_id, "
                    "admitted_generation FROM scheduler_agent_slots",
                ),
                "scheduler_activations": sqlite_rows(
                    connection,
                    "SELECT agent_id, activation_id, authority_id, work_item_id, "
                    "admitted_generation, admission_kind, lifecycle_state, "
                    "idempotency_key, payload_json FROM scheduler_activations",
                ),
                "scheduler_activation_settlements": sqlite_rows(
                    connection,
                    "SELECT agent_id, settlement_id, activation_id, payload_json "
                    "FROM scheduler_activation_settlements",
                ),
                "scheduler_wait_generations": sqlite_rows(
                    connection,
                    "SELECT agent_id, wait_id, generation, owner_work_item_id, "
                    "lifecycle_state, trigger_id, trigger_generation, "
                    "consuming_activation_id, payload_json "
                    "FROM scheduler_wait_generations ORDER BY wait_id, generation",
                ),
                "scheduler_missing_settlements": sqlite_rows(
                    connection,
                    "SELECT agent_id, missing_settlement_id, activation_id, payload_json "
                    "FROM scheduler_missing_settlements",
                ),
                "scheduler_protocol_command_results": sqlite_rows(
                    connection,
                    "SELECT agent_id, command_kind, command_identity, decision, "
                    "conflict_kind, conflict_code, result_references_json, "
                    "pre_state_fence_json, post_state_fence_json "
                    "FROM scheduler_protocol_command_results ORDER BY created_at",
                ),
                "scheduler_shadow_comparisons": sqlite_rows(
                    connection,
                    "SELECT agent_id, scenario_class, boundary, comparison_identity, "
                    "comparison_outcome, authority_mode, input_identity "
                    "FROM scheduler_shadow_comparisons ORDER BY created_at",
                ),
            }
        finally:
            connection.close()
        write_json(self.evidence / f"{label}-runtime-db.json", snapshot)
        return snapshot


def result_value(detail: dict[str, Any]) -> dict[str, Any]:
    output = detail.get("output", {})
    return output.get("envelope", {}).get("result", output.get("result", output))


def sqlite_rows(connection: sqlite3.Connection, query: str) -> list[dict[str, Any]]:
    return [dict(row) for row in connection.execute(query).fetchall()]


def require_processed_queue_entries(
    queue_entries: list[dict[str, Any]], message_ids: set[str]
) -> None:
    matching = [
        row for row in queue_entries if row.get("message_id") in message_ids
    ]
    require(
        len(matching) == len(message_ids)
        and all(row.get("status") == "processed" for row in matching),
        f"work_queue messages did not reach processed current state: {matching}",
    )


SCHEDULER_ROLLOUT_GATES = {
    "reducer_only_candidates": (
        10_000,
        72 * 60 * 60,
        ["deterministic_replay", "duplicate_command_idempotency"],
    ),
    "exact_task_rejoin": (
        1_000,
        7 * 24 * 60 * 60,
        [
            "duplicate_task_result",
            "out_of_order_task_result",
            "restart_before_rejoin_settlement",
        ],
    ),
    "exact_wait_resume": (
        1_000,
        7 * 24 * 60 * 60,
        ["duplicate_trigger", "stale_generation", "restart_after_consume", "rearm"],
    ),
    "work_item_autonomous_continuation": (
        2_000,
        14 * 24 * 60 * 60,
        [
            "concurrent_claim",
            "reservation_conflict",
            "yield_return",
            "work_item_rollback",
        ],
    ),
    "settlement": (
        1_000,
        7 * 24 * 60 * 60,
        [
            "duplicate_settlement",
            "missing_settlement_recovery",
            "restart_before_settlement_commit",
        ],
    ),
    "delivery": (
        1_000,
        7 * 24 * 60 * 60,
        ["duplicate_delivery", "delivery_retry", "restart_before_delivery_commit"],
    ),
}


def scheduler_rollout_manifest(
    scenario_modes: dict[str, str],
    *,
    revision: int = 1,
    preflight_revision: int = 1,
) -> dict[str, Any]:
    # Explicit acceptance-only synthetic data: exercise the production reducer
    # and revision fences without claiming fleet shadow evidence collection.
    universal = {"restart", "fault_injection", "rollback_drill"}
    classes = {}
    for scenario_class, configured_mode in scenario_modes.items():
        minimum_samples, minimum_duration, class_evidence = SCHEDULER_ROLLOUT_GATES[
            scenario_class
        ]
        evidence = sorted(universal.union(class_evidence))
        classes[scenario_class] = {
            "configured_mode": configured_mode,
            "minimum_shadow_samples": minimum_samples,
            "minimum_shadow_duration_secs": minimum_duration,
            "observed_shadow_samples": minimum_samples,
            "observed_shadow_duration_secs": minimum_duration,
            "maximum_p99_latency_regression_bps": 500,
            "observed_p99_latency_regression_bps": 0,
            "hard_blocker_count": 0,
            "unresolved_divergence_count": 0,
            "required_evidence": evidence,
            "verified_evidence": evidence,
            "rollback_policy": {
                "trigger": "any_hard_blocker",
                "action": {
                    "kind": "stop_admissions_and_revert",
                    "target": "shadow",
                },
            },
        }
    return {
        "revision": revision,
        "preflight_revision": preflight_revision,
        "preflight_for_manifest_revision": revision,
        "preflight_succeeded": True,
        "protocol_build": "holon-docker-e2e-synthetic-acceptance",
        "schema_build": "scheduler-protocol-schema-v1",
        "schema_revision": 1,
        "fixture_corpus_revision": "scheduler-pr-g-synthetic-acceptance-v1",
        "classes": classes,
        "safety_divergence_bps": 0,
        "canonical_state_divergence_bps": 0,
        "allowed_observational_divergence": {
            "diagnostic_order": {
                "maximum_rate_bps": 0,
                "reviewed_by": "docker-e2e",
            }
        },
        "approver": "docker-e2e-synthetic-acceptance",
        "approved_at": "2026-07-23T00:00:00Z",
    }


def scheduler_incomplete_rollout_commands(
    scenario_modes: dict[str, str],
) -> list[dict[str, Any]]:
    manifest = scheduler_rollout_manifest(scenario_modes)
    for evidence in manifest["classes"].values():
        evidence["observed_shadow_samples"] = 0
        evidence["observed_shadow_duration_secs"] = 0
        evidence["verified_evidence"] = []
    return [
        {
            "command_identity": "docker-e2e-rejected-rollout-open",
            "command": {
                "kind": "open_preflight",
                "expected_config_revision": 0,
                "manifest_revision": 1,
            },
        },
        {
            "command_identity": "docker-e2e-rejected-rollout-complete",
            "command": {
                "kind": "complete_preflight",
                "expected_config_revision": 0,
                "expected_preflight_revision": 1,
                "manifest": manifest,
            },
        },
    ]


def scheduler_rollout_commands(
    scenario_modes: dict[str, str],
    *,
    approved_scenario_modes: dict[str, str] | None = None,
    rollback_scenario: str | None = None,
) -> list[dict[str, Any]]:
    manifest = scheduler_rollout_manifest(
        approved_scenario_modes or scenario_modes
    )
    commands = [
        {
            "command_identity": "docker-e2e-rollout-open",
            "command": {
                "kind": "open_preflight",
                "expected_config_revision": 0,
                "manifest_revision": 1,
            },
        },
        {
            "command_identity": "docker-e2e-rollout-complete",
            "command": {
                "kind": "complete_preflight",
                "expected_config_revision": 0,
                "expected_preflight_revision": 1,
                "manifest": manifest,
            },
        },
        {
            "command_identity": "docker-e2e-rollout-install",
            "command": {
                "kind": "install_manifest",
                "expected_config_revision": 0,
                "manifest": manifest,
            },
        },
        {
            "command_identity": "docker-e2e-rollout-configure",
            "command": {
                "kind": "configure_protocol",
                "expected_config_revision": 1,
                "mode": "authoritative",
            },
        },
    ]
    revision = 2
    for scenario_class, target_mode in scenario_modes.items():
        if target_mode == "off":
            continue
        commands.append(
            {
                "command_identity": f"docker-e2e-{scenario_class}-shadow",
                "command": {
                    "kind": "change_scenario_authority",
                    "scenario_class": scenario_class,
                    "expected_config_revision": revision,
                    "expected_manifest_revision": 1,
                    "expected_preflight_revision": 1,
                    "mode": "shadow",
                },
            }
        )
        revision += 1
        if target_mode == "authoritative":
            commands.append(
                {
                    "command_identity": f"docker-e2e-{scenario_class}-authoritative",
                    "command": {
                        "kind": "change_scenario_authority",
                        "scenario_class": scenario_class,
                        "expected_config_revision": revision,
                        "expected_manifest_revision": 1,
                        "expected_preflight_revision": 1,
                        "mode": "authoritative",
                    },
                }
            )
            revision += 1
    if rollback_scenario is not None:
        commands.append(
            {
                "command_identity": f"docker-e2e-{rollback_scenario}-rollback",
                "command": {
                    "kind": "change_scenario_authority",
                    "scenario_class": rollback_scenario,
                    "expected_config_revision": revision,
                    "expected_manifest_revision": 1,
                    "expected_preflight_revision": 1,
                    "mode": "shadow",
                },
            }
        )
    return commands


def scheduler_scenario_mode_commands(
    scenario_modes: dict[str, str],
    *,
    expected_config_revision: int,
    identity_suffix: str,
) -> list[dict[str, Any]]:
    commands = []
    revision = expected_config_revision
    for scenario_class, mode in scenario_modes.items():
        commands.append(
            {
                "command_identity": (
                    f"docker-e2e-{scenario_class}-{mode}-{identity_suffix}"
                ),
                "command": {
                    "kind": "change_scenario_authority",
                    "scenario_class": scenario_class,
                    "expected_config_revision": revision,
                    "expected_manifest_revision": 1,
                    "expected_preflight_revision": 1,
                    "mode": mode,
                },
            }
        )
        revision += 1
    return commands


def require_rollout_state(
    snapshot: dict[str, Any],
    expected_modes: dict[str, str],
) -> None:
    require(
        snapshot["scheduler_protocol_config"]
        and snapshot["scheduler_protocol_config"][0]["protocol_mode"]
        == "authoritative",
        f"scheduler protocol ceiling is not authoritative: "
        f"{snapshot['scheduler_protocol_config']}",
    )
    preflights = snapshot["scheduler_rollout_preflights"]
    require(
        len(preflights) == 1
        and preflights[0]["preflight_revision"] == 1
        and preflights[0]["manifest_revision"] == 1
        and preflights[0]["state"] == "consumed",
        f"rollout preflight was not consumed exactly once: {preflights}",
    )
    require(
        len(snapshot["scheduler_rollout_manifests"]) == 1
        and snapshot["scheduler_rollout_manifests"][0]["manifest_revision"] == 1,
        f"rollout manifest is missing or duplicated: "
        f"{snapshot['scheduler_rollout_manifests']}",
    )
    authorities = {
        row["scenario_class"]: row for row in snapshot["scheduler_scenario_authorities"]
    }
    for scenario_class, mode in expected_modes.items():
        require(
            authorities.get(scenario_class, {}).get("mode") == mode,
            f"scenario {scenario_class} did not reach {mode}: {authorities}",
        )
        row = authorities[scenario_class]
        if mode == "authoritative":
            require(
                row["manifest_revision"] == 1 and row["preflight_revision"] == 1,
                f"authoritative scenario lacks rollout fence: {row}",
            )
        else:
            require(
                row["manifest_revision"] is None
                and row["preflight_revision"] is None,
                f"non-authoritative scenario retained authority fence: {row}",
            )
    require(
        all(
            row["conflict_kind"] is None
            for row in snapshot["scheduler_rollout_command_results"]
        ),
        f"rollout command chain contains conflicts: "
        f"{snapshot['scheduler_rollout_command_results']}",
    )


def require_turns_terminal(
    snapshot: dict[str, Any], message_ids: set[str]
) -> list[dict[str, Any]]:
    turns = [
        row
        for row in snapshot["turn_records"]
        if row.get("trigger_message_id") in message_ids
    ]
    require(
        len(turns) == len(message_ids)
        and all(row.get("terminal_kind") == "completed" for row in turns),
        f"scheduler turns did not reach completed terminal state: {turns}",
    )
    return turns


def require_scheduler_activation_chain(
    snapshot: dict[str, Any],
    *,
    agent_id: str,
    work_item_id: str,
    expected_activation_count: int,
) -> list[dict[str, Any]]:
    activations = [
        row
        for row in snapshot["scheduler_activations"]
        if row["work_item_id"] == work_item_id
    ]
    require(
        len(activations) == expected_activation_count
        and all(row["lifecycle_state"] == "settled" for row in activations),
        f"canonical activations did not settle exactly once: {activations}",
    )
    activation_ids = {row["activation_id"] for row in activations}
    settlements = [
        row
        for row in snapshot["scheduler_activation_settlements"]
        if row["activation_id"] in activation_ids
    ]
    require(
        len(settlements) == expected_activation_count,
        f"canonical settlements are missing or duplicated: {settlements}",
    )
    require(
        not [
            row
            for row in snapshot["scheduler_missing_settlements"]
            if row["activation_id"] in activation_ids
        ],
        "canonical activation chain retained missing settlement evidence",
    )
    slots = [
        row for row in snapshot["scheduler_agent_slots"] if row["agent_id"] == agent_id
    ]
    require(
        len(slots) == 1
        and slots[0]["slot_kind"] == "idle"
        and slots[0]["activation_id"] is None,
        f"canonical activation slot was not released: {slots}",
    )
    return activations


def require_scheduler_comparisons(
    snapshot: dict[str, Any],
    expected: dict[str, int],
) -> None:
    for scenario_class, minimum_count in expected.items():
        rows = [
            row
            for row in snapshot["scheduler_shadow_comparisons"]
            if row["scenario_class"] == scenario_class
        ]
        require(
            len(rows) >= minimum_count
            and all(row["comparison_outcome"] == "matched" for row in rows),
            f"scheduler comparison evidence missing or divergent for "
            f"{scenario_class}: {rows}",
        )


def find_case(manifest: dict[str, Any], case_id: str) -> dict[str, Any]:
    for case in manifest["cases"]:
        if case["id"] == case_id:
            return case
    raise KeyError(case_id)


def phase_tools(phase: dict[str, Any]) -> tuple[list[str], list[str]]:
    required = phase.get("required_tools", phase.get("expected_tools", []))
    return list(required), list(phase.get("forbidden_tools", []))


def run_runtime_case(harness: CaseHarness, case: dict[str, Any]) -> None:
    harness.initialize_workspace()
    harness.start()
    unauthorized = harness.request(
        "GET",
        "/api/control/runtime/readiness",
        expected_status=403,
        authenticated=False,
    )
    write_json(harness.evidence / "unauthorized-readiness.json", unauthorized)

    readiness = harness.request("GET", "/api/control/runtime/readiness")
    write_json(harness.evidence / "readiness.json", readiness)
    runtime_surface = readiness["runtime_surface"]
    require(
        normalize_model_route(runtime_surface["model_default"])
        == normalize_model_route(harness.model),
        f"runtime model route mismatch: {runtime_surface['model_default']}",
    )
    require(
        runtime_surface["disable_provider_fallback"] is True,
        "provider fallback must be disabled for release E2E",
    )

    phase = case["phases"][0]
    marker = f"RUNTIME-DELIVERY-{secrets.token_hex(6)}"
    baseline, _ = harness.prompt(
        "runtime-delivery",
        phase["prompt"].format(
            case_id=case["id"],
            provider=harness.model.split("/", 1)[0],
            marker=marker,
        ),
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools("runtime-delivery", baseline, required, forbidden)

    events = harness.events("runtime-delivery-assert")
    provider_events = [
        event
        for event in events
        if event["type"] == "provider_round_completed"
        and int(event["payload"].get("turn_index", 0)) > baseline
    ]
    require(provider_events, "provider_round_completed event is missing")
    provider = provider_events[-1]["payload"]
    timeline = provider.get("provider_attempt_timeline") or {}
    attempts = timeline.get("attempts") or []
    require(len(attempts) == 1, f"expected one provider attempt: {timeline}")
    require(
        provider.get("fallback_active") is False,
        f"provider fallback unexpectedly activated: {provider}",
    )
    require(
        (provider.get("token_usage") or {}).get("total_tokens", 0) > 0,
        f"provider token usage is missing: {provider}",
    )
    winning = timeline.get("winning_model_ref")
    require(
        normalize_model_route(str(winning)) == normalize_model_route(harness.model),
        f"winning model {winning!r} did not match {harness.model!r}",
    )

    briefs = harness.request("GET", harness.agent_path("briefs?limit=20"))
    write_json(harness.evidence / "runtime-delivery-briefs.json", briefs)
    brief_rows = briefs if isinstance(briefs, list) else briefs.get("briefs", [])
    matching = [
        brief
        for brief in brief_rows
        if marker in (brief.get("text") or "")
        and int(brief.get("turn_index") or 0) > baseline
    ]
    require(len(matching) == 1, f"expected one marker brief: {matching}")

    transcript = harness.request(
        "GET", harness.agent_path("transcript?limit=200")
    )
    write_json(harness.evidence / "runtime-delivery-transcript.json", transcript)
    entries = transcript if isinstance(transcript, list) else transcript.get("entries", [])
    assistant_rounds = [
        entry
        for entry in entries
        if entry.get("kind") == "assistant_round"
        and marker in json.dumps(entry, ensure_ascii=False)
    ]
    require(assistant_rounds, "marker assistant round is missing from transcript")


def run_memory_case(harness: CaseHarness, case: dict[str, Any]) -> None:
    harness.initialize_workspace()
    harness.start()
    marker = f"MEMORY-PERSISTENCE-{secrets.token_hex(6)}-记忆"
    memory_path = f"/var/lib/holon/agents/{harness.agent_id}/memory/self.md"
    harness.docker(
        "exec",
        harness.container,
        "bash",
        "-lc",
        f"set -euo pipefail; printf '\\n%s\\n' {json.dumps(marker, ensure_ascii=False)} >> "
        f"{json.dumps(memory_path)}",
    )

    first_phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "memory-search",
        first_phase["prompt"].format(case_id=case["id"], marker=marker),
    )
    required, forbidden = phase_tools(first_phase)
    events = harness.assert_tools("memory-search", baseline, required, forbidden)
    search_event = next(
        event
        for event in events
        if event["payload"].get("tool_name") == "MemorySearch"
    )
    search_result = result_value(harness.tool_detail(search_event, "memory-search"))
    matches = [
        result
        for result in search_result.get("results", [])
        if marker in json.dumps(result, ensure_ascii=False)
    ]
    require(matches, f"MemorySearch did not return marker {marker}: {search_result}")
    source_ref = matches[0].get("source_ref")
    require(
        isinstance(source_ref, str) and source_ref,
        f"MemorySearch result omitted source_ref: {matches[0]}",
    )
    get_event = next(
        event
        for event in events
        if event["payload"].get("tool_name") == "MemoryGet"
    )
    get_result = memory_value(
        result_value(harness.tool_detail(get_event, "memory-get"))
    )
    require(
        get_result.get("source_ref") == source_ref,
        f"MemoryGet used an unexpected source_ref: {get_result}",
    )
    require(
        marker in (get_result.get("content") or ""),
        f"MemoryGet omitted marker {marker}: {get_result}",
    )

    harness.restart()
    second_phase = case["phases"][1]
    baseline, _ = harness.prompt(
        "memory-recover",
        second_phase["prompt"].format(
            case_id=case["id"],
            marker=marker,
            source_ref=source_ref,
        ),
    )
    required, forbidden = phase_tools(second_phase)
    events = harness.assert_tools("memory-recover", baseline, required, forbidden)
    recovered_get = next(
        event
        for event in events
        if event["payload"].get("tool_name") == "MemoryGet"
    )
    recovered = memory_value(
        result_value(harness.tool_detail(recovered_get, "memory-recover-get"))
    )
    require(
        recovered.get("source_ref") == source_ref
        and marker in (recovered.get("content") or ""),
        f"memory source did not survive restart: {recovered}",
    )


def run_workspace_case(harness: CaseHarness, case: dict[str, Any]) -> None:
    harness.initialize_workspace()
    harness.start()
    attached = harness.request(
        "POST",
        harness.agent_path("workspace/attach", control=True),
        {"path": "/acceptance/repo"},
    )
    write_json(harness.evidence / "workspace-attach.json", attached)
    workspace_id = attached["workspace_id"]
    branch = f"live-acceptance-{secrets.token_hex(4)}"

    create_phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "workspace-create",
        create_phase["prompt"].format(
            case_id=case["id"],
            workspace_id=workspace_id,
            branch=branch,
        ),
    )
    required, forbidden = phase_tools(create_phase)
    create_events = harness.assert_tools(
        "workspace-create", baseline, required, forbidden
    )
    create_event = next(
        event
        for event in create_events
        if event["payload"].get("tool_name") == "CreateWorktree"
    )
    create_detail = harness.tool_detail(create_event, "workspace-create")
    created = result_value(create_detail)
    execution_root_id = created.get("execution_root_id")
    require(
        isinstance(execution_root_id, str) and execution_root_id,
        f"CreateWorktree result missing execution_root_id: {created}",
    )

    harness.restart()
    recovered_state = harness.state("workspace-after-restart")
    require(
        workspace_id
        in recovered_state["agent"]["agent"].get("attached_workspaces", []),
        "attached canonical workspace did not survive service restart",
    )

    recover_phase = case["phases"][1]
    baseline, final_state = harness.prompt(
        "workspace-recover",
        recover_phase["prompt"].format(
            case_id=case["id"],
            workspace_id=workspace_id,
            execution_root_id=execution_root_id,
        ),
    )
    required, forbidden = phase_tools(recover_phase)
    harness.assert_tools("workspace-recover", baseline, required, forbidden)
    git_state = harness.docker(
        "exec",
        harness.container,
        "bash",
        "-lc",
        "set -euo pipefail; "
        "git -C /acceptance/repo status --porcelain; "
        "printf '%s\\n' '--- worktrees ---'; "
        "git -C /acceptance/repo worktree list --porcelain",
    ).stdout
    (harness.evidence / "workspace-final-git.txt").write_text(git_state)
    status, worktrees = git_state.split("--- worktrees ---\n", 1)
    require(not status.strip(), f"canonical repository is dirty:\n{status}")
    require(
        worktrees.count("worktree ") == 1,
        f"managed worktree was not removed cleanly:\n{worktrees}",
    )
    active = final_state["workspace"]["workspaces"][0]
    require(
        active["is_active"] and active["workspace_id"] == workspace_id,
        f"canonical workspace was not active after cleanup: {active}",
    )
    require(
        active.get("worktree") is None,
        f"active workspace still reports a worktree after cleanup: {active}",
    )


def run_workitem_case(harness: CaseHarness, case: dict[str, Any]) -> None:
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective = f"live-workitem-{marker}"
    plan_marker = f"LIVE-WORKITEM-CHECKPOINT-{marker}"
    completion_marker = f"LIVE-WORKITEM-COMPLETE-{marker}"

    wait_phase = case["phases"][0]
    baseline, state = harness.prompt(
        "workitem-wait",
        wait_phase["prompt"].format(
            case_id=case["id"],
            objective=objective,
            plan_marker=plan_marker,
        ),
    )
    required, forbidden = phase_tools(wait_phase)
    harness.assert_tools("workitem-wait", baseline, required, forbidden)
    items = harness.work_items("workitem-wait-assert")
    matches = [item for item in items if item["objective"] == objective]
    require(len(matches) == 1, f"expected exactly one matching WorkItem: {matches}")
    item = matches[0]
    work_item_id = item["id"]
    require(item["state"] == "open", f"WorkItem should remain open: {item}")
    require(
        item["plan_status"] == "needs_input",
        f"WorkItem should need input: {item}",
    )
    require(
        item["readiness"] == "waiting_for_operator",
        f"WorkItem should wait for operator: {item}",
    )
    require(
        [(todo["text"], todo["state"]) for todo in item.get("todo_list", [])]
        == [
            ("phase-one", "completed"),
            ("phase-two", "in_progress"),
            ("phase-three", "pending"),
        ],
        f"WorkItem todos do not match the checked-in case: {item}",
    )
    require(
        state["agent"]["agent"].get("current_work_item_id") is None,
        "waiting WorkItem should release current focus after WaitFor",
    )
    require(
        item.get("has_active_waits") is True,
        f"WorkItem should retain an active operator wait: {item}",
    )
    plan = harness.agent_home_file(
        f"work-items/{work_item_id}/plan.md", "workitem-plan"
    )
    require(
        plan_marker in plan.get("content", ""),
        "WorkItem plan artifact did not preserve the required marker",
    )

    harness.restart()
    restart_state = harness.state("workitem-after-restart")
    restart_items = harness.work_items("workitem-after-restart")
    restored = next(item for item in restart_items if item["id"] == work_item_id)
    require(restored["state"] == "open", "WorkItem was not restored as open")
    require(
        restored["readiness"] == "waiting_for_operator",
        f"WorkItem wait did not survive restart: {restored}",
    )
    require(
        restart_state["agent"]["agent"].get("current_work_item_id") is None,
        "blocked WorkItem should not become current merely because of restart",
    )
    require(
        restored.get("has_active_waits") is True,
        f"WorkItem operator wait did not survive restart: {restored}",
    )

    complete_phase = case["phases"][1]
    baseline, _ = harness.prompt(
        "workitem-complete",
        complete_phase["prompt"].format(
            case_id=case["id"],
            work_item_id=work_item_id,
            plan_marker=plan_marker,
            completion_marker=completion_marker,
        ),
    )
    required, forbidden = phase_tools(complete_phase)
    harness.assert_tools("workitem-complete", baseline, required, forbidden)
    final_items = harness.work_items("workitem-final")
    completed = next(item for item in final_items if item["id"] == work_item_id)
    require(completed["state"] == "completed", f"WorkItem not completed: {completed}")
    require(
        len(completed.get("todo_list", [])) == 3
        and all(
            todo["state"] == "completed"
            for todo in completed.get("todo_list", [])
        ),
        f"WorkItem todos were not all completed: {completed}",
    )
    result_brief_id = completed.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"completed WorkItem omitted result_brief_id: {completed}",
    )
    result_brief = harness.brief(result_brief_id, "workitem-result")
    require(
        result_brief.get("work_item_id") == work_item_id,
        f"completion brief is not linked to WorkItem {work_item_id}: {result_brief}",
    )
    require(
        completion_marker in (result_brief.get("text") or ""),
        f"completion brief did not preserve marker {completion_marker}: {result_brief}",
    )


def run_scheduler_protocol_case(harness: CaseHarness, case: dict[str, Any]) -> None:
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_marker = f"SCHEDULER-AUTONOMOUS-{marker}"
    completion_marker = f"SCHEDULER-COMPLETE-{marker}"
    objective = (
        f"{objective_marker}. Complete this WorkItem only after the Runtime resumes it "
        "through an autonomous work_queue SystemTick. On that autonomous turn, inspect "
        "the exact current item with ListWorkItems using filter current and optionally "
        "GetWorkItem, update both existing todos to completed, then emit a concise "
        f"completion result containing {completion_marker} immediately followed by "
        "CompleteWorkItem for that exact item. Do not wait for more operator input."
    )

    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-autonomous",
        phase["prompt"].format(
            case_id=case["id"],
            objective=json.dumps(objective, ensure_ascii=False),
            objective_marker=objective_marker,
            completion_marker=completion_marker,
        ),
    )
    item = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="scheduler-autonomous-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools("scheduler-autonomous", baseline, required, forbidden)

    items = harness.work_items("scheduler-autonomous-assert")
    matches = [item for item in items if objective_marker in item["objective"]]
    require(len(matches) == 1, f"expected one scheduler WorkItem: {matches}")
    require(
        matches[0]["id"] == item["id"],
        f"scheduler WorkItem identity changed during completion: {matches}",
    )
    item = matches[0]
    work_item_id = item["id"]
    require(item["state"] == "completed", f"scheduler WorkItem not completed: {item}")
    require(
        len(item.get("todo_list", [])) == 2
        and all(todo["state"] == "completed" for todo in item["todo_list"]),
        f"scheduler WorkItem todos were not completed: {item}",
    )
    result_brief_id = item.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"scheduler WorkItem omitted result brief: {item}",
    )
    result_brief = harness.brief(result_brief_id, "scheduler-result")
    require(
        result_brief.get("work_item_id") == work_item_id
        and completion_marker in (result_brief.get("text") or ""),
        f"scheduler completion brief mismatch: {result_brief}",
    )

    harness.restart()
    restarted_items = harness.work_items("scheduler-after-restart")
    restarted = next(item for item in restarted_items if item["id"] == work_item_id)
    require(
        restarted["state"] == "completed"
        and restarted.get("result_brief_id") == result_brief_id,
        f"scheduler WorkItem identity did not survive restart: {restarted}",
    )

    snapshot = harness.runtime_db_snapshot("scheduler")
    message_rows = [
        row
        for row in snapshot["messages"]
        if row.get("work_item_id") == work_item_id
        and "work_queue" in row.get("payload_json", "")
        and (
            "SystemTick" in row.get("payload_json", "")
            or "system_tick" in row.get("payload_json", "")
        )
    ]
    require(message_rows, "autonomous work_queue SystemTick evidence is missing")
    message_ids = {row["message_id"] for row in message_rows}
    # queue_entries is a message_id-keyed current-state view, so queued and
    # dequeued are intentionally overwritten by the terminal processed row.
    require_processed_queue_entries(snapshot["queue_entries"], message_ids)

    enabled = case.get("scheduler_protocol_commands_enabled")
    require(
        enabled in {True, False},
        "scheduler case must declare scheduler_protocol_commands_enabled",
    )
    demands = [
        row
        for row in snapshot["scheduler_work_demands"]
        if row["work_item_id"] == work_item_id
    ]
    activations = [
        row
        for row in snapshot["scheduler_activations"]
        if row["work_item_id"] == work_item_id
    ]
    activation_ids = {row["activation_id"] for row in activations}
    settlements = [
        row
        for row in snapshot["scheduler_activation_settlements"]
        if row["activation_id"] in activation_ids
    ]
    command_rows = [
        row
        for row in snapshot["scheduler_protocol_command_results"]
        if row["command_identity"] == work_item_id
        or any(
            message_id in row["command_identity"] for message_id in message_ids
        )
    ]

    if not enabled:
        require(not demands, f"legacy mode wrote canonical demand facts: {demands}")
        require(not activations, f"legacy mode wrote canonical activations: {activations}")
        require(not settlements, f"legacy mode wrote canonical settlements: {settlements}")
        require(not command_rows, f"legacy mode wrote protocol commands: {command_rows}")
        return

    require(
        len(demands) == 1 and demands[0]["status"] == "terminal",
        f"canonical demand did not settle terminal: {demands}",
    )
    require(
        len(activations) == 1 and activations[0]["lifecycle_state"] == "settled",
        f"canonical activation did not settle exactly once: {activations}",
    )
    require(
        len(settlements) == 1,
        f"canonical settlement is missing or duplicated: {settlements}",
    )
    slots = [
        row
        for row in snapshot["scheduler_agent_slots"]
        if row["agent_id"] == harness.agent_id
    ]
    require(
        len(slots) == 1
        and slots[0]["slot_kind"] == "idle"
        and slots[0]["activation_id"] is None,
        f"canonical activation slot was not released: {slots}",
    )
    command_kinds = {row["command_kind"] for row in command_rows}
    require(
        {
            "register_work_demand",
            "issue_activation_authority",
            "admit_activation",
            "settle_activation",
        }.issubset(command_kinds),
        f"canonical command chain is incomplete: {command_rows}",
    )
    require(
        all(row["conflict_kind"] is None for row in command_rows),
        f"canonical command chain contains conflicts: {command_rows}",
    )


def run_scheduler_rollout_authoritative_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    harness.initialize_workspace()
    scenario_modes = {
        "work_item_autonomous_continuation": "shadow",
        "exact_task_rejoin": "shadow",
        "exact_wait_resume": "shadow",
        "settlement": "shadow",
        "delivery": "shadow",
    }
    authoritative_modes = {
        scenario_class: "authoritative" for scenario_class in scenario_modes
    }
    rejected = harness.reject_scheduler_rollout(
        "scheduler-rollout-insufficient-evidence",
        scheduler_incomplete_rollout_commands(authoritative_modes),
    )
    require(
        "rejected" in rejected["stderr"].lower(),
        f"insufficient rollout evidence did not report rejection: {rejected}",
    )
    rejected_snapshot = harness.runtime_db_snapshot(
        "scheduler-rollout-insufficient-evidence"
    )
    require(
        not rejected_snapshot["scheduler_rollout_preflights"]
        and not rejected_snapshot["scheduler_rollout_manifests"]
        and not rejected_snapshot["scheduler_rollout_command_results"],
        "rejected rollout batch left partial canonical state",
    )
    harness.apply_scheduler_rollout(
        "scheduler-rollout-shadow",
        scheduler_rollout_commands(
            scenario_modes,
            approved_scenario_modes=authoritative_modes,
        ),
    )
    harness.start()

    shadow_marker = secrets.token_hex(4)
    shadow_objective = f"SCHEDULER-SHADOW-WAIT-{shadow_marker}"
    shadow_completion = f"SCHEDULER-SHADOW-COMPLETE-{shadow_marker}"
    callback = harness.reset_callback("scheduler-shadow-callback")
    shadow_phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-shadow-wait",
        shadow_phase["prompt"].format(
            case_id=case["id"],
            objective=shadow_objective,
            completion_marker=shadow_completion,
        ),
    )
    waiting = harness.wait_work_item_scheduling_state(
        objective_marker=shadow_objective,
        expected_scheduling_state="waiting_external",
        label="scheduler-shadow-waiting",
    )
    harness.fire_callback(
        "scheduler-shadow-wake",
        callback["trigger_url"],
        {"case_id": case["id"], "marker": shadow_marker},
    )
    shadow_item = harness.wait_work_item(
        objective_marker=shadow_objective,
        expected_state="completed",
        label="scheduler-shadow-completed",
    )
    required, forbidden = phase_tools(shadow_phase)
    harness.assert_tools("scheduler-shadow-wait", baseline, required, forbidden)
    require(
        shadow_item["id"] == waiting["id"],
        "shadow wait-resume changed WorkItem identity",
    )
    shadow_snapshot = harness.runtime_db_snapshot("scheduler-shadow")
    shadow_comparisons = [
        row
        for row in shadow_snapshot["scheduler_shadow_comparisons"]
        if row["scenario_class"] == "exact_wait_resume"
    ]
    require(
        len(shadow_comparisons) == 1
        and shadow_comparisons[0]["authority_mode"] == "shadow"
        and shadow_comparisons[0]["comparison_outcome"] == "matched",
        f"shadow wait-resume evidence is incomplete: {shadow_comparisons}",
    )
    require(
        not shadow_snapshot["scheduler_activations"],
        f"shadow mode unexpectedly acquired canonical execution ownership: "
        f"{shadow_snapshot['scheduler_activations']}",
    )

    harness.stop()
    first_authoritative_revision = 2 + len(scenario_modes)
    harness.apply_scheduler_rollout(
        "scheduler-rollout-authoritative",
        scheduler_scenario_mode_commands(
            authoritative_modes,
            expected_config_revision=first_authoritative_revision,
            identity_suffix="cutover",
        ),
    )
    harness.start()

    marker = secrets.token_hex(4)
    objective_marker = f"SCHEDULER-AUTHORITATIVE-CONTINUATIONS-{marker}"
    task_marker = f"SCHEDULER-TASK-RESULT-{marker}"
    completion_marker = f"SCHEDULER-AUTHORITATIVE-COMPLETE-{marker}"
    objective = (
        f"{objective_marker}. This WorkItem must exercise two continuation boundaries. "
        f"On the first autonomous work_queue turn, call ExecCommand with command "
        f"`sleep 15; printf {task_marker}`, yield_time_ms=50, and a bounded output limit. "
        "Use the returned promoted command task_id to call WaitFor with wake=task_result. "
        "Do not poll the task and do not complete the WorkItem. On the task-result rejoin, "
        "call GetWorkItem for the current WorkItem, then call WaitFor with wake=external, "
        f"resource=docker-e2e:{marker}, and a concrete reason. Do not complete it yet. "
        "On the later external wake, call GetWorkItem, update both existing todos to "
        "completed, then emit a concise operator-facing completion report containing "
        f"{completion_marker} immediately followed by CompleteWorkItem for the exact "
        "current item. Do not create another WorkItem."
    )
    authoritative_phase = case["phases"][1]
    baseline, _ = harness.prompt(
        "scheduler-authoritative-seed",
        authoritative_phase["prompt"].format(
            case_id=case["id"],
            objective=json.dumps(objective, ensure_ascii=False),
            completion_marker=completion_marker,
        ),
    )
    task_waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_marker,
        expected_scheduling_state="waiting_task",
        label="scheduler-authoritative-task-wait",
    )
    external_waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_marker,
        expected_scheduling_state="waiting_external",
        label="scheduler-authoritative-external-wait",
    )
    require(
        external_waiting["id"] == task_waiting["id"],
        "task-result rejoin changed WorkItem identity",
    )
    callback = harness.reset_callback("scheduler-authoritative-callback")
    harness.fire_callback(
        "scheduler-authoritative-wake",
        callback["trigger_url"],
        {"case_id": case["id"], "marker": marker},
    )
    item = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="scheduler-authoritative-completed",
    )
    required, forbidden = phase_tools(authoritative_phase)
    harness.assert_tools(
        "scheduler-authoritative-seed", baseline, required, forbidden
    )
    work_item_id = item["id"]
    result_brief_id = item.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"authoritative WorkItem omitted result brief: {item}",
    )
    result_brief = harness.brief(result_brief_id, "scheduler-authoritative-result")
    require(
        result_brief.get("work_item_id") == work_item_id
        and completion_marker in (result_brief.get("text") or ""),
        f"authoritative completion brief mismatch: {result_brief}",
    )

    snapshot = harness.runtime_db_snapshot("scheduler-authoritative")
    require_rollout_state(snapshot, authoritative_modes)
    activation_rows = [
        row
        for row in snapshot["scheduler_activations"]
        if row["work_item_id"] == work_item_id
    ]
    message_ids = {
        json.loads(row["payload_json"])["activation"]["provenance"]["source_id"]
        for row in activation_rows
    }
    scheduler_messages = [
        row for row in snapshot["messages"] if row["message_id"] in message_ids
    ]
    require(
        len(activation_rows) == 3
        and len(message_ids) == 3
        and len(scheduler_messages) == 3,
        f"expected autonomous, task-result, and wait-resume messages: {scheduler_messages}",
    )
    require_processed_queue_entries(snapshot["queue_entries"], message_ids)
    turns = require_turns_terminal(snapshot, message_ids)
    activations = require_scheduler_activation_chain(
        snapshot,
        agent_id=harness.agent_id,
        work_item_id=work_item_id,
        expected_activation_count=3,
    )
    demand = [
        row
        for row in snapshot["scheduler_work_demands"]
        if row["work_item_id"] == work_item_id
    ]
    require(
        len(demand) == 1
        and demand[0]["status"] == "terminal"
        and demand[0]["scheduling_generation"]
        == max(row["admitted_generation"] for row in activations) + 1,
        f"canonical WorkItem demand did not converge at the final generation: {demand}",
    )
    final_turn_id = result_brief.get("turn_id")
    final_message_id = result_brief.get("related_message_id")
    require(
        any(
            row["turn_id"] == final_turn_id
            and row["trigger_message_id"] == final_message_id
            for row in turns
        ),
        f"result brief was not bound to the terminal continuation turn: {result_brief}",
    )
    brief_rows = [
        row for row in snapshot["briefs"] if row["evidence_id"] == result_brief_id
    ]
    require(
        len(brief_rows) == 1
        and brief_rows[0]["work_item_id"] == work_item_id
        and brief_rows[0]["turn_id"] == final_turn_id
        and brief_rows[0]["message_id"] == final_message_id,
        f"database result brief binding is not exact: {brief_rows}",
    )
    waits = [
        row
        for row in snapshot["wait_conditions"]
        if row["work_item_id"] == work_item_id
    ]
    require(
        len(waits) == 2
        and {row["kind"] for row in waits} == {"task", "external"}
        and {
            row["kind"]: row["status"]
            for row in waits
        }
        == {"task": "resolved", "external": "cancelled"},
        f"legacy task/external waits did not reach their terminal states: {waits}",
    )
    canonical_waits = [
        row
        for row in snapshot["scheduler_wait_generations"]
        if row["owner_work_item_id"] == work_item_id
    ]
    require(
        len(canonical_waits) == 2
        and all(row["lifecycle_state"] == "resolved" for row in canonical_waits)
        and all(row["consuming_activation_id"] is None for row in canonical_waits),
        f"canonical task/external waits did not resolve exactly once: {canonical_waits}",
    )
    require_scheduler_comparisons(
        snapshot,
        {
            "work_item_autonomous_continuation": 1,
            "exact_task_rejoin": 1,
            "exact_wait_resume": 1,
            "settlement": 3,
            "delivery": 3,
        },
    )

    harness.stop()
    rollback_revision = first_authoritative_revision + len(authoritative_modes)
    harness.apply_scheduler_rollout(
        "scheduler-rollout-rollback",
        scheduler_scenario_mode_commands(
            {"work_item_autonomous_continuation": "shadow"},
            expected_config_revision=rollback_revision,
            identity_suffix="rollback",
        ),
    )
    harness.start()
    restarted = harness.work_items("scheduler-authoritative-after-rollback")
    restored = next(candidate for candidate in restarted if candidate["id"] == work_item_id)
    require(
        restored["state"] == "completed"
        and restored.get("result_brief_id") == result_brief_id,
        f"rollback/restart changed completed WorkItem identity: {restored}",
    )
    rollback_snapshot = harness.runtime_db_snapshot("scheduler-rollback")
    expected_after_rollback = dict(authoritative_modes)
    expected_after_rollback["work_item_autonomous_continuation"] = "shadow"
    require_rollout_state(rollback_snapshot, expected_after_rollback)
    require_scheduler_activation_chain(
        rollback_snapshot,
        agent_id=harness.agent_id,
        work_item_id=work_item_id,
        expected_activation_count=3,
    )


def run_scheduler_terminal_recovery_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    scenario_modes = {
        "work_item_autonomous_continuation": "authoritative",
        "settlement": "authoritative",
    }
    harness.apply_scheduler_rollout(
        "scheduler-recovery-rollout",
        scheduler_rollout_commands(scenario_modes),
    )
    objective = f"SCHEDULER-TERMINAL-RECOVERY-{secrets.token_hex(4)}"
    fixture = harness.seed_scheduler_recovery_fixture(
        "scheduler-terminal-recovery-fixture",
        objective,
    )
    harness.start(wait_idle=False)
    deadline = time.monotonic() + harness.timeout_seconds
    recovered = False
    while time.monotonic() < deadline:
        recovered = any(
            event["type"] == "scheduler_bootstrap_claim_recovered"
            and event["payload"].get("message_id") == fixture["message_id"]
            for event in harness.events("scheduler-recovery-poll")
        )
        if recovered:
            break
        time.sleep(1)
    require(recovered, "serve bootstrap did not reconcile the recovery fixture")
    first = harness.runtime_db_snapshot("scheduler-recovery-first")
    require_rollout_state(first, scenario_modes)
    require_processed_queue_entries(first["queue_entries"], {fixture["message_id"]})
    require_turns_terminal(first, {fixture["message_id"]})
    activations = require_scheduler_activation_chain(
        first,
        agent_id=fixture["agent_id"],
        work_item_id=fixture["work_item_id"],
        expected_activation_count=1,
    )
    require(
        activations[0]["activation_id"] == fixture["activation_id"]
        and activations[0]["admitted_generation"] == fixture["admitted_generation"],
        f"recovery changed activation identity or generation: {activations}",
    )
    work_items = [
        row
        for row in first["work_items"]
        if row["work_item_id"] == fixture["work_item_id"]
    ]
    require(
        len(work_items) == 1 and work_items[0]["state"] == "open",
        f"recovery fixture WorkItem disposition changed unexpectedly: {work_items}",
    )
    demands = [
        row
        for row in first["scheduler_work_demands"]
        if row["work_item_id"] == fixture["work_item_id"]
    ]
    require(
        len(demands) == 1
        and demands[0]["status"] == "runnable"
        and demands[0]["scheduling_generation"]
        == fixture["admitted_generation"] + 1,
        f"terminal recovery did not advance the successor WorkItem generation: {demands}",
    )
    require(
        not first["scheduler_missing_settlements"],
        f"terminal recovery produced missing-settlement evidence: "
        f"{first['scheduler_missing_settlements']}",
    )

    harness.restart(wait_idle=False)
    second = harness.runtime_db_snapshot("scheduler-recovery-second")
    require(
        second["queue_entries"] == first["queue_entries"]
        and second["scheduler_activations"] == first["scheduler_activations"]
        and second["scheduler_activation_settlements"]
        == first["scheduler_activation_settlements"]
        and second["scheduler_missing_settlements"]
        == first["scheduler_missing_settlements"]
        and second["scheduler_work_demands"] == first["scheduler_work_demands"]
        and second["scheduler_agent_slots"] == first["scheduler_agent_slots"]
        and second["scheduler_wait_generations"]
        == first["scheduler_wait_generations"]
        and second["scheduler_protocol_command_results"]
        == first["scheduler_protocol_command_results"]
        and second["turn_records"] == first["turn_records"]
        and second["audit_events"] == first["audit_events"],
        "second restart changed reconciled scheduler state",
    )


CASE_RUNNERS = {
    "runtime-auth-model-delivery": run_runtime_case,
    "memory-agent-home-persistence": run_memory_case,
    "workspace-restart-lifecycle": run_workspace_case,
    "workitem-wait-restart-complete": run_workitem_case,
    "scheduler-autonomous-legacy": run_scheduler_protocol_case,
    "scheduler-autonomous-authoritative": run_scheduler_protocol_case,
    "scheduler-rollout-authoritative-autonomous": (
        run_scheduler_rollout_authoritative_case
    ),
    "scheduler-terminal-before-settlement-restart": (
        run_scheduler_terminal_recovery_case
    ),
}


def validate_manifest(manifest: dict[str, Any]) -> None:
    require(manifest.get("version") == 2, "manifest version must be 2")
    cases = manifest.get("cases")
    require(isinstance(cases, list) and cases, "manifest cases must be non-empty")
    seen: set[str] = set()
    for case in cases:
        case_id = case.get("id")
        require(isinstance(case_id, str) and case_id, "case id must be non-empty")
        require(case_id not in seen, f"duplicate case id: {case_id}")
        seen.add(case_id)
        require(case_id in CASE_RUNNERS, f"case has no registered runner: {case_id}")
        require(
            case.get("tier") in {"core", "extended", "published"},
            f"{case_id} has invalid tier",
        )
        require(
            isinstance(case.get("tags"), list),
            f"{case_id} tags must be a list",
        )
        require(
            isinstance(case.get("timeout_seconds"), int)
            and case["timeout_seconds"] > 0,
            f"{case_id} timeout_seconds must be positive",
        )
        if "requires_model" in case:
            require(
                isinstance(case["requires_model"], bool),
                f"{case_id} requires_model must be boolean",
            )
        runtime_env = case.get("runtime_env", {})
        require(isinstance(runtime_env, dict), f"{case_id} runtime_env must be an object")
        require(
            all(
                isinstance(name, str)
                and name.startswith("HOLON_")
                and isinstance(value, str)
                for name, value in runtime_env.items()
            ),
            f"{case_id} runtime_env must contain HOLON_ string entries",
        )
        if "scheduler_protocol_commands_enabled" in case:
            require(
                isinstance(case["scheduler_protocol_commands_enabled"], bool),
                f"{case_id} scheduler_protocol_commands_enabled must be boolean",
            )
        phases = case.get("phases")
        require(isinstance(phases, list) and phases, f"{case_id} needs phases")
        phase_ids: set[str] = set()
        for phase in phases:
            phase_id = phase.get("id")
            require(
                isinstance(phase_id, str) and phase_id,
                f"{case_id} phase id must be non-empty",
            )
            require(
                phase_id not in phase_ids,
                f"{case_id} has duplicate phase {phase_id}",
            )
            phase_ids.add(phase_id)
            require(
                isinstance(phase.get("prompt"), str) and phase["prompt"],
                f"{case_id}/{phase_id} prompt must be non-empty",
            )
            required, forbidden = phase_tools(phase)
            require(
                all(isinstance(name, str) and name for name in required + forbidden),
                f"{case_id}/{phase_id} tool names must be non-empty strings",
            )
            require(
                not set(required).intersection(forbidden),
                f"{case_id}/{phase_id} has required/forbidden tool overlap",
            )


def select_cases(
    manifest: dict[str, Any],
    *,
    requested: list[str] | None,
    suite: str,
    tags: list[str],
) -> list[dict[str, Any]]:
    cases = manifest["cases"]
    if requested:
        unknown = sorted(set(requested) - {case["id"] for case in cases})
        require(not unknown, f"unknown cases: {', '.join(unknown)}")
        selected = [case for case in cases if case["id"] in requested]
    else:
        selected = [case for case in cases if case["tier"] == suite]
    if tags:
        selected = [
            case for case in selected if set(tags).issubset(set(case.get("tags", [])))
        ]
    require(selected, "case selection is empty")
    return selected


def parse_env_file(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in path.read_text().splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#") or "=" not in stripped:
            continue
        name, value = stripped.split("=", 1)
        values[name.strip()] = value.strip().strip("'\"")
    return values


def secret_scan(evidence_root: Path, secrets_to_find: list[str]) -> dict[str, Any]:
    findings: list[dict[str, str]] = []
    for path in sorted(evidence_root.rglob("*")):
        if not path.is_file() or path.name == "secret-scan.json":
            continue
        try:
            text = path.read_text(errors="replace")
        except OSError:
            continue
        relative = str(path.relative_to(evidence_root))
        for index, value in enumerate(secrets_to_find):
            if len(value) >= 8 and value in text:
                findings.append({"path": relative, "kind": f"secret-value-{index + 1}"})
        if BEARER_SECRET_PATTERN.search(text):
            findings.append({"path": relative, "kind": "bearer-header"})
        if CALLBACK_CAPABILITY_SCAN_PATTERN.search(text):
            findings.append({"path": relative, "kind": "callback-capability"})
    result = {
        "schema_version": EVIDENCE_SCHEMA_VERSION,
        "status": "pass" if not findings else "fail",
        "findings": findings,
    }
    write_json(evidence_root / "secret-scan.json", result)
    if findings:
        quarantine_secret_findings(evidence_root, findings)
    return result


def quarantine_secret_findings(
    evidence_root: Path, findings: list[dict[str, str]]
) -> None:
    by_path: dict[str, set[str]] = {}
    for finding in findings:
        by_path.setdefault(finding["path"], set()).add(finding["kind"])
    for relative, kinds in by_path.items():
        path = evidence_root / relative
        try:
            if path.is_file():
                path.write_text(
                    "Evidence file quarantined because the secret scan reported "
                    f"{', '.join(sorted(kinds))}. See secret-scan.json for metadata.\n"
                )
        except OSError:
            continue


def memory_value(result: dict[str, Any]) -> dict[str, Any]:
    memory = result.get("memory")
    return memory if isinstance(memory, dict) else result


def image_identity(image: str) -> dict[str, Any]:
    result = run(
        ["docker", "image", "inspect", image, "--format", "{{json .}}"],
        check=False,
    )
    if result.returncode != 0:
        return {"ref": image, "id": None, "repo_digests": []}
    inspected = json.loads(result.stdout)
    return {
        "ref": image,
        "id": inspected.get("Id"),
        "repo_digests": inspected.get("RepoDigests") or [],
    }


def collect_case_metrics(evidence: Path) -> dict[str, Any]:
    tool_counts: dict[str, int] = {}
    provider_attempts = 0
    token_usage = {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}
    for path in evidence.glob("*-events.json"):
        try:
            events = json.loads(path.read_text()).get("events", [])
        except (OSError, json.JSONDecodeError):
            continue
        for event in events:
            event_type = event.get("type")
            if event_type == "tool_executed":
                name = event.get("payload", {}).get("tool_name", "unknown")
                tool_counts[name] = tool_counts.get(name, 0) + 1
            if event_type == "provider_round_completed":
                payload = event.get("payload", {})
                provider_attempts += len(
                    (payload.get("provider_attempt_timeline") or {}).get("attempts") or []
                )
                usage = payload.get("token_usage") or {}
                for key in token_usage:
                    token_usage[key] += int(usage.get(key) or 0)
    return {
        "tool_counts": tool_counts,
        "provider_attempts": provider_attempts,
        "token_usage": token_usage,
    }


def write_junit(path: Path, cases: list[dict[str, Any]], duration: float) -> None:
    suite = ElementTree.Element(
        "testsuite",
        {
            "name": "holon-docker-e2e",
            "tests": str(len(cases)),
            "failures": str(sum(case["status"] != "pass" for case in cases)),
            "time": f"{duration:.3f}",
        },
    )
    for case in cases:
        node = ElementTree.SubElement(
            suite,
            "testcase",
            {
                "classname": f"docker-e2e.{case['tier']}",
                "name": case["id"],
                "time": f"{case['duration_seconds']:.3f}",
            },
        )
        if case["status"] != "pass":
            failure = ElementTree.SubElement(node, "failure", {"message": case["error"]})
            failure.text = case["error"]
    ElementTree.indent(suite)
    ElementTree.ElementTree(suite).write(path, encoding="unicode", xml_declaration=True)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", default="holon:dev")
    parser.add_argument("--image-digest")
    parser.add_argument("--previous-image")
    parser.add_argument("--model")
    parser.add_argument("--suite", choices=["core", "extended", "published"], default="core")
    parser.add_argument("--case", action="append", dest="cases")
    parser.add_argument("--tag", action="append", default=[])
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--env-file", type=Path)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument("--keep-on-failure", action="store_true")
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--validate-manifest", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    manifest = json.loads(args.manifest.read_text())
    validate_manifest(manifest)
    if args.validate_manifest:
        print(f"valid manifest: {args.manifest}")
        return 0
    if args.list:
        for case in manifest["cases"]:
            tags = ",".join(case.get("tags", []))
            print(f"{case['id']}\t{case['tier']}\t{tags}")
        return 0

    require(shutil.which("docker") is not None, "docker is required")
    selected = select_cases(
        manifest,
        requested=args.cases,
        suite=args.suite,
        tags=args.tag,
    )
    requires_model = any(case.get("requires_model", True) for case in selected)
    model = args.model or first_env(
        "HOLON_E2E_MODEL", "HOLON_LIVE_MODEL", default=DEFAULT_MODEL
    )
    raw_names = first_env(
        "HOLON_E2E_CREDENTIAL_ENVS", "HOLON_LIVE_CREDENTIAL_ENVS"
    )
    credential_envs = [name.strip() for name in raw_names.split(",") if name.strip()]
    env_file_value = args.env_file or first_env(
        "HOLON_E2E_DOCKER_ENV_FILE", "HOLON_LIVE_DOCKER_ENV_FILE"
    )
    env_file = Path(env_file_value).resolve() if env_file_value else None
    if requires_model and not credential_envs and env_file is None:
        inferred = inferred_credential_env(model)
        require(
            inferred is not None,
            "set HOLON_E2E_CREDENTIAL_ENVS or HOLON_E2E_DOCKER_ENV_FILE "
            f"for model {model}",
        )
        credential_envs = [inferred]
    if requires_model:
        for name in credential_envs:
            require(name in os.environ, f"required credential environment {name} is unset")
    else:
        credential_envs = []
        env_file = None

    secret_values = [os.environ[name] for name in credential_envs]
    if env_file is not None:
        require(env_file.is_file(), f"env file does not exist: {env_file}")
        mode = stat.S_IMODE(env_file.stat().st_mode)
        require(mode & 0o077 == 0, "env file must not be accessible by group or others")
        secret_values.extend(parse_env_file(env_file).values())

    image = args.image_digest or args.image
    if args.image_digest:
        require(
            "@sha256:" in args.image_digest,
            "--image-digest must be an immutable ref containing @sha256:",
        )
    if not args.skip_build:
        require(not args.image_digest, "cannot build when --image-digest is supplied")
        run(["docker", "build", "--tag", image, str(ROOT)], capture=False)

    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    evidence_root = (
        args.evidence_dir.resolve()
        if args.evidence_dir
        else ROOT / "target/docker-e2e" / timestamp
    )
    evidence_root.mkdir(parents=True, exist_ok=True)
    started_at = utc_now()
    started_monotonic = time.monotonic()
    git_sha = run(["git", "rev-parse", "HEAD"]).stdout.strip()
    identity = image_identity(image)
    run_record = {
        "schema_version": EVIDENCE_SCHEMA_VERSION,
        "started_at": started_at,
        "git_sha": git_sha,
        "image": identity,
        "previous_image": args.previous_image,
        "model_route": model,
        "suite": args.suite,
        "cases": [case["id"] for case in selected],
        "credential_env_names": credential_envs,
        "env_file_used": env_file is not None,
        "manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
    }
    write_json(evidence_root / "run.json", run_record)

    timeout_override = first_env(
        "HOLON_E2E_TIMEOUT_SECONDS", "HOLON_LIVE_TIMEOUT_SECONDS"
    )
    keep_on_failure = args.keep_on_failure or env_flag(
        "HOLON_E2E_KEEP", "HOLON_LIVE_KEEP"
    )
    case_results: list[dict[str, Any]] = []
    control_tokens: list[str] = []
    for case in selected:
        case_id = case["id"]
        case_started = time.monotonic()
        print(f"Running {case_id} with {model}")
        harness = CaseHarness(
            case_id=case_id,
            image=image,
            model=model,
            requires_model=case.get("requires_model", True),
            credential_envs=credential_envs,
            env_file=env_file,
            runtime_env=dict(case.get("runtime_env", {})),
            evidence_root=evidence_root,
            timeout_seconds=(
                int(timeout_override)
                if timeout_override
                else int(case["timeout_seconds"])
            ),
            keep=False,
        )
        control_tokens.append(harness.token)
        error_text = ""
        try:
            CASE_RUNNERS[case_id](harness, case)
            harness.capture_context("final")
            status = "pass"
            print(f"PASS {case_id}")
        except Exception as error:
            status = "fail"
            error_text = f"{type(error).__name__}: {error}"
            (harness.evidence / "failure.txt").write_text(error_text + "\n")
            try:
                harness.capture_context("failure")
            except Exception:
                pass
            try:
                harness.capture_logs()
            except Exception:
                pass
            print(f"FAIL {case_id}: {error}", file=sys.stderr)
        finally:
            harness.keep = keep_on_failure and status == "fail"
            cleanup_result = harness.cleanup()
            if cleanup_result["status"] == "fail":
                status = "fail"
                cleanup_error = "; ".join(cleanup_result["errors"])
                error_text = (
                    f"{error_text}; cleanup failed: {cleanup_error}"
                    if error_text
                    else f"cleanup failed: {cleanup_error}"
                )
        result = {
            "id": case_id,
            "tier": case["tier"],
            "tags": case.get("tags", []),
            "status": status,
            "error": error_text,
            "duration_seconds": round(time.monotonic() - case_started, 3),
            "cleanup": cleanup_result["status"],
            "cleanup_errors": cleanup_result["errors"],
            **collect_case_metrics(harness.evidence),
        }
        write_json(harness.evidence / "case.json", result)
        case_results.append(result)

    scan = secret_scan(evidence_root, secret_values + control_tokens)
    duration = time.monotonic() - started_monotonic
    if scan["status"] != "pass":
        case_results.append(
            {
                "id": "secret-scan",
                "tier": "core",
                "tags": ["security"],
                "status": "fail",
                "error": f"evidence contains {len(scan['findings'])} secret finding(s)",
                "duration_seconds": 0.0,
                "cleanup": "not-applicable",
                "tool_counts": {},
                "provider_attempts": 0,
                "token_usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "total_tokens": 0,
                },
            }
        )
    summary = {
        **run_record,
        "finished_at": utc_now(),
        "duration_seconds": round(duration, 3),
        "status": (
            "pass" if all(case["status"] == "pass" for case in case_results) else "fail"
        ),
        "case_results": case_results,
        "secret_scan": scan["status"],
    }
    write_json(evidence_root / "summary.json", summary)
    write_junit(evidence_root / "junit.xml", case_results, duration)

    print(f"Evidence: {evidence_root}")
    failures = [
        f"{case['id']}: {case['error']}"
        for case in case_results
        if case["status"] != "pass"
    ]
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    return 0
