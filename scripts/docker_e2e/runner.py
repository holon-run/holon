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
    r"/api/callbacks/(?:wake|enqueue)/([^\s\"'`),}\]]+)"
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
        credential_envs: list[str],
        env_file: Path | None,
        evidence_root: Path,
        timeout_seconds: int,
        keep: bool,
    ) -> None:
        suffix = secrets.token_hex(4)
        self.case_id = case_id
        self.image = image
        self.model = model
        self.credential_envs = credential_envs
        self.env_file = env_file
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

    def start(self) -> None:
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

    def restart(self) -> None:
        self.stop()
        self.start()

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
        path.write_text(result.stdout + result.stderr)

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


def result_value(detail: dict[str, Any]) -> dict[str, Any]:
    output = detail.get("output", {})
    return output.get("envelope", {}).get("result", output.get("result", output))


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


CASE_RUNNERS = {
    "runtime-auth-model-delivery": run_runtime_case,
    "memory-agent-home-persistence": run_memory_case,
    "workspace-restart-lifecycle": run_workspace_case,
    "workitem-wait-restart-complete": run_workitem_case,
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
        for match in CALLBACK_CAPABILITY_SCAN_PATTERN.finditer(text):
            if match.group(1) != "<redacted>":
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
    if not credential_envs and env_file is None:
        inferred = inferred_credential_env(model)
        require(
            inferred is not None,
            "set HOLON_E2E_CREDENTIAL_ENVS or HOLON_E2E_DOCKER_ENV_FILE "
            f"for model {model}",
        )
        credential_envs = [inferred]
    for name in credential_envs:
        require(name in os.environ, f"required credential environment {name} is unset")

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

    selected = select_cases(
        manifest,
        requested=args.cases,
        suite=args.suite,
        tags=args.tag,
    )
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
            credential_envs=credential_envs,
            env_file=env_file,
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
