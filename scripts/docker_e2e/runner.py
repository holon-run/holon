#!/usr/bin/env python3
"""Release Docker E2E against a real LLM and the public Holon HTTP API."""

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
import subprocess
import sys
import threading
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
OPENAI_STUB_ROOT = ROOT / "tests/e2e/docker/openai_stub"
OPENAI_STUB_IMAGE = "holon-openai-responses-stub:local"
DEFAULT_MODEL = "deepseek/deepseek-v4-flash"
OFFLINE_MODEL_CREDENTIAL_ENV = "DEEPSEEK_API_KEY"
OFFLINE_MODEL_CREDENTIAL = "docker-e2e-offline-provider-unused"
EVIDENCE_SCHEMA_VERSION = 1
SCHEDULER_ACCEPTANCE_REPORT_SCHEMA_VERSION = 1
SCHEDULER_COVERAGE_REPORT_SCHEMA_VERSION = 1
SCHEDULER_LIVE_CANARY_REPORT_SCHEMA_VERSION = 1
SCHEDULER_ENGINES = ("legacy", "canonical")
TERMINAL_STATUSES = {"awake_idle", "asleep", "awaiting_task"}
RUNTIME_DB_COPY_TIMEOUT_SECONDS = 120
DOCKER_CONTROL_TIMEOUT_SECONDS = 30
DOCKER_CIRCUIT_BREAKER_THRESHOLD = 2
CONTEXT_EVENT_LIMIT = 300
CONTEXT_BRIEF_LIMIT = 10
CONTEXT_TRANSCRIPT_LIMIT = 20


class DockerCircuitBreakerOpen(RuntimeError):
    pass


def run(
    args: list[str],
    *,
    check: bool = True,
    capture: bool = True,
    env: dict[str, str] | None = None,
    timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        check=check,
        text=True,
        capture_output=capture,
        env=env,
        timeout=timeout,
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


def load_runtime_config(path: Path | None) -> dict[str, Any]:
    if path is None:
        return {}
    require(path.is_file(), f"config file does not exist: {path}")
    value = json.loads(path.read_text())
    require(isinstance(value, dict), "config file must contain a JSON object")
    return value


def merged_runtime_config(
    config: dict[str, Any],
    model: str,
    model_runtime_override: dict[str, int] | None,
) -> dict[str, Any]:
    merged = copy.deepcopy(config)
    if model_runtime_override:
        models = merged.setdefault("models", {})
        require(isinstance(models, dict), "config models must be a JSON object")
        catalog = models.setdefault("catalog", {})
        require(isinstance(catalog, dict), "config models.catalog must be a JSON object")
        catalog[model] = dict(model_runtime_override)
    return merged


def provider_base_url_env(model: str) -> str:
    route = model.split("/", 1)[0]
    provider, separator, endpoint = route.partition("@")
    if provider == "anthropic":
        return "ANTHROPIC_BASE_URL"
    endpoint_overrides = {
        ("volcengine", "plan"): "HOLON_VOLCENGINE_AGENT_BASE_URL",
    }
    if separator and endpoint != "default":
        override = endpoint_overrides.get((provider, endpoint))
        require(
            override is not None,
            f"provider failure retry does not know the base URL environment for {route}",
        )
        return override
    fragment = "".join(
        character.upper() if character.isalnum() else "_"
        for character in provider
    )
    return f"HOLON_{fragment}_BASE_URL"


class CaseHarness:
    def __init__(
        self,
        *,
        case_id: str,
        image: str,
        model: str,
        model_fallbacks: list[str] | None = None,
        disable_provider_fallback: bool = True,
        requires_model: bool = True,
        credential_envs: list[str],
        env_file: Path | None,
        runtime_env: dict[str, str],
        evidence_root: Path,
        timeout_seconds: int,
        keep: bool,
        runtime_config: dict[str, Any] | None = None,
        provider_mode: str = "live",
        stub_scenario: str | None = None,
        model_runtime_override: dict[str, int] | None = None,
        tool_assertion_mode: str = "strict",
        resource_names: dict[str, str] | None = None,
        control_token: str | None = None,
    ) -> None:
        suffix = secrets.token_hex(4)
        self.case_id = case_id
        self.image = image
        self.model = model if requires_model else DEFAULT_MODEL
        self.model_fallbacks = list(model_fallbacks or []) if requires_model else []
        self.disable_provider_fallback = (
            disable_provider_fallback if requires_model else True
        )
        self.credential_envs = credential_envs if requires_model else []
        self.env_file = env_file if requires_model else None
        self.runtime_config = copy.deepcopy(runtime_config or {})
        self.runtime_env = dict(runtime_env)
        if not requires_model:
            self.runtime_env.setdefault(
                OFFLINE_MODEL_CREDENTIAL_ENV,
                OFFLINE_MODEL_CREDENTIAL,
            )
        self.evidence = evidence_root / case_id
        self.timeout_seconds = timeout_seconds
        self.keep = keep
        self.provider_mode = provider_mode
        self.stub_scenario = stub_scenario
        self.stub_provider_base_url: str | None = None
        self.model_runtime_override = dict(model_runtime_override or {})
        self._model_runtime_override_seeded = False
        self.tool_assertion_mode = tool_assertion_mode
        names = resource_names or {}
        self.volume = names.get("volume", f"holon-live-{case_id}-{suffix}")
        self.network = names.get("network", f"holon-live-{case_id}-{suffix}")
        self.container = names.get("container", f"holon-live-{case_id}-{suffix}")
        self.stub_container = names.get(
            "stub_container", f"holon-openai-stub-{case_id}-{suffix}"
        )
        self.token = control_token or secrets.token_urlsafe(24)
        self.base_url = ""
        self.agent_id = ""
        workspace_parent = names.get("workspace_parent")
        self.workspace_parent = (
            Path(workspace_parent) if workspace_parent else self.evidence / "workspace"
        )
        self.log_index = 0
        self._docker_health = {
            "lock": threading.Lock(),
            "consecutive_failures": 0,
            "open": False,
            "last_error": None,
        }
        self._checkpoint_state = {
            "lock": threading.Lock(),
            "claimed": set(),
        }
        self._prompt_scopes: dict[str, dict[str, Any]] = {}
        self.evidence.mkdir(parents=True, exist_ok=True)

    @property
    def scheduler_engine(self) -> str | None:
        return self.runtime_env.get("HOLON_SCHEDULER")

    @property
    def canonical_scheduler_enabled(self) -> bool:
        return self.scheduler_engine == "canonical"

    def docker(self, *args: str, **kwargs: Any) -> subprocess.CompletedProcess[str]:
        with self._docker_health["lock"]:
            if self._docker_health["open"]:
                raise DockerCircuitBreakerOpen(
                    "Docker circuit breaker is open after consecutive control-plane failures"
                )
        if "timeout" not in kwargs and args:
            bounded = args[0] in {
                "cp",
                "inspect",
                "kill",
                "logs",
                "network",
                "port",
                "ps",
                "rm",
                "stats",
                "version",
                "volume",
            } or (args[0] == "run" and "--detach" in args)
            if bounded:
                kwargs["timeout"] = DOCKER_CONTROL_TIMEOUT_SECONDS
        try:
            result = run(["docker", *args], **kwargs)
        except (
            subprocess.CalledProcessError,
            subprocess.TimeoutExpired,
            OSError,
        ) as error:
            with self._docker_health["lock"]:
                self._docker_health["consecutive_failures"] += 1
                self._docker_health["last_error"] = f"{type(error).__name__}: {error}"
                if (
                    self._docker_health["consecutive_failures"]
                    >= DOCKER_CIRCUIT_BREAKER_THRESHOLD
                ):
                    self._docker_health["open"] = True
            raise
        with self._docker_health["lock"]:
            self._docker_health["consecutive_failures"] = 0
            self._docker_health["last_error"] = None
        return result

    def check_docker_health(self, label: str) -> dict[str, Any]:
        try:
            result = self.docker(
                "version",
                "--format",
                "{{json .Server}}",
                timeout=10,
            )
            server = json.loads(result.stdout)
            health = {
                "status": "healthy",
                "checked_at": utc_now(),
                "server_version": server.get("Version"),
                "os": server.get("Os"),
                "arch": server.get("Arch"),
            }
        except Exception as error:
            health = {
                "status": "failed",
                "checked_at": utc_now(),
                "error": f"{type(error).__name__}: {error}",
            }
            write_json(self.evidence / f"{label}-docker-health.json", health)
            raise
        write_json(self.evidence / f"{label}-docker-health.json", health)
        return health

    def claim_checkpoint(self, name: str) -> bool:
        with self._checkpoint_state["lock"]:
            if name in self._checkpoint_state["claimed"]:
                return False
            self._checkpoint_state["claimed"].add(name)
            return True

    def resource_telemetry(self, label: str) -> dict[str, Any]:
        disk = os.statvfs(self.evidence)
        host_rss_kb = 0
        for line in Path("/proc/self/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                host_rss_kb = int(line.split()[1])
                break
        telemetry: dict[str, Any] = {
            "captured_at": utc_now(),
            "host": {
                "runner_rss_kb": host_rss_kb,
                "runner_fd_count": len(list(Path("/proc/self/fd").iterdir())),
                "evidence_bytes": sum(
                    path.stat().st_size
                    for path in self.evidence.rglob("*")
                    if path.is_file()
                ),
                "filesystem_free_bytes": disk.f_bavail * disk.f_frsize,
            },
        }
        stats = self.docker(
            "stats",
            "--no-stream",
            "--format",
            "{{json .}}",
            self.container,
            check=False,
        )
        if stats.returncode == 0 and stats.stdout.strip():
            telemetry["docker_stats"] = json.loads(stats.stdout)
        process = self.docker(
            "exec",
            self.container,
            "bash",
            "-lc",
            "set -euo pipefail; "
            "rss_kb=$(awk '/^VmRSS:/{print $2}' /proc/1/status); "
            "fd_count=$(find /proc/1/fd -maxdepth 1 -type l | wc -l); "
            "printf '{\"rss_kb\":%s,\"fd_count\":%s,\"state_files\":[' "
            "\"${rss_kb:-0}\" \"$fd_count\"; "
            "first=1; for path in /var/lib/holon/state/runtime.sqlite*; do "
            "[ -e \"$path\" ] || continue; "
            "size=$(stat -c %s \"$path\"); "
            "name=$(basename \"$path\"); "
            "[ $first -eq 1 ] || printf ','; first=0; "
            "printf '{\"name\":\"%s\",\"bytes\":%s}' \"$name\" \"$size\"; "
            "done; printf ']}'",
            check=False,
            timeout=15,
        )
        if process.returncode == 0 and process.stdout.strip():
            telemetry["container_process"] = json.loads(process.stdout)
        else:
            telemetry["container_process_error"] = (
                process.stdout + process.stderr
            ).strip()
        write_json(self.evidence / f"{label}-telemetry.json", telemetry)
        return telemetry

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

    def seed_scheduler_restart_fixture(
        self,
        label: str,
        *,
        agent: str,
        checkpoint: str,
        stage: str = "prepare",
        objective: str,
    ) -> dict[str, Any]:
        return self.offline_debug(
            label,
            "scheduler-restart-fixture",
            "--agent",
            agent,
            "--checkpoint",
            checkpoint,
            "--stage",
            stage,
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
        if self.provider_mode == "stub":
            require(bool(self.stub_scenario), "stub provider mode requires stub_scenario")
            stub_state = self.docker(
                "inspect",
                "--format",
                "{{.State.Running}}",
                self.stub_container,
                check=False,
            )
            if stub_state.returncode != 0:
                self.docker(
                    "run",
                    "--detach",
                    "--name",
                    self.stub_container,
                    "--network",
                    self.network,
                    "--network-alias",
                    "provider-stub",
                    "--volume",
                    f"{self.evidence}:/data",
                    OPENAI_STUB_IMAGE,
                    "--scenario",
                    self.stub_scenario,
                )
            else:
                require(
                    stub_state.stdout.strip() == "true",
                    "deterministic provider stub stopped during case restart",
                )
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                ready = self.docker(
                    "exec",
                    self.stub_container,
                    "python",
                    "-c",
                    "import urllib.request;"
                    "urllib.request.urlopen('http://127.0.0.1:8080/healthz')",
                    check=False,
                )
                if ready.returncode == 0:
                    break
                time.sleep(0.2)
            else:
                raise TimeoutError("deterministic provider stub did not become ready")
            self.model = "openai/gpt-5.4"
            self.runtime_env["HOLON_OPENAI_BASE_URL"] = (
                self.stub_provider_base_url or "http://provider-stub:8080/v1"
            )
            self.runtime_env["OPENAI_API_KEY"] = "deterministic-test-key"
        if (
            (self.runtime_config or self.model_runtime_override)
            and not self._model_runtime_override_seeded
        ):
            config = merged_runtime_config(
                self.runtime_config,
                self.model,
                self.model_runtime_override,
            )
            self.docker(
                "run",
                "--rm",
                "--volume",
                f"{self.volume}:/var/lib/holon",
                "--entrypoint",
                "bash",
                self.image,
                "-lc",
                "set -euo pipefail; umask 077; "
                "printf '%s' \"$1\" > /var/lib/holon/config.json",
                "bash",
                json.dumps(config, separators=(",", ":")),
            )
            self._model_runtime_override_seeded = True
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
            "HOLON_DISABLE_PROVIDER_FALLBACK="
            + str(self.disable_provider_fallback).lower(),
            "--publish",
            "127.0.0.1::7878",
            "--volume",
            f"{self.volume}:/var/lib/holon",
            "--volume",
            f"{self.workspace_parent}:/acceptance",
        ]
        if self.model_fallbacks:
            args.extend(
                [
                    "--env",
                    "HOLON_MODEL_FALLBACKS="
                    + json.dumps(self.model_fallbacks, separators=(",", ":")),
                ]
            )
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
        if self.provider_mode == "stub":
            self.docker("rm", "-f", self.stub_container, check=False)
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
        if self.provider_mode == "stub":
            residuals.append(
                (
                    "container",
                    self.stub_container,
                    self.docker("inspect", self.stub_container, check=False),
                )
            )
        for kind, name, result in residuals:
            if result.returncode == 0:
                errors.append(f"{kind} still exists after cleanup: {name}")
        return {"status": "fail" if errors else "completed", "errors": errors}

    def assert_stub_complete(self) -> None:
        if self.provider_mode != "stub":
            return
        result = self.docker(
            "exec",
            self.stub_container,
            "python",
            "-c",
            "import json,urllib.request;"
            "print(json.dumps(json.load(urllib.request.urlopen('http://127.0.0.1:8080/status'))))",
        )
        status = json.loads(result.stdout)
        write_json(self.evidence / "stub-status.json", status)
        require(status.get("complete") is True, f"stub transcript was not fully consumed: {status}")
        require(
            status.get("extra_requests") == 0,
            f"stub received requests after transcript exhaustion: {status}",
        )

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
        retry_deadline = time.monotonic() + min(self.timeout_seconds, 30)
        while True:
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
                    response_headers = response.headers
            except urllib.error.HTTPError as error:
                status = error.code
                payload = error.read()
                response_headers = error.headers
            except urllib.error.URLError:
                self.check_docker_health("request-failure")
                raise
            if status != 429 or time.monotonic() >= retry_deadline:
                break
            try:
                error_body = json.loads(payload)
            except json.JSONDecodeError:
                break
            if (
                error_body.get("code") != "projection_busy"
                or error_body.get("retryable") is not True
            ):
                break
            retry_after = response_headers.get("Retry-After", "1")
            try:
                retry_delay = max(0.05, float(retry_after))
            except ValueError:
                retry_delay = 1.0
            time.sleep(min(retry_delay, max(0.0, retry_deadline - time.monotonic())))
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

    def wait_agent_asleep(self, *, timeout: float = 30) -> None:
        """Wait until the agent reaches the *asleep* status specifically.

        ``wait_agent_idle`` returns for any terminal status (including
        ``awake_idle``), but external wake hints fired while the agent is
        still transitioning from ``awake_idle`` to ``asleep`` can be lost.
        Call this before ``fire_callback`` to close that race window.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            state = self.request("GET", self.agent_path("state"))
            agent = state["agent"]["agent"]
            if agent["status"] == "asleep":
                return
            time.sleep(0.5)
        raise TimeoutError(f"agent did not reach asleep within {timeout} s")

    def wait_queue_drained(self, *, stable_checks: int = 3) -> None:
        """Wait until the agent is idle *and* the queue stays empty for
        ``stable_checks`` consecutive polls.  This closes the race window
        where ``wait_agent_idle`` returns between a message being enqueued
        and the scheduler claiming it."""
        deadline = time.monotonic() + 90
        consecutive = 0
        while time.monotonic() < deadline:
            state = self.request("GET", self.agent_path("state"))
            agent = state["agent"]["agent"]
            idle = (
                agent["status"] in TERMINAL_STATUSES
                and agent.get("current_run_id") is None
                and int(state["session"]["pending_count"]) == 0
            )
            consecutive = consecutive + 1 if idle else 0
            if consecutive >= stable_checks:
                return
            time.sleep(1)
        raise TimeoutError("queue did not drain within 90 s")

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
            if len(matches) > 1:
                write_json(self.evidence / f"{label}-duplicate-work-items.json", matches)
                self.capture_context(f"{label}-duplicate")
                raise AssertionError(
                    f"multiple WorkItems matched {objective_marker}: "
                    + ", ".join(item.get("id", "<missing-id>") for item in matches)
                )
            if len(matches) == 1 and matches[0].get("state") == expected_state:
                write_json(self.evidence / f"{label}-work-items.json", items)
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
            if len(matches) > 1:
                write_json(self.evidence / f"{label}-duplicate-work-items.json", matches)
                self.capture_context(f"{label}-duplicate")
                raise AssertionError(
                    f"multiple WorkItems matched {objective_marker}: "
                    + ", ".join(item.get("id", "<missing-id>") for item in matches)
                )
            if matches and matches[0].get("state") == "completed":
                write_json(self.evidence / f"{label}-premature-work-items.json", items)
                self.capture_context(f"{label}-premature")
                raise AssertionError(
                    f"WorkItem {objective_marker} completed before reaching "
                    f"{expected_scheduling_state}; the agent likely skipped WaitFor"
                )
            if (
                len(matches) == 1
                and matches[0].get("scheduling_state")
                == expected_scheduling_state
            ):
                write_json(self.evidence / f"{label}-work-items.json", items)
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

    def events(
        self,
        label: str,
        *,
        after_seq: int | None = None,
        limit: int = CONTEXT_EVENT_LIMIT,
    ) -> list[dict[str, Any]]:
        query = f"events?limit={limit}&order=asc"
        if after_seq is not None:
            query += f"&after_seq={after_seq}"
        page = self.request("GET", self.agent_path(query))
        write_json(self.evidence / f"{label}-events.json", page)
        return page["events"]

    def event_cursor(self) -> int:
        page = self.request("GET", self.agent_path("events?limit=1&order=desc"))
        return int(page.get("cursor_seq") or 0)

    def event_batch(
        self,
        label: str,
        *,
        after_seq: int,
        limit: int = CONTEXT_EVENT_LIMIT,
    ) -> dict[str, Any]:
        events: list[dict[str, Any]] = []
        cursor = after_seq
        final_cursor = after_seq
        while True:
            page = self.request(
                "GET",
                self.agent_path(
                    f"events?limit={limit}&order=asc&after_seq={cursor}"
                ),
            )
            events.extend(page["events"])
            newest_seq = page.get("newest_seq")
            if isinstance(newest_seq, int):
                final_cursor = newest_seq
            if not page.get("has_newer"):
                break
            require(
                isinstance(newest_seq, int) and newest_seq > cursor,
                f"event pagination did not advance after {cursor}: {page}",
            )
            cursor = newest_seq
        batch = {
            "events": events,
            "after_seq": after_seq,
            "newest_seq": final_cursor,
        }
        write_json(self.evidence / f"{label}-events.json", batch)
        return batch

    def capture_context(
        self,
        label: str,
        *,
        after_seq: int | None = None,
        include_conversation: bool = True,
    ) -> None:
        if include_conversation:
            write_json(
                self.evidence / f"{label}-briefs.json",
                self.request(
                    "GET",
                    self.agent_path(f"briefs?limit={CONTEXT_BRIEF_LIMIT}"),
                ),
            )
            write_json(
                self.evidence / f"{label}-transcript.json",
                self.request(
                    "GET",
                    self.agent_path(f"transcript?limit={CONTEXT_TRANSCRIPT_LIMIT}"),
                ),
            )
        self.state(label)
        self.work_items(label)
        self.events(label, after_seq=after_seq)

    def prompt(
        self,
        label: str,
        text: str,
        *,
        work_item_id: str | None = None,
    ) -> tuple[int, dict[str, Any]]:
        baseline_event_seq = self.event_cursor()
        before = self.state(f"{label}-before")
        baseline = int(before["agent"]["agent"]["turn_index"])
        body = {"text": text}
        if work_item_id is not None:
            body["work_item_id"] = work_item_id
        response = self.request(
            "POST",
            self.agent_path("prompt", control=True),
            body,
        )
        message_id = response.get("message_id")
        require(
            isinstance(message_id, str) and message_id,
            f"prompt response omitted message_id: {response}",
        )
        write_json(self.evidence / f"{label}-prompt-response.json", response)
        (self.evidence / f"{label}-prompt.txt").write_text(text + "\n")

        deadline = time.monotonic() + self.timeout_seconds
        last_state = before
        baseline_failure_at = (
            (before["agent"]["agent"].get("last_runtime_failure") or {}).get(
                "occurred_at"
            )
        )
        target_turn_id: str | None = None
        target_turn_index: int | None = None
        event_poll_cursor = baseline_event_seq
        while time.monotonic() < deadline:
            last_state = self.request("GET", self.agent_path("state"))
            failure = last_state["agent"]["agent"].get("last_runtime_failure")
            if failure and failure.get("occurred_at") != baseline_failure_at:
                write_json(
                    self.evidence / f"{label}-runtime-failure-state.json",
                    last_state,
                )
                raise AssertionError(
                    f"runtime failure occurred while waiting for {label}: "
                    f"{failure.get('summary', 'unknown runtime failure')}"
                )
            page = self.event_batch(
                f"{label}-terminal-poll",
                after_seq=event_poll_cursor,
            )
            phase_events = page["events"]
            event_poll_cursor = int(page["newest_seq"])
            for event in phase_events:
                payload = event.get("payload", {})
                if (
                    event.get("type") == "turn_started"
                    and payload.get("message_id") == message_id
                ):
                    target_turn_id = payload.get("turn_id")
                    target_turn_index = int(payload.get("turn_index", 0))
            runtime_failures = [
                event
                for event in phase_events
                if event.get("type") == "runtime_error"
                and (
                    event.get("payload", {}).get("message_id") == message_id
                    or (
                        target_turn_id is not None
                        and event.get("payload", {}).get("turn_id")
                        == target_turn_id
                    )
                )
            ]
            if runtime_failures:
                failure = runtime_failures[-1]["payload"]
                source_chain = failure.get("source_chain") or []
                detail = (
                    source_chain[0]
                    if source_chain
                    else failure.get("error", "unknown runtime failure")
                )
                raise AssertionError(
                    f"runtime failure occurred while waiting for {label}: "
                    f"{failure.get('domain', 'unknown')}: {detail}"
                )
            terminal = next(
                (
                    event
                    for event in phase_events
                    if event.get("type") == "turn_terminal"
                    and target_turn_id is not None
                    and event.get("payload", {}).get("turn_id")
                    == target_turn_id
                ),
                None,
            )
            if terminal is not None:
                terminal_kind = terminal.get("payload", {}).get("kind")
                require(
                    terminal_kind == "completed",
                    f"target turn for {label} ended with {terminal_kind}: "
                    f"{terminal.get('payload', {})}",
                )
                self._prompt_scopes[label] = {
                    "message_id": message_id,
                    "turn_id": target_turn_id,
                    "turn_index": target_turn_index,
                    "terminal_kind": terminal_kind,
                }
                write_json(self.evidence / f"{label}-after-state.json", last_state)
                self.capture_context(
                    label,
                    after_seq=baseline_event_seq,
                    include_conversation=False,
                )
                return baseline, last_state
            time.sleep(1)
        write_json(self.evidence / f"{label}-timeout-state.json", last_state)
        self.capture_logs()
        target_state = (
            "target turn was not observed"
            if target_turn_id is None
            else (
                f"target turn {target_turn_id} started at index "
                f"{target_turn_index} but did not reach terminal"
            )
        )
        raise TimeoutError(
            f"timed out after {self.timeout_seconds}s waiting for phase {label}: "
            f"{target_state}"
        )

    def prompt_scope(self, label: str) -> dict[str, Any]:
        scope = self._prompt_scopes.get(label)
        require(scope is not None, f"prompt scope is unavailable for {label}")
        return scope

    def successful_tool_events(
        self,
        label: str,
        baseline_turn: int,
        *,
        end_turn: int | None = None,
        message_id: str | None = None,
        turn_ids: set[str] | None = None,
    ) -> list[dict[str, Any]]:
        events = self.events(f"{label}-tool-check")
        turn_indexes = {
            event["payload"].get("turn_id"): int(
                event["payload"].get("turn_index", 0)
            )
            for event in events
            if event["type"] == "turn_started"
        }
        message_turn_ids = {
            event["payload"].get("turn_id")
            for event in events
            if event["type"] == "turn_started"
            and event["payload"].get("message_id") == message_id
        }
        if message_id is not None:
            require(
                message_turn_ids,
                f"no turn was recorded for message {message_id} in {label}",
            )
        require(
            message_id is None or turn_ids is None,
            "tool assertion accepts message_id or turn_ids, not both",
        )

        def in_scope(event: dict[str, Any]) -> bool:
            payload = event["payload"]
            if message_id is not None:
                return payload.get("turn_id") in message_turn_ids
            if turn_ids is not None:
                return payload.get("turn_id") in turn_ids
            turn_index = int(
                turn_indexes.get(
                    payload.get("turn_id"),
                    payload.get("turn_index", 0),
                )
            )
            return turn_index > baseline_turn and (
                end_turn is None or turn_index <= end_turn
            )

        runtime_failures = [
            event
            for event in events
            if event["type"] == "runtime_error"
            and in_scope(event)
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
            and in_scope(event)
        ]
        require(not failures, f"tool failures occurred in {label}: {failures}")
        return [
            event
            for event in events
            if event["type"] == "tool_executed"
            and event["payload"].get("status") == "success"
            and in_scope(event)
        ]

    def assert_tools(
        self,
        label: str,
        baseline_turn: int,
        expected: list[str],
        forbidden: list[str] | None = None,
        *,
        end_turn: int | None = None,
        message_id: str | None = None,
        turn_ids: set[str] | None = None,
    ) -> list[dict[str, Any]]:
        events = self.successful_tool_events(
            label,
            baseline_turn,
            end_turn=end_turn,
            message_id=message_id,
            turn_ids=turn_ids,
        )
        actual = [event["payload"].get("tool_name") for event in events]
        missing = [name for name in expected if name not in actual]
        forbidden_actual = [name for name in (forbidden or []) if name in actual]
        if self.tool_assertion_mode == "observe":
            write_json(
                self.evidence / f"{label}-tool-observation.json",
                {
                    "mode": "observe",
                    "expected": expected,
                    "forbidden": forbidden or [],
                    "actual": actual,
                    "missing": missing,
                    "forbidden_actual": forbidden_actual,
                },
            )
            return events
        require(not missing, f"{label} missing successful tools {missing}; got {actual}")
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
        try:
            for name in ("runtime.sqlite", "runtime.sqlite-wal", "runtime.sqlite-shm"):
                result = self.docker(
                    "cp",
                    f"{self.container}:/var/lib/holon/state/{name}",
                    str(snapshot_dir / name),
                    check=False,
                    timeout=RUNTIME_DB_COPY_TIMEOUT_SECONDS,
                )
                if name == "runtime.sqlite":
                    require(
                        result.returncode == 0,
                        "runtime database snapshot is missing: "
                        + (result.stderr or result.stdout).strip(),
                    )
        except subprocess.TimeoutExpired as error:
            write_json(
                self.evidence / f"{label}-runtime-state-copy-failure.json",
                {
                    "status": "timeout",
                    "timeout_seconds": RUNTIME_DB_COPY_TIMEOUT_SECONDS,
                    "command": error.cmd,
                    "stdout": error.stdout or "",
                    "stderr": error.stderr or "",
                },
            )
            raise
        database = snapshot_dir / "runtime.sqlite"
        require(database.is_file(), "runtime database snapshot is missing")
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
        connection.row_factory = sqlite3.Row
        try:
            snapshot = {
                "schema_revision": connection.execute(
                    "SELECT COALESCE(MAX(version), 0) FROM schema_migrations"
                ).fetchone()[0],
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


def require_turn_local_compaction(
    snapshot: dict[str, Any],
    *,
    label: str,
) -> list[dict[str, Any]]:
    events = [
        json.loads(row["data_json"])["data"]
        for row in snapshot["audit_events"]
        if row["kind"] == "turn_local_compaction_applied"
    ]
    require(
        events and any(int(event.get("compacted_rounds") or 0) > 0 for event in events),
        f"{label} compaction stimulus did not produce compacted rounds: {events}",
    )
    return events


def require_scheduler_engine_activation_chain(
    harness: CaseHarness,
    snapshot: dict[str, Any],
    *,
    work_item_id: str,
    expected_admission_kinds: tuple[str, ...],
    lifecycle_message_ids: set[str] | None = None,
) -> None:
    if harness.canonical_scheduler_enabled:
        require_scheduler_activation_chain(
            snapshot,
            agent_id=harness.agent_id,
            work_item_id=work_item_id,
            expected_admission_kinds=expected_admission_kinds,
            lifecycle_message_ids=lifecycle_message_ids or set(),
        )
        return
    activations = [
        row
        for row in snapshot["scheduler_activations"]
        if row["work_item_id"] == work_item_id
    ]
    demands = [
        row
        for row in snapshot["scheduler_work_demands"]
        if row["work_item_id"] == work_item_id
    ]
    require(
        not activations and not demands,
        f"legacy scheduler wrote canonical execution state: "
        f"activations={activations}, demands={demands}",
    )
    lifecycle_activation_ids = {
        f"activation:message:{message_id}"
        for message_id in lifecycle_message_ids or set()
    }
    require(
        not [
            row
            for row in snapshot["scheduler_activations"]
            if row["activation_id"] in lifecycle_activation_ids
        ],
        "legacy scheduler wrote canonical lifecycle activations",
    )


def require_scheduler_engine_wait_resolution(
    harness: CaseHarness,
    snapshot: dict[str, Any],
    *,
    work_item_id: str,
    wait_ids: set[str],
) -> None:
    canonical_waits = [
        row
        for row in snapshot["scheduler_wait_generations"]
        if row["wait_id"] in wait_ids or row["owner_work_item_id"] == work_item_id
    ]
    if harness.canonical_scheduler_enabled:
        require(
            len(canonical_waits) == len(wait_ids)
            and all(row["lifecycle_state"] == "resolved" for row in canonical_waits)
            and all(row["consuming_activation_id"] is None for row in canonical_waits),
            f"canonical waits did not resolve exactly once: {canonical_waits}",
        )
        return
    require(
        not canonical_waits,
        f"legacy scheduler wrote canonical wait generations: {canonical_waits}",
    )


def require_scheduler_wait_terminal(
    harness: CaseHarness,
    snapshot: dict[str, Any],
    *,
    work_item_id: str,
    wait_kind: str,
    require_callback_trigger: bool = False,
    callback_external_trigger_id: str | None = None,
) -> list[dict[str, Any]]:
    waits = [
        row
        for row in snapshot["wait_conditions"]
        if row["work_item_id"] == work_item_id and row["kind"] == wait_kind
    ]
    require(
        len(waits) == 1,
        f"expected one {wait_kind} wait for {work_item_id}: {waits}",
    )
    wait = waits[0]
    if harness.canonical_scheduler_enabled:
        require(
            wait["status"] == "resolved",
            f"canonical {wait_kind} wait did not resolve: {wait}",
        )
    else:
        require(
            wait["status"] in {"resolved", "cancelled"},
            f"legacy {wait_kind} wait did not reach a terminal state: {wait}",
        )
    if not harness.canonical_scheduler_enabled and wait["status"] == "cancelled":
        cancellation_events = [
            json.loads(row["data_json"])["data"]
            for row in snapshot["audit_events"]
            if row["kind"] == "wait_conditions_cancelled"
        ]
        require(
            any(
                event.get("work_item_id") == work_item_id
                and event.get("reason")
                in {"completion_intent_recorded", "work_item_completed"}
                and wait["wait_condition_id"]
                in event.get("wait_condition_ids", [])
                for event in cancellation_events
            ),
            f"legacy {wait_kind} cancellation lacked completion evidence: "
            f"wait={wait}, cancellations={cancellation_events}",
        )
    if require_callback_trigger:
        require(
            callback_external_trigger_id is not None,
            "callback trigger evidence requires an external trigger id",
        )
        callback_events = [
            json.loads(row["data_json"])["data"]
            for row in snapshot["audit_events"]
            if row["kind"] == "callback_delivered"
        ]
        require(
            any(
                event.get("disposition") == "triggered"
                and event.get("external_trigger_id")
                == callback_external_trigger_id
                for event in callback_events
            ),
            f"legacy {wait_kind} wait lacked callback trigger evidence: "
            f"external_trigger_id={callback_external_trigger_id}, "
            f"callbacks={callback_events}",
        )
    return waits


def require_scheduler_activation_chain(
    snapshot: dict[str, Any],
    *,
    agent_id: str,
    work_item_id: str,
    expected_admission_kinds: tuple[str, ...],
    lifecycle_message_ids: set[str],
) -> list[dict[str, Any]]:
    work_item_activations = [
        row
        for row in snapshot["scheduler_activations"]
        if row["work_item_id"] == work_item_id
    ]
    lifecycle_activation_ids = {
        f"activation:message:{message_id}" for message_id in lifecycle_message_ids
    }
    lifecycle_activations = [
        row
        for row in snapshot["scheduler_activations"]
        if row["activation_id"] in lifecycle_activation_ids
    ]
    require(
        len(lifecycle_activations) == len(lifecycle_activation_ids)
        and all(
            row["work_item_id"] is None
            and row["lifecycle_state"] == "settled"
            for row in lifecycle_activations
        ),
        "canonical lifecycle activations did not settle without claiming a "
        f"WorkItem: {lifecycle_activations}",
    )
    require(
        sorted(row["admission_kind"] for row in work_item_activations)
        == sorted(expected_admission_kinds)
        and all(
            row["lifecycle_state"] == "settled"
            for row in work_item_activations
        ),
        "canonical WorkItem activation lineage did not match the expected "
        f"admission kinds {expected_admission_kinds}: {work_item_activations}",
    )
    activations = work_item_activations + lifecycle_activations
    activation_ids = {row["activation_id"] for row in activations}
    settlements = [
        row
        for row in snapshot["scheduler_activation_settlements"]
        if row["activation_id"] in activation_ids
    ]
    require(
        len(settlements) == len(activations),
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


def require_checkpoint_restart_activation_lineage(
    before_restart_snapshot: dict[str, Any],
    snapshot: dict[str, Any],
    *,
    work_item_id: str,
    wait_id: str,
) -> None:
    before_restart_activations = [
        row
        for row in before_restart_snapshot["scheduler_activations"]
        if row["work_item_id"] == work_item_id
    ]
    require(
        len(before_restart_activations) == 1
        and before_restart_activations[0]["admitted_generation"] == 1
        and before_restart_activations[0]["admission_kind"] == "scheduling"
        and str(before_restart_activations[0]["idempotency_key"]).startswith(
            "work-queue-attempt:"
        ),
        "checkpoint replay restart boundary did not preserve exactly the initial "
        f"scheduling activation: {before_restart_activations}",
    )
    activations = sorted(
        (
            row
            for row in snapshot["scheduler_activations"]
            if row["work_item_id"] == work_item_id
        ),
        key=lambda row: row["admitted_generation"],
    )
    require(
        len(activations) == 2,
        f"checkpoint replay expected exactly two WorkItem activations: {activations}",
    )
    scheduling, wait_resume = activations
    require(
        scheduling["admitted_generation"] == 1
        and scheduling["admission_kind"] == "scheduling"
        and str(scheduling["idempotency_key"]).startswith("work-queue-attempt:"),
        f"checkpoint replay initial scheduling activation mismatch: {scheduling}",
    )
    expected_wait_resume_key = (
        f"wait-resume:{wait_id}:2:{wait_resume['activation_id']}"
    )
    require(
        wait_resume["admitted_generation"] == 2
        and wait_resume["admission_kind"] == "wait_resume"
        and wait_resume["idempotency_key"] == expected_wait_resume_key,
        f"checkpoint replay wait-resume activation mismatch: {wait_resume}",
    )
    wait_generations = [
        row
        for row in snapshot["scheduler_wait_generations"]
        if row["wait_id"] == wait_id
    ]
    require(
        len(wait_generations) == 1
        and wait_generations[0]["owner_work_item_id"] == work_item_id
        and wait_generations[0]["generation"] == 2
        and wait_generations[0]["lifecycle_state"] == "resolved",
        f"checkpoint replay wait generation mismatch: {wait_generations}",
    )


def require_lifecycle_wait_adoption(
    snapshot: dict[str, Any],
    *,
    agent_id: str,
    work_item_id: str,
    wait: dict[str, Any],
) -> None:
    source_turn_id = wait.get("last_turn_id")
    require(
        isinstance(source_turn_id, str) and source_turn_id,
        f"adopted wait omitted its source turn: {wait}",
    )
    source_turns = [
        row
        for row in snapshot["turn_records"]
        if row["agent_id"] == agent_id and row["turn_id"] == source_turn_id
    ]
    require(
        len(source_turns) == 1,
        f"adopted wait source turn is missing or duplicated: {source_turns}",
    )
    source_message_id = source_turns[0]["trigger_message_id"]
    activation_id = f"activation:message:{source_message_id}"
    activations = [
        row
        for row in snapshot["scheduler_activations"]
        if row["agent_id"] == agent_id and row["activation_id"] == activation_id
    ]
    require(
        len(activations) == 1
        and activations[0]["work_item_id"] is None
        and activations[0]["lifecycle_state"] == "settled"
        and activations[0]["admitted_generation"] > 0,
        f"lifecycle adoption source activation is invalid: {activations}",
    )
    settlements = [
        row
        for row in snapshot["scheduler_activation_settlements"]
        if row["agent_id"] == agent_id and row["activation_id"] == activation_id
    ]
    settlement = json.loads(settlements[0]["payload_json"]) if len(settlements) == 1 else {}
    require(
        len(settlements) == 1
        and settlement.get("turn_terminal") == source_turn_id
        and settlement.get("disposition") == {"kind": "work_continues"}
        and settlement.get("agent_dispatch") == {"kind": "open"}
        and settlement.get("operator_delivery") is None,
        f"lifecycle adoption source did not settle WorkContinues: {settlements}",
    )
    adoption_rows = [
        row
        for row in snapshot["scheduler_protocol_command_results"]
        if row["agent_id"] == agent_id
        and row["command_kind"] == "adopt_activation_work_state"
        and row["command_identity"] == f"{activation_id}:{work_item_id}"
    ]
    require(
        len(adoption_rows) == 1
        and adoption_rows[0]["decision"] == "legacy_work_state_adopted"
        and adoption_rows[0]["conflict_kind"] is None
        and adoption_rows[0]["conflict_code"] is None,
        f"lifecycle wait adoption command is missing or conflicted: {adoption_rows}",
    )
    post_state = json.loads(adoption_rows[0]["post_state_fence_json"])
    adopted_work = post_state.get("work", {}).get(work_item_id, {})
    adopted_generation = adopted_work.get("scheduling_generation")
    references = json.loads(adoption_rows[0]["result_references_json"])
    require(
        isinstance(adopted_generation, int)
        and adopted_generation > 0
        and adopted_work.get("metadata_revision") == adopted_generation
        and adopted_work.get("status")
        == {"kind": "waiting", "wait_id": wait["wait_condition_id"]}
        and set(references)
        == {
            f"activation:{activation_id}",
            f"work:{work_item_id}",
            f"wait:{wait['wait_condition_id']}:generation:{adopted_generation}",
        },
        f"lifecycle adoption did not preserve wait ownership/generation: {adoption_rows}",
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
    harness.assert_tools(
        "runtime-delivery",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("runtime-delivery")["message_id"],
    )

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
    events = harness.assert_tools(
        "memory-search",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("memory-search")["message_id"],
    )
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
    events = harness.assert_tools(
        "memory-recover",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("memory-recover")["message_id"],
    )
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
        "workspace-create",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("workspace-create")["message_id"],
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
    harness.assert_tools(
        "workspace-recover",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("workspace-recover")["message_id"],
    )
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
    harness.assert_tools(
        "workitem-wait",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("workitem-wait")["message_id"],
    )
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
    harness.assert_tools(
        "workitem-complete",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("workitem-complete")["message_id"],
    )
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


def work_queue_message_evidence(
    snapshot: dict[str, Any],
    *,
    work_item_id: str,
    reason: str,
) -> list[dict[str, Any]]:
    queue_status = {
        row["message_id"]: row["status"] for row in snapshot["queue_entries"]
    }
    evidence = []
    for row in snapshot["messages"]:
        if row.get("work_item_id") != work_item_id:
            continue
        try:
            payload = json.loads(row["payload_json"])
        except (TypeError, json.JSONDecodeError):
            continue
        work_queue = (payload.get("metadata") or {}).get("work_queue") or {}
        if work_queue.get("reason") != reason:
            continue
        evidence.append(
            {
                "message_id": row["message_id"],
                "idempotency_key": work_queue.get("idempotency_key"),
                "status": queue_status.get(row["message_id"]),
            }
        )
    return evidence


def recovered_retry_ticks(
    failed_ticks: list[dict[str, Any]],
    recovered_ticks: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    failed_keys = {
        tick["idempotency_key"]
        for tick in failed_ticks
        if tick["status"] in {"aborted", "interrupted"}
    }
    return [
        tick
        for tick in recovered_ticks
        if tick["status"] == "processed"
        and tick["idempotency_key"] in failed_keys
    ]


def run_scheduler_task_wait_resume_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_marker = f"SCHEDULER-TASK-WAIT-{marker}"
    task_marker = f"SCHEDULER-TASK-RESULT-{marker}"
    completion_marker = f"SCHEDULER-TASK-WAIT-COMPLETE-{marker}"
    objective = (
        f"{objective_marker}. On the first autonomous work_queue turn, call "
        f"ExecCommand with command `sleep 15; printf {task_marker}`, "
        "yield_time_ms=50, and a bounded output limit. Use the returned promoted "
        "task_id to call WaitFor with wake=task_result. Do not poll the task or "
        "complete the WorkItem. On the task-result rejoin, call GetWorkItem for "
        "the current WorkItem, then call WaitFor with wake=external, "
        f"resource=docker-e2e:{marker}, and a concrete reason. On the external "
        "wake, call GetWorkItem, update both existing todos to completed, then "
        "emit a concise completion result containing "
        f"{completion_marker} immediately followed by CompleteWorkItem for the "
        "exact current item. Do not create another WorkItem."
    )
    callback = harness.reset_callback("scheduler-task-wait-callback")
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-task-wait-seed",
        phase["prompt"].format(
            case_id=case["id"],
            objective=json.dumps(objective, ensure_ascii=False),
            completion_marker=completion_marker,
        ),
    )
    task_waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_marker,
        expected_scheduling_state="waiting_task",
        label="scheduler-task-wait-task",
    )
    external_waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_marker,
        expected_scheduling_state="waiting_external",
        label="scheduler-task-wait-external",
    )
    require(
        external_waiting["id"] == task_waiting["id"],
        "task-result rejoin changed WorkItem identity",
    )
    harness.wait_agent_asleep()
    harness.fire_callback(
        "scheduler-task-wait-wake",
        callback["trigger_url"],
        {"case_id": case["id"], "marker": marker},
    )
    item = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="scheduler-task-wait-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools(
        "scheduler-task-wait-seed",
        baseline,
        ["CreateWorkItem"],
        forbidden
        + [
            "ExecCommand",
            "WaitFor",
            "GetWorkItem",
            "UpdateWorkItem",
            "CompleteWorkItem",
        ],
        message_id=harness.prompt_scope("scheduler-task-wait-seed")[
            "message_id"
        ],
    )
    work_item_id = item["id"]
    result_brief_id = item.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"task/wait WorkItem omitted result brief: {item}",
    )
    result_brief = harness.brief(result_brief_id, "scheduler-task-wait-result")
    require(
        result_brief.get("work_item_id") == work_item_id
        and completion_marker in (result_brief.get("text") or ""),
        f"task/wait completion brief mismatch: {result_brief}",
    )

    snapshot = harness.runtime_db_snapshot("scheduler-task-wait")
    turns = [
        row
        for row in snapshot["turn_records"]
        if row.get("current_work_item_id") == work_item_id
        and row.get("terminal_kind") == "completed"
    ]
    message_ids = {row["trigger_message_id"] for row in turns}
    require(
        len(turns) == 3 and None not in message_ids,
        f"expected autonomous, task-result, and external-wake turns: {turns}",
    )
    harness.assert_tools(
        "scheduler-task-wait-continuations",
        baseline,
        [name for name in required if name != "CreateWorkItem"],
        forbidden + ["CreateWorkItem"],
        turn_ids={row["turn_id"] for row in turns},
    )
    require_processed_queue_entries(snapshot["queue_entries"], message_ids)
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
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_id,
        expected_admission_kinds=("scheduling", "wait_resume", "wait_resume"),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-task-wait-seed")["message_id"]
        },
    )
    if harness.canonical_scheduler_enabled:
        activation_causes = [
            json.loads(row["payload_json"])["activation"]["cause"]["kind"]
            for row in snapshot["scheduler_activations"]
            if row["work_item_id"] == work_item_id
        ]
        require(
            sorted(activation_causes)
            == ["task_rejoin", "wait_resume", "work_item_runnable"],
            "canonical task/wait activation causes did not preserve scheduling, "
            f"task-rejoin, and external-resume provenance: {activation_causes}",
        )
    task_waits = require_scheduler_wait_terminal(
        harness,
        snapshot,
        work_item_id=work_item_id,
        wait_kind="task",
    )
    external_waits = require_scheduler_wait_terminal(
        harness,
        snapshot,
        work_item_id=work_item_id,
        wait_kind="external",
        require_callback_trigger=True,
        callback_external_trigger_id=callback["external_trigger_id"],
    )
    waits = task_waits + external_waits
    require_scheduler_engine_wait_resolution(
        harness,
        snapshot,
        work_item_id=work_item_id,
        wait_ids={wait["wait_condition_id"] for wait in waits},
    )
    harness.restart()
    restarted_items = harness.work_items("scheduler-task-wait-after-restart")
    restarted = next(item for item in restarted_items if item["id"] == work_item_id)
    require(
        restarted["state"] == "completed"
        and restarted.get("result_brief_id") == result_brief_id,
        f"task/wait WorkItem did not survive restart: {restarted}",
    )


def run_scheduler_provider_failure_retry_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    harness.initialize_workspace()
    base_url_env = provider_base_url_env(harness.model)
    harness.start()
    original_base_url = harness.runtime_env.get(base_url_env)
    original_stub_provider_base_url = harness.stub_provider_base_url

    harness.request(
        "POST",
        harness.agent_path("control", control=True),
        {"action": "stop"},
    )
    marker = secrets.token_hex(4)
    objective_marker = f"SCHEDULER-PROVIDER-RETRY-{marker}"
    completion_marker = f"SCHEDULER-PROVIDER-RETRY-COMPLETE-{marker}"
    objective = (
        f"{objective_marker}. This WorkItem is being resumed by an autonomous "
        "work_queue retry after a provider transport failure. Call ListWorkItems "
        "with filter current, then emit a concise completion report containing "
        f"{completion_marker} immediately followed by CompleteWorkItem for the "
        "exact current WorkItem. Do not create another WorkItem or wait for input."
    )
    created = harness.request(
        "POST",
        harness.agent_path("work-items", control=True),
        {"objective": objective},
    )
    write_json(harness.evidence / "provider-retry-created.json", created)
    work_item_id = created["id"]
    picked = harness.request(
        "POST",
        harness.agent_path(f"work-items/{work_item_id}/pick", control=True),
        {"reason": "prepare deterministic provider failure retry"},
    )
    write_json(harness.evidence / "provider-retry-picked.json", picked)
    failure_event_cursor = harness.event_cursor()
    harness.stop()
    if harness.provider_mode == "stub":
        harness.stub_provider_base_url = "http://127.0.0.1:9"
    else:
        harness.runtime_env[base_url_env] = "http://127.0.0.1:9"
    harness.start(wait_idle=False)
    harness.request(
        "POST",
        harness.agent_path("control", control=True),
        {"action": "start"},
    )

    deadline = time.monotonic() + harness.timeout_seconds
    runtime_errors: list[dict[str, Any]] = []
    target_message_ids: set[str] = set()
    while time.monotonic() < deadline:
        events = harness.events(
            "provider-retry-failure-poll",
            after_seq=failure_event_cursor,
        )
        target_message_ids.update(
            event["payload"]["message_id"]
            for event in events
            if event["type"] == "message_admitted"
            and event["payload"].get("work_item_id") == work_item_id
            and isinstance(event["payload"].get("message_id"), str)
        )
        runtime_errors = [
            event
            for event in events
            if event["type"] == "runtime_error"
            and event["payload"].get("message_id") in target_message_ids
        ]
        if runtime_errors:
            break
        time.sleep(0.5)
    require(
        runtime_errors,
        "invalid provider endpoint did not produce a runtime_error for the "
        f"target WorkItem; admitted messages={sorted(target_message_ids)}",
    )
    failed_message_id = runtime_errors[-1]["payload"]["message_id"]
    harness.request(
        "POST",
        harness.agent_path("control", control=True),
        {"action": "stop"},
    )
    failed = harness.runtime_db_snapshot("provider-retry-failed")
    failed_item = next(
        row for row in failed["work_items"] if row["work_item_id"] == work_item_id
    )
    require(
        failed_item["state"] == "open",
        f"provider failure closed the retry WorkItem: {failed_item}",
    )
    failed_ticks = work_queue_message_evidence(
        failed,
        work_item_id=work_item_id,
        reason="continue_active",
    )
    require(
        failed_ticks
        and any(tick["status"] == "aborted" for tick in failed_ticks)
        and not any(tick["status"] == "processed" for tick in failed_ticks),
        f"provider failure did not abort continue-active ticks: {failed_ticks}",
    )
    require(
        failed_message_id in {tick["message_id"] for tick in failed_ticks},
        "runtime_error did not belong to the target continue-active message: "
        f"error_message={failed_message_id}, ticks={failed_ticks}",
    )
    failed_keys = {tick["idempotency_key"] for tick in failed_ticks}
    require(
        len(failed_keys) == 1 and None not in failed_keys,
        f"failed continue-active ticks lost stable idempotency: {failed_ticks}",
    )

    harness.stop()
    if harness.provider_mode == "stub":
        harness.stub_provider_base_url = original_stub_provider_base_url
    elif original_base_url is None:
        harness.runtime_env.pop(base_url_env, None)
    else:
        harness.runtime_env[base_url_env] = original_base_url
    harness.start(wait_idle=False)
    harness.request(
        "POST",
        harness.agent_path("control", control=True),
        {"action": "start"},
    )
    completed = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="provider-retry-completed",
    )
    result_brief_id = completed.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"provider retry completion omitted result brief: {completed}",
    )
    result_brief = harness.brief(result_brief_id, "provider-retry-result")
    require(
        completion_marker in (result_brief.get("text") or ""),
        f"provider retry completion marker is missing: {result_brief}",
    )
    recovered = harness.runtime_db_snapshot("provider-retry-recovered")
    recovered_ticks = work_queue_message_evidence(
        recovered,
        work_item_id=work_item_id,
        reason="continue_active",
    )
    processed_ticks = recovered_retry_ticks(failed_ticks, recovered_ticks)
    require(
        processed_ticks,
        "provider recovery did not process a stable-idempotency retry after "
        f"an aborted/interrupted attempt: failed={failed_ticks}, "
        f"recovered={recovered_ticks}",
    )
    require(
        {tick["idempotency_key"] for tick in recovered_ticks} == failed_keys,
        f"provider recovery changed continue-active idempotency: {recovered_ticks}",
    )
    required, forbidden = phase_tools(case["phases"][0])
    harness.assert_tools(
        "provider-retry-tools",
        0,
        required,
        forbidden,
        message_id=processed_ticks[-1]["message_id"],
    )


def run_scheduler_multi_workitem_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_a_marker = f"SCHEDULER-MULTI-A-{marker}"
    objective_b_marker = f"SCHEDULER-MULTI-B-{marker}"
    completion_a = f"SCHEDULER-MULTI-COMPLETE-A-{marker}"
    completion_b = f"SCHEDULER-MULTI-COMPLETE-B-{marker}"
    objective_a = (
        f"{objective_a_marker}. Inspect the current agent identity by calling "
        "AgentGet, then call ListWorkItems with filter current to confirm "
        "this WorkItem is the active focus. Complete this WorkItem only after "
        "the Runtime resumes it through an autonomous work_queue SystemTick. "
        "On that autonomous turn, perform the inspection steps, update both "
        f"existing todos to completed, then emit a concise completion result "
        f"containing {completion_a} immediately followed by CompleteWorkItem "
        "for that exact item. Do not wait for more operator input."
    )
    objective_b = (
        f"{objective_b_marker}. Review the agent workspace by calling "
        "GetWorkspaceState to inspect the current projection, then call "
        "ListWorkItems to verify the work item queue. Complete this WorkItem "
        "only after the Runtime resumes it through an autonomous work_queue "
        "SystemTick. On that autonomous turn, perform the review steps, "
        f"update both existing todos to completed, then emit a concise "
        f"completion result containing {completion_b} immediately followed "
        "by CompleteWorkItem for that exact item. Do not wait for more "
        "operator input."
    )
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-multi-create",
        phase["prompt"].format(
            case_id=case["id"],
            objective_a=json.dumps(objective_a, ensure_ascii=False),
            objective_b=json.dumps(objective_b, ensure_ascii=False),
            completion_a=completion_a,
            completion_b=completion_b,
        ),
    )
    item_a = harness.wait_work_item(
        objective_marker=objective_a_marker,
        expected_state="completed",
        label="scheduler-multi-a-completed",
    )
    item_b = harness.wait_work_item(
        objective_marker=objective_b_marker,
        expected_state="completed",
        label="scheduler-multi-b-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools(
        "scheduler-multi-create",
        baseline,
        ["CreateWorkItem"],
        forbidden
        + [
            "AgentGet",
            "GetWorkspaceState",
            "ListWorkItems",
            "UpdateWorkItem",
            "CompleteWorkItem",
        ],
        message_id=harness.prompt_scope("scheduler-multi-create")["message_id"],
    )
    work_item_a_id = item_a["id"]
    work_item_b_id = item_b["id"]
    require(item_a["state"] == "completed", f"WorkItem A not completed: {item_a}")
    require(item_b["state"] == "completed", f"WorkItem B not completed: {item_b}")
    require(work_item_a_id != work_item_b_id, "WorkItem A and B share the same id")
    for item, label_letter, completion, brief_label in (
        (item_a, "A", completion_a, "scheduler-multi-result-a"),
        (item_b, "B", completion_b, "scheduler-multi-result-b"),
    ):
        brief_id = item.get("result_brief_id")
        require(
            isinstance(brief_id, str) and brief_id,
            f"WorkItem {label_letter} omitted result brief: {item}",
        )
        brief = harness.brief(brief_id, brief_label)
        require(
            brief.get("work_item_id") == item["id"]
            and completion in (brief.get("text") or ""),
            f"WorkItem {label_letter} brief mismatch: {brief}",
        )
    snapshot = harness.runtime_db_snapshot("scheduler-multi")
    turn_ids_by_work_item = {
        work_item_id: {
            row["turn_id"]
            for row in snapshot["turn_records"]
            if row["current_work_item_id"] == work_item_id
        }
        for work_item_id in (work_item_a_id, work_item_b_id)
    }
    harness.assert_tools(
        "scheduler-multi-a",
        baseline,
        ["AgentGet", "ListWorkItems", "UpdateWorkItem", "CompleteWorkItem"],
        forbidden + ["CreateWorkItem", "GetWorkspaceState"],
        turn_ids=turn_ids_by_work_item[work_item_a_id],
    )
    harness.assert_tools(
        "scheduler-multi-b",
        baseline,
        [
            "GetWorkspaceState",
            "ListWorkItems",
            "UpdateWorkItem",
            "CompleteWorkItem",
        ],
        forbidden + ["CreateWorkItem", "AgentGet"],
        turn_ids=turn_ids_by_work_item[work_item_b_id],
    )
    for wid in (work_item_a_id, work_item_b_id):
        require_scheduler_engine_activation_chain(
            harness,
            snapshot,
            work_item_id=wid,
            expected_admission_kinds=("scheduling",),
            lifecycle_message_ids={
                harness.prompt_scope("scheduler-multi-create")["message_id"]
            },
        )
    if harness.canonical_scheduler_enabled:
        demands = [
            row
            for row in snapshot["scheduler_work_demands"]
            if row["work_item_id"] in (work_item_a_id, work_item_b_id)
        ]
        require(
            len(demands) == 2
            and all(d["status"] == "terminal" for d in demands),
            f"multi-WorkItem demands did not converge: {demands}",
        )
    harness.restart()
    restarted_items = harness.work_items("scheduler-multi-after-restart")
    for wid, brief_id in (
        (work_item_a_id, item_a.get("result_brief_id")),
        (work_item_b_id, item_b.get("result_brief_id")),
    ):
        restarted = next(item for item in restarted_items if item["id"] == wid)
        require(
            restarted["state"] == "completed"
            and restarted.get("result_brief_id") == brief_id,
            f"multi-WorkItem did not survive restart: {restarted}",
        )


def run_scheduler_external_wait_resume_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_marker = f"SCHEDULER-EXTERNAL-WAIT-{marker}"
    completion_marker = f"SCHEDULER-EXTERNAL-COMPLETE-{marker}"
    objective = (
        f"{objective_marker}. Complete this WorkItem only after an external "
        "trigger wakes it. On the resume turn, call GetWorkItem, update both "
        "existing todos to completed, then emit a concise completion result "
        f"containing {completion_marker} immediately followed by "
        "CompleteWorkItem for the exact current item. Do not wait for more "
        "operator input."
    )
    callback = harness.reset_callback("scheduler-external-callback")
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-external-wait",
        phase["prompt"].format(
            case_id=case["id"],
            objective=json.dumps(objective, ensure_ascii=False),
            marker=marker,
            completion_marker=completion_marker,
        ),
    )
    waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_marker,
        expected_scheduling_state="waiting_external",
        label="scheduler-external-waiting",
    )
    harness.wait_agent_asleep()
    harness.fire_callback(
        "scheduler-external-wake",
        callback["trigger_url"],
        {"case_id": case["id"], "marker": marker},
    )
    item = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="scheduler-external-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools(
        "scheduler-external-wait",
        baseline,
        ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        forbidden + ["GetWorkItem", "UpdateWorkItem", "CompleteWorkItem"],
        message_id=harness.prompt_scope("scheduler-external-wait")["message_id"],
    )
    work_item_id = item["id"]
    require(item["state"] == "completed", f"WorkItem not completed: {item}")
    require(item["id"] == waiting["id"], "external wait-resume changed WorkItem identity")
    result_brief_id = item.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"external wait WorkItem omitted result brief: {item}",
    )
    result_brief = harness.brief(result_brief_id, "scheduler-external-result")
    require(
        result_brief.get("work_item_id") == work_item_id
        and completion_marker in (result_brief.get("text") or ""),
        f"external wait completion brief mismatch: {result_brief}",
    )
    snapshot = harness.runtime_db_snapshot("scheduler-external")
    resume_turn_ids = {
        row["turn_id"]
        for row in snapshot["turn_records"]
        if row["current_work_item_id"] == work_item_id
    }
    harness.assert_tools(
        "scheduler-external-resume",
        baseline,
        [name for name in required if name not in {"CreateWorkItem", "PickWorkItem", "WaitFor"}],
        forbidden + ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        turn_ids=resume_turn_ids,
    )
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_id,
        expected_admission_kinds=("wait_resume",),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-external-wait")["message_id"]
        },
    )
    waits = require_scheduler_wait_terminal(
        harness,
        snapshot,
        work_item_id=work_item_id,
        wait_kind="external",
        require_callback_trigger=True,
        callback_external_trigger_id=callback["external_trigger_id"],
    )
    if harness.canonical_scheduler_enabled:
        require_lifecycle_wait_adoption(
            snapshot,
            agent_id=harness.agent_id,
            work_item_id=work_item_id,
            wait=waits[0],
        )
    else:
        resume_messages = [
            row
            for row in snapshot["messages"]
            if row["work_item_id"] == work_item_id and row["kind"] == "system_tick"
        ]
        require(
            len(resume_messages) == 1,
            "legacy external wait did not produce exactly one targeted resume "
            f"message: waits={waits}, resumes={resume_messages}",
        )
    require_scheduler_engine_wait_resolution(
        harness,
        snapshot,
        work_item_id=work_item_id,
        wait_ids={wait["wait_condition_id"] for wait in waits},
    )


def run_scheduler_operator_wait_resume_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_marker = f"SCHEDULER-OPERATOR-WAIT-{marker}"
    completion_marker = f"SCHEDULER-OPERATOR-COMPLETE-{marker}"
    objective = (
        f"{objective_marker}. Complete this WorkItem only after the operator "
        "sends a message. On the resume turn, call GetWorkItem, update both "
        "existing todos to completed, then emit a concise completion result "
        f"containing {completion_marker} immediately followed by "
        "CompleteWorkItem for the exact current item. Do not wait for more "
        "operator input."
    )
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-operator-wait",
        phase["prompt"].format(
            case_id=case["id"],
            objective=json.dumps(objective, ensure_ascii=False),
            completion_marker=completion_marker,
        ),
    )
    waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_marker,
        expected_scheduling_state="waiting_operator",
        label="scheduler-operator-waiting",
    )
    harness.wait_agent_asleep()
    harness.prompt(
        "scheduler-operator-resume",
        f"The operator is resuming WorkItem {marker}. Proceed with the "
        "completion steps described in the objective.",
        work_item_id=waiting["id"],
    )
    item = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="scheduler-operator-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools(
        "scheduler-operator-wait",
        baseline,
        ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        forbidden + ["GetWorkItem", "UpdateWorkItem", "CompleteWorkItem"],
        message_id=harness.prompt_scope("scheduler-operator-wait")["message_id"],
    )
    work_item_id = item["id"]
    require(item["state"] == "completed", f"WorkItem not completed: {item}")
    require(item["id"] == waiting["id"], "operator wait-resume changed WorkItem identity")
    result_brief_id = item.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"operator wait WorkItem omitted result brief: {item}",
    )
    result_brief = harness.brief(result_brief_id, "scheduler-operator-result")
    require(
        result_brief.get("work_item_id") == work_item_id
        and completion_marker in (result_brief.get("text") or ""),
        f"operator wait completion brief mismatch: {result_brief}",
    )
    snapshot = harness.runtime_db_snapshot("scheduler-operator")
    resume_turn_ids = {
        row["turn_id"]
        for row in snapshot["turn_records"]
        if row["current_work_item_id"] == work_item_id
    }
    harness.assert_tools(
        "scheduler-operator-resume",
        baseline,
        [name for name in required if name not in {"CreateWorkItem", "PickWorkItem", "WaitFor"}],
        forbidden + ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        turn_ids=resume_turn_ids,
    )
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_id,
        expected_admission_kinds=("wait_resume",),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-operator-wait")["message_id"]
        },
    )
    waits = require_scheduler_wait_terminal(
        harness,
        snapshot,
        work_item_id=work_item_id,
        wait_kind="operator",
    )
    if harness.canonical_scheduler_enabled:
        require_lifecycle_wait_adoption(
            snapshot,
            agent_id=harness.agent_id,
            work_item_id=work_item_id,
            wait=waits[0],
        )
    require_scheduler_engine_wait_resolution(
        harness,
        snapshot,
        work_item_id=work_item_id,
        wait_ids={wait["wait_condition_id"] for wait in waits},
    )


def run_scheduler_concurrent_claim_fencing_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    """SCHED-E2E-010: interject during external wait creates new WorkItem."""
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_a_marker = f"SCHEDULER-CONCURRENT-A-{marker}"
    completion_a = f"SCHEDULER-CONCURRENT-COMPLETE-A-{marker}"
    objective_b_marker = f"SCHEDULER-CONCURRENT-B-{marker}"
    completion_b = f"SCHEDULER-CONCURRENT-COMPLETE-B-{marker}"
    objective_a = (
        f"{objective_a_marker}. Complete this WorkItem only after an external "
        "trigger wakes it. On the resume turn, call GetWorkItem, update both "
        "existing todos to completed, then emit a concise completion result "
        f"containing {completion_a} immediately followed by "
        "CompleteWorkItem for the exact current item. Do not wait for more "
        "operator input."
    )
    objective_b = (
        f"{objective_b_marker}. Complete this WorkItem only after the Runtime "
        "resumes it through an autonomous work_queue SystemTick. On that "
        "autonomous turn, inspect the exact current item with ListWorkItems "
        "using filter current and optionally GetWorkItem, update both existing "
        f"todos to completed, then emit a concise completion result containing "
        f"{completion_b} immediately followed by CompleteWorkItem for that "
        "exact item. Do not wait for more operator input."
    )
    callback = harness.reset_callback("scheduler-concurrent-callback")
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-concurrent-create",
        phase["prompt"].format(
            case_id=case["id"],
            objective_a=json.dumps(objective_a, ensure_ascii=False),
            marker=marker,
            completion_a=completion_a,
        ),
    )
    waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_a_marker,
        expected_scheduling_state="waiting_external",
        label="scheduler-concurrent-waiting",
    )
    harness.wait_agent_asleep()
    created_b = harness.request(
        "POST",
        harness.agent_path("work-items", control=True),
        {"objective": objective_b},
    )
    write_json(harness.evidence / "scheduler-concurrent-created-b.json", created_b)
    item_b = harness.wait_work_item(
        objective_marker=objective_b_marker,
        expected_state="completed",
        label="scheduler-concurrent-b-completed",
    )
    harness.fire_callback(
        "scheduler-concurrent-wake",
        callback["trigger_url"],
        {"case_id": case["id"], "marker": marker},
    )
    item_a = harness.wait_work_item(
        objective_marker=objective_a_marker,
        expected_state="completed",
        label="scheduler-concurrent-a-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools(
        "scheduler-concurrent-create",
        baseline,
        ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        forbidden + ["GetWorkItem", "UpdateWorkItem", "CompleteWorkItem"],
        message_id=harness.prompt_scope("scheduler-concurrent-create")[
            "message_id"
        ],
    )
    work_item_a_id = item_a["id"]
    work_item_b_id = item_b["id"]
    require(
        created_b.get("id") == work_item_b_id,
        f"concurrent control-plane WorkItem identity mismatch: {created_b}",
    )
    require(item_a["state"] == "completed", f"WorkItem A not completed: {item_a}")
    require(item_b["state"] == "completed", f"WorkItem B not completed: {item_b}")
    require(work_item_a_id != work_item_b_id, "WorkItem A and B share the same id")
    for item, letter, completion, brief_label in (
        (item_a, "A", completion_a, "scheduler-concurrent-result-a"),
        (item_b, "B", completion_b, "scheduler-concurrent-result-b"),
    ):
        brief_id = item.get("result_brief_id")
        require(
            isinstance(brief_id, str) and brief_id,
            f"WorkItem {letter} omitted result brief: {item}",
        )
        brief = harness.brief(brief_id, brief_label)
        require(
            brief.get("work_item_id") == item["id"]
            and completion in (brief.get("text") or ""),
            f"WorkItem {letter} brief mismatch: {brief}",
        )
    snapshot = harness.runtime_db_snapshot("scheduler-concurrent")
    messages_by_id = {
        row["message_id"]: row for row in snapshot["messages"]
    }
    a_turn_ids = {
        row["turn_id"]
        for row in snapshot["turn_records"]
        if row["current_work_item_id"] == work_item_a_id
        and messages_by_id.get(row["trigger_message_id"], {}).get("work_item_id")
        == work_item_a_id
    }
    b_turn_ids = {
        row["turn_id"]
        for row in snapshot["turn_records"]
        if row["current_work_item_id"] == work_item_b_id
        and messages_by_id.get(row["trigger_message_id"], {}).get("work_item_id")
        == work_item_b_id
    }
    harness.assert_tools(
        "scheduler-concurrent-a-resume",
        baseline,
        ["GetWorkItem", "UpdateWorkItem", "CompleteWorkItem"],
        forbidden + ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        turn_ids=a_turn_ids,
    )
    harness.assert_tools(
        "scheduler-concurrent-b-autonomous",
        baseline,
        ["ListWorkItems", "UpdateWorkItem", "CompleteWorkItem"],
        forbidden + ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        turn_ids=b_turn_ids,
    )
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_a_id,
        expected_admission_kinds=("wait_resume",),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-concurrent-create")["message_id"]
        },
    )
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_b_id,
        expected_admission_kinds=("scheduling",),
        lifecycle_message_ids=set(),
    )
    waits = require_scheduler_wait_terminal(
        harness,
        snapshot,
        work_item_id=work_item_a_id,
        wait_kind="external",
        require_callback_trigger=True,
        callback_external_trigger_id=callback["external_trigger_id"],
    )
    require_scheduler_engine_wait_resolution(
        harness,
        snapshot,
        work_item_id=work_item_a_id,
        wait_ids={wait["wait_condition_id"] for wait in waits},
    )
    if harness.canonical_scheduler_enabled:
        demands = [
            row
            for row in snapshot["scheduler_work_demands"]
            if row["work_item_id"] in (work_item_a_id, work_item_b_id)
        ]
        require(
            len(demands) == 2
            and all(d["status"] == "terminal" for d in demands),
            f"concurrent demands did not converge: {demands}",
        )
    harness.restart()
    restarted_items = harness.work_items("scheduler-concurrent-after-restart")
    for wid, brief_id in (
        (work_item_a_id, item_a.get("result_brief_id")),
        (work_item_b_id, item_b.get("result_brief_id")),
    ):
        restarted = next(item for item in restarted_items if item["id"] == wid)
        require(
            restarted["state"] == "completed"
            and restarted.get("result_brief_id") == brief_id,
            f"concurrent WorkItem did not survive restart: {restarted}",
        )


def run_scheduler_operator_interject_during_wait_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    """SCHED-E2E-011: operator interject during operator wait creates new WorkItem."""
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_a_marker = f"SCHEDULER-INTERJECT-A-{marker}"
    completion_a = f"SCHEDULER-INTERJECT-COMPLETE-A-{marker}"
    objective_b_marker = f"SCHEDULER-INTERJECT-B-{marker}"
    completion_b = f"SCHEDULER-INTERJECT-COMPLETE-B-{marker}"
    objective_a = (
        f"{objective_a_marker}. Complete this WorkItem only after the operator "
        "sends a message. On the resume turn, call GetWorkItem, update both "
        "existing todos to completed, then emit a concise completion result "
        f"containing {completion_a} immediately followed by "
        "CompleteWorkItem for the exact current item. Do not wait for more "
        "operator input."
    )
    objective_b = (
        f"{objective_b_marker}. Complete this WorkItem only after the Runtime "
        "resumes it through an autonomous work_queue SystemTick. On that "
        "autonomous turn, inspect the exact current item with ListWorkItems "
        "using filter current and optionally GetWorkItem, update both existing "
        f"todos to completed, then emit a concise completion result containing "
        f"{completion_b} immediately followed by CompleteWorkItem for that "
        "exact item. Do not wait for more operator input."
    )
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-interject-create",
        phase["prompt"].format(
            case_id=case["id"],
            objective_a=json.dumps(objective_a, ensure_ascii=False),
            completion_a=completion_a,
        ),
    )
    waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_a_marker,
        expected_scheduling_state="waiting_operator",
        label="scheduler-interject-waiting",
    )
    harness.wait_agent_asleep()
    interject_text = (
        f"Scheduler Docker E2E case {case['id']} interject. Create exactly one "
        f"WorkItem whose objective is "
        f"{json.dumps(objective_b, ensure_ascii=False)}, "
        f"with plan_status ready and exactly these todos: interject-b-seed "
        f"completed, interject-b-complete pending. The expected completion "
        f"marker is {completion_b}. Do not PickWorkItem, WaitFor, or "
        "CompleteWorkItem in this turn. After CreateWorkItem succeeds, end "
        "with a concise acknowledgement."
    )
    harness.prompt("scheduler-interject-b-create", interject_text)
    item_b = harness.wait_work_item(
        objective_marker=objective_b_marker,
        expected_state="completed",
        label="scheduler-interject-b-completed",
    )
    harness.prompt(
        "scheduler-interject-resume",
        f"The operator is resuming WorkItem {marker}. Proceed with the "
        "completion steps described in the objective.",
        work_item_id=waiting["id"],
    )
    item_a = harness.wait_work_item(
        objective_marker=objective_a_marker,
        expected_state="completed",
        label="scheduler-interject-a-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools(
        "scheduler-interject-create",
        baseline,
        ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        forbidden + ["GetWorkItem", "UpdateWorkItem", "CompleteWorkItem"],
        message_id=harness.prompt_scope("scheduler-interject-create")[
            "message_id"
        ],
    )
    harness.assert_tools(
        "scheduler-interject-b-create",
        baseline,
        ["CreateWorkItem"],
        forbidden + ["PickWorkItem", "WaitFor", "CompleteWorkItem"],
        message_id=harness.prompt_scope("scheduler-interject-b-create")[
            "message_id"
        ],
    )
    work_item_a_id = item_a["id"]
    work_item_b_id = item_b["id"]
    require(item_a["state"] == "completed", f"WorkItem A not completed: {item_a}")
    require(item_b["state"] == "completed", f"WorkItem B not completed: {item_b}")
    require(work_item_a_id != work_item_b_id, "WorkItem A and B share the same id")
    for item, letter, completion, brief_label in (
        (item_a, "A", completion_a, "scheduler-interject-result-a"),
        (item_b, "B", completion_b, "scheduler-interject-result-b"),
    ):
        brief_id = item.get("result_brief_id")
        require(
            isinstance(brief_id, str) and brief_id,
            f"WorkItem {letter} omitted result brief: {item}",
        )
        brief = harness.brief(brief_id, brief_label)
        require(
            brief.get("work_item_id") == item["id"]
            and completion in (brief.get("text") or ""),
            f"WorkItem {letter} brief mismatch: {brief}",
        )
    snapshot = harness.runtime_db_snapshot("scheduler-interject")
    a_turn_ids = {
        row["turn_id"]
        for row in snapshot["turn_records"]
        if row["current_work_item_id"] == work_item_a_id
    }
    b_turn_ids = {
        row["turn_id"]
        for row in snapshot["turn_records"]
        if row["current_work_item_id"] == work_item_b_id
    }
    harness.assert_tools(
        "scheduler-interject-a-resume",
        baseline,
        ["GetWorkItem", "UpdateWorkItem", "CompleteWorkItem"],
        forbidden + ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        turn_ids=a_turn_ids,
    )
    harness.assert_tools(
        "scheduler-interject-b-autonomous",
        baseline,
        ["ListWorkItems", "UpdateWorkItem", "CompleteWorkItem"],
        forbidden + ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        turn_ids=b_turn_ids,
    )
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_a_id,
        expected_admission_kinds=("wait_resume",),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-interject-create")["message_id"]
        },
    )
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_b_id,
        expected_admission_kinds=("scheduling",),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-interject-b-create")["message_id"]
        },
    )
    waits = require_scheduler_wait_terminal(
        harness,
        snapshot,
        work_item_id=work_item_a_id,
        wait_kind="operator",
    )
    require_scheduler_engine_wait_resolution(
        harness,
        snapshot,
        work_item_id=work_item_a_id,
        wait_ids={wait["wait_condition_id"] for wait in waits},
    )
    if harness.canonical_scheduler_enabled:
        demands = [
            row
            for row in snapshot["scheduler_work_demands"]
            if row["work_item_id"] in (work_item_a_id, work_item_b_id)
        ]
        require(
            len(demands) == 2
            and all(d["status"] == "terminal" for d in demands),
            f"interject demands did not converge: {demands}",
        )
    harness.restart()
    restarted_items = harness.work_items("scheduler-interject-after-restart")
    for wid, brief_id in (
        (work_item_a_id, item_a.get("result_brief_id")),
        (work_item_b_id, item_b.get("result_brief_id")),
    ):
        restarted = next(item for item in restarted_items if item["id"] == wid)
        require(
            restarted["state"] == "completed"
            and restarted.get("result_brief_id") == brief_id,
            f"interject WorkItem did not survive restart: {restarted}",
        )


def run_scheduler_compaction_continuity_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    """SCHED-E2E-012: WorkItem survives compaction and restart."""
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_marker = f"SCHEDULER-COMPACTION-{marker}"
    completion_marker = f"SCHEDULER-COMPACTION-COMPLETE-{marker}"
    objective = (
        f"{objective_marker}. Complete this WorkItem only after the operator "
        "sends a message. On the resume turn, call GetWorkItem, update both "
        "existing todos to completed, then emit a concise completion result "
        f"containing {completion_marker} immediately followed by "
        "CompleteWorkItem for the exact current item. Do not wait for more "
        "operator input."
    )
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-compaction-create",
        phase["prompt"].format(
            case_id=case["id"],
            objective=json.dumps(objective, ensure_ascii=False),
            completion_marker=completion_marker,
        ),
    )
    waiting = harness.wait_work_item_scheduling_state(
        objective_marker=objective_marker,
        expected_scheduling_state="waiting_operator",
        label="scheduler-compaction-waiting",
    )
    stimulus_snapshot = harness.runtime_db_snapshot(
        "scheduler-compaction-stimulus"
    )
    require_turn_local_compaction(
        stimulus_snapshot,
        label="scheduler-compaction",
    )
    harness.wait_agent_asleep()
    harness.prompt(
        "scheduler-compaction-resume",
        f"The operator is resuming WorkItem {marker}. Proceed with the "
        "completion steps described in the objective.",
        work_item_id=waiting["id"],
    )
    item = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="scheduler-compaction-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools(
        "scheduler-compaction-create",
        baseline,
        ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        forbidden + ["GetWorkItem", "UpdateWorkItem", "CompleteWorkItem"],
        message_id=harness.prompt_scope("scheduler-compaction-create")[
            "message_id"
        ],
    )
    work_item_id = item["id"]
    require(item["state"] == "completed", f"WorkItem not completed: {item}")
    require(item["id"] == waiting["id"], "compaction wait-resume changed WorkItem identity")
    result_brief_id = item.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"compaction WorkItem omitted result brief: {item}",
    )
    result_brief = harness.brief(result_brief_id, "scheduler-compaction-result")
    require(
        result_brief.get("work_item_id") == work_item_id
        and completion_marker in (result_brief.get("text") or ""),
        f"compaction completion brief mismatch: {result_brief}",
    )
    snapshot = harness.runtime_db_snapshot("scheduler-compaction")
    resume_turn_ids = {
        row["turn_id"]
        for row in snapshot["turn_records"]
        if row["current_work_item_id"] == work_item_id
    }
    stimulus_turn_ids = {
        row["turn_id"]
        for row in snapshot["turn_records"]
        if row["turn_id"] not in resume_turn_ids
    }
    harness.assert_tools(
        "scheduler-compaction-stimulus",
        baseline,
        ["ExecCommand"],
        forbidden,
        turn_ids=stimulus_turn_ids,
    )
    harness.assert_tools(
        "scheduler-compaction-resume",
        baseline,
        [
            name
            for name in required
            if name in {"GetWorkItem", "UpdateWorkItem", "CompleteWorkItem"}
        ],
        forbidden + ["CreateWorkItem", "PickWorkItem", "WaitFor"],
        turn_ids=resume_turn_ids,
    )
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_id,
        expected_admission_kinds=("wait_resume",),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-compaction-create")["message_id"]
        },
    )
    require_turn_local_compaction(
        snapshot,
        label="scheduler-compaction",
    )
    waits = require_scheduler_wait_terminal(
        harness,
        snapshot,
        work_item_id=work_item_id,
        wait_kind="operator",
    )
    require_scheduler_engine_wait_resolution(
        harness,
        snapshot,
        work_item_id=work_item_id,
        wait_ids={wait["wait_condition_id"] for wait in waits},
    )
    harness.restart()
    restarted_items = harness.work_items("scheduler-compaction-after-restart")
    restarted = next(item for item in restarted_items if item["id"] == work_item_id)
    require(
        restarted["state"] == "completed"
        and restarted.get("result_brief_id") == result_brief_id,
        f"compaction WorkItem did not survive restart: {restarted}",
    )


def run_scheduler_worktree_isolation_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    """SCHED-E2E-013: agent creates and removes a worktree through model tools."""
    harness.initialize_workspace()
    harness.start()
    attached = harness.request(
        "POST",
        harness.agent_path("workspace/attach", control=True),
        {"path": "/acceptance/repo"},
    )
    write_json(harness.evidence / "scheduler-worktree-attach.json", attached)
    workspace_id = attached["workspace_id"]
    branch = f"e2e-worktree-{secrets.token_hex(4)}"
    marker = secrets.token_hex(4)
    objective_marker = f"SCHEDULER-WORKTREE-{marker}"
    completion_marker = f"SCHEDULER-WORKTREE-COMPLETE-{marker}"
    objective = (
        f"{objective_marker}. Create a linked worktree from base_ref main with "
        f"branch {branch}, activate it, verify it is active with "
        "GetWorkspaceState, switch back to the canonical workspace, remove the "
        "worktree, and verify removal. Then complete this WorkItem."
    )
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-worktree-lifecycle",
        phase["prompt"].format(
            case_id=case["id"],
            objective=json.dumps(objective, ensure_ascii=False),
            branch=branch,
            workspace_id=workspace_id,
            completion_marker=completion_marker,
        ),
    )
    item = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="scheduler-worktree-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools(
        "scheduler-worktree-lifecycle",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("scheduler-worktree-lifecycle")[
            "message_id"
        ],
    )
    work_item_id = item["id"]
    require(item["state"] == "completed", f"WorkItem not completed: {item}")
    result_brief_id = item.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"worktree WorkItem omitted result brief: {item}",
    )
    result_brief = harness.brief(result_brief_id, "scheduler-worktree-result")
    require(
        result_brief.get("work_item_id") == work_item_id
        and completion_marker in (result_brief.get("text") or ""),
        f"worktree completion brief mismatch: {result_brief}",
    )
    git_state = harness.docker(
        "exec",
        harness.container,
        "bash",
        "-lc",
        "set -euo pipefail; "
        "git -C /acceptance/repo status --porcelain; "
        "printf '%s\n' '--- worktrees ---'; "
        "git -C /acceptance/repo worktree list --porcelain",
    ).stdout
    (harness.evidence / "scheduler-worktree-git.txt").write_text(git_state)
    status, worktrees = git_state.split("--- worktrees ---\n", 1)
    require(not status.strip(), f"canonical repository is dirty:\n{status}")
    require(
        worktrees.count("worktree ") == 1,
        f"managed worktree was not removed cleanly:\n{worktrees}",
    )
    snapshot = harness.runtime_db_snapshot("scheduler-worktree")
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_id,
        expected_admission_kinds=(),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-worktree-lifecycle")["message_id"]
        },
    )
    harness.restart()
    restarted_items = harness.work_items("scheduler-worktree-after-restart")
    restarted = next(item for item in restarted_items if item["id"] == work_item_id)
    require(
        restarted["state"] == "completed"
        and restarted.get("result_brief_id") == result_brief_id,
        f"worktree WorkItem did not survive restart: {restarted}",
    )


def run_scheduler_spawn_agent_supervision_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    """SCHED-E2E-014: agent spawns a private_child and completes parent WorkItem."""
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_marker = f"SCHEDULER-SPAWN-{marker}"
    completion_marker = f"SCHEDULER-SPAWN-COMPLETE-{marker}"
    child_marker = f"SCHEDULER-SPAWN-CHILD-{marker}"
    objective = (
        f"{objective_marker}. Spawn a private_child agent, inspect its task "
        "status, and complete this WorkItem."
    )
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-spawn-and-complete",
        phase["prompt"].format(
            case_id=case["id"],
            objective=json.dumps(objective, ensure_ascii=False),
            child_marker=child_marker,
            completion_marker=completion_marker,
        ),
    )
    item = harness.wait_work_item(
        objective_marker=objective_marker,
        expected_state="completed",
        label="scheduler-spawn-completed",
    )
    required, forbidden = phase_tools(phase)
    create_events = harness.assert_tools(
        "scheduler-spawn-and-complete",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("scheduler-spawn-and-complete")[
            "message_id"
        ],
    )
    work_item_id = item["id"]
    require(item["state"] == "completed", f"WorkItem not completed: {item}")
    result_brief_id = item.get("result_brief_id")
    require(
        isinstance(result_brief_id, str) and result_brief_id,
        f"spawn WorkItem omitted result brief: {item}",
    )
    result_brief = harness.brief(result_brief_id, "scheduler-spawn-result")
    require(
        result_brief.get("work_item_id") == work_item_id
        and completion_marker in (result_brief.get("text") or ""),
        f"spawn completion brief mismatch: {result_brief}",
    )
    spawn_event = next(
        event
        for event in create_events
        if event["payload"].get("tool_name") == "SpawnAgent"
    )
    spawn_detail = harness.tool_detail(spawn_event, "scheduler-spawn")
    spawn_result = result_value(spawn_detail)
    require(
        isinstance(spawn_result.get("agent_id"), str)
        and spawn_result["agent_id"],
        f"SpawnAgent result missing agent_id: {spawn_result}",
    )
    task_handle = spawn_result.get("task_handle") or {}
    task_id = task_handle.get("task_id")
    require(
        isinstance(task_id, str) and task_id,
        f"SpawnAgent result missing task_id: {spawn_result}",
    )
    snapshot = harness.runtime_db_snapshot("scheduler-spawn")
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_id,
        expected_admission_kinds=(),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-spawn-and-complete")["message_id"]
        },
    )
    harness.restart()
    restarted_items = harness.work_items("scheduler-spawn-after-restart")
    restarted = next(item for item in restarted_items if item["id"] == work_item_id)
    require(
        restarted["state"] == "completed"
        and restarted.get("result_brief_id") == result_brief_id,
        f"spawn WorkItem did not survive restart: {restarted}",
    )


def run_scheduler_checkpoint_replay_case(
    harness: CaseHarness, case: dict[str, Any]
) -> None:
    """SCHED-E2E-015: multiple WorkItems survive restart and converge."""
    harness.initialize_workspace()
    harness.start()
    marker = secrets.token_hex(4)
    objective_a_marker = f"SCHEDULER-REPLAY-A-{marker}"
    completion_a = f"SCHEDULER-REPLAY-COMPLETE-A-{marker}"
    objective_b_marker = f"SCHEDULER-REPLAY-B-{marker}"
    completion_b = f"SCHEDULER-REPLAY-COMPLETE-B-{marker}"
    callback = harness.reset_callback("scheduler-replay-callback")
    objective_a = (
        f"{objective_a_marker}. Complete this WorkItem only after an external "
        "trigger wakes it. On the resume turn, call GetWorkItem, update both "
        "existing todos to completed, then emit a concise completion result "
        f"containing {completion_a} immediately followed by "
        "CompleteWorkItem for the exact current item. Do not wait for more "
        "operator input."
    )
    objective_b = (
        f"{objective_b_marker}. Complete this WorkItem only after the Runtime "
        "resumes it through an autonomous work_queue SystemTick. On that "
        "autonomous turn, inspect the exact current item with ListWorkItems "
        "using filter current and optionally GetWorkItem, update both existing "
        f"todos to completed, then emit a concise completion result containing "
        f"{completion_b} immediately followed by CompleteWorkItem for that "
        "exact item. Do not wait for more operator input."
    )
    phase = case["phases"][0]
    baseline, _ = harness.prompt(
        "scheduler-replay-create",
        phase["prompt"].format(
            case_id=case["id"],
            objective_a=json.dumps(objective_a, ensure_ascii=False),
            objective_b=json.dumps(objective_b, ensure_ascii=False),
            completion_a=completion_a,
            completion_b=completion_b,
        ),
    )
    waiting_a = harness.wait_work_item_scheduling_state(
        objective_marker=objective_a_marker,
        expected_scheduling_state="waiting_external",
        label="scheduler-replay-waiting-a",
    )
    item_b = harness.wait_work_item(
        objective_marker=objective_b_marker,
        expected_state="completed",
        label="scheduler-replay-b-completed",
    )
    before_restart_snapshot = harness.runtime_db_snapshot(
        "scheduler-replay-before-restart"
    )
    harness.restart()
    restarted_items = harness.work_items("scheduler-replay-after-restart")
    restarted_a = next(
        item for item in restarted_items if item["id"] == waiting_a["id"]
    )
    restarted_b = next(
        item for item in restarted_items if item["id"] == item_b["id"]
    )
    require(
        restarted_a.get("scheduling_state") == "waiting_external",
        f"WorkItem A did not survive restart in waiting state: {restarted_a}",
    )
    require(
        restarted_b["state"] == "completed"
        and restarted_b.get("result_brief_id") == item_b.get("result_brief_id"),
        f"WorkItem B did not survive restart: {restarted_b}",
    )
    harness.wait_agent_asleep()
    harness.fire_callback(
        "scheduler-replay-wake",
        callback["trigger_url"],
        {"case_id": case["id"], "marker": marker},
    )
    item_a = harness.wait_work_item(
        objective_marker=objective_a_marker,
        expected_state="completed",
        label="scheduler-replay-a-completed",
    )
    required, forbidden = phase_tools(phase)
    harness.assert_tools(
        "scheduler-replay-create",
        baseline,
        required,
        forbidden,
        message_id=harness.prompt_scope("scheduler-replay-create")["message_id"],
    )
    work_item_a_id = item_a["id"]
    work_item_b_id = item_b["id"]
    require(item_a["state"] == "completed", f"WorkItem A not completed: {item_a}")
    require(item_b["state"] == "completed", f"WorkItem B not completed: {item_b}")
    require(work_item_a_id != work_item_b_id, "WorkItem A and B share the same id")
    for item, letter, completion, brief_label in (
        (item_a, "A", completion_a, "scheduler-replay-result-a"),
        (item_b, "B", completion_b, "scheduler-replay-result-b"),
    ):
        brief_id = item.get("result_brief_id")
        require(
            isinstance(brief_id, str) and brief_id,
            f"WorkItem {letter} omitted result brief: {item}",
        )
        brief = harness.brief(brief_id, brief_label)
        require(
            brief.get("work_item_id") == item["id"]
            and completion in (brief.get("text") or ""),
            f"WorkItem {letter} brief mismatch: {brief}",
        )
    snapshot = harness.runtime_db_snapshot("scheduler-replay")
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_a_id,
        expected_admission_kinds=("scheduling", "wait_resume"),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-replay-create")["message_id"]
        },
    )
    require_scheduler_engine_activation_chain(
        harness,
        snapshot,
        work_item_id=work_item_b_id,
        expected_admission_kinds=("scheduling",),
        lifecycle_message_ids={
            harness.prompt_scope("scheduler-replay-create")["message_id"]
        },
    )
    waits = require_scheduler_wait_terminal(
        harness,
        snapshot,
        work_item_id=work_item_a_id,
        wait_kind="external",
        require_callback_trigger=True,
        callback_external_trigger_id=callback["external_trigger_id"],
    )
    require_scheduler_engine_wait_resolution(
        harness,
        snapshot,
        work_item_id=work_item_a_id,
        wait_ids={wait["wait_condition_id"] for wait in waits},
    )
    if harness.canonical_scheduler_enabled:
        require_checkpoint_restart_activation_lineage(
            before_restart_snapshot,
            snapshot,
            work_item_id=work_item_a_id,
            wait_id=waits[0]["wait_condition_id"],
        )
        demands = [
            row
            for row in snapshot["scheduler_work_demands"]
            if row["work_item_id"] in (work_item_a_id, work_item_b_id)
        ]
        require(
            len(demands) == 2
            and all(d["status"] == "terminal" for d in demands),
            f"replay demands did not converge: {demands}",
        )



CASE_RUNNERS = {
    "runtime-auth-model-delivery": run_runtime_case,
    "memory-agent-home-persistence": run_memory_case,
    "workspace-restart-lifecycle": run_workspace_case,
    "workitem-wait-restart-complete": run_workitem_case,
    "scheduler-task-wait-resume": run_scheduler_task_wait_resume_case,
    "scheduler-provider-failure-work-queue-retry": (
        run_scheduler_provider_failure_retry_case
    ),
    "scheduler-multi-workitem-scheduling": run_scheduler_multi_workitem_case,
    "scheduler-external-wait-resume": run_scheduler_external_wait_resume_case,
    "scheduler-operator-wait-resume": run_scheduler_operator_wait_resume_case,
    "scheduler-concurrent-claim-fencing": run_scheduler_concurrent_claim_fencing_case,
    "scheduler-operator-interject-during-wait": run_scheduler_operator_interject_during_wait_case,
    "scheduler-compaction-continuity": run_scheduler_compaction_continuity_case,
    "scheduler-worktree-isolation": run_scheduler_worktree_isolation_case,
    "scheduler-spawn-agent-supervision": run_scheduler_spawn_agent_supervision_case,
    "scheduler-checkpoint-replay": run_scheduler_checkpoint_replay_case,
}


def validate_manifest(manifest: dict[str, Any]) -> None:
    require(manifest.get("version") == 2, "manifest version must be 2")
    fixture_corpus_revision = manifest.get("scheduler_fixture_corpus_revision")
    require(
        isinstance(fixture_corpus_revision, str) and fixture_corpus_revision,
        "scheduler_fixture_corpus_revision must be non-empty",
    )
    cases = manifest.get("cases")
    require(isinstance(cases, list) and cases, "manifest cases must be non-empty")
    profiles = manifest.get("profiles", {})
    require(isinstance(profiles, dict), "manifest profiles must be an object")
    for profile_id, profile in profiles.items():
        require(
            isinstance(profile_id, str) and profile_id,
            "profile id must be non-empty",
        )
        require(isinstance(profile, dict), f"profile {profile_id} must be an object")
        require(
            profile.get("provider_mode", "live") in {"live", "stub"},
            f"profile {profile_id} has invalid provider_mode",
        )
        require(
            profile.get("gate_kind", "required") in {"required", "live_canary"},
            f"profile {profile_id} has invalid gate_kind",
        )
        require(
            profile.get("tool_assertion_mode", "strict") in {"strict", "observe"},
            f"profile {profile_id} has invalid tool_assertion_mode",
        )
        if profile.get("gate_kind") == "live_canary":
            require(
                profile.get("provider_mode", "live") == "live",
                f"profile {profile_id} live canary must use live provider mode",
            )
            require(
                profile.get("tool_assertion_mode") == "observe",
                f"profile {profile_id} live canary must observe tool assertions",
            )
        required = profile.get("required_coverage_ids", [])
        require(
            isinstance(required, list)
            and len(required) == len(set(required))
            and all(isinstance(value, str) and value for value in required),
            f"profile {profile_id} required_coverage_ids must be unique strings",
        )
        case_ids = profile.get("case_ids", [])
        require(
            isinstance(case_ids, list)
            and len(case_ids) == len(set(case_ids))
            and all(isinstance(value, str) and value for value in case_ids),
            f"profile {profile_id} case_ids must be unique strings",
        )
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
        coverage_ids = case.get("coverage_ids", [])
        require(
            isinstance(coverage_ids, list)
            and len(coverage_ids) == len(set(coverage_ids))
            and all(isinstance(value, str) and value for value in coverage_ids),
            f"{case_id} coverage_ids must be unique strings",
        )
        provider_mode = case.get("provider_mode", "live")
        require(
            provider_mode in {"live", "stub"},
            f"{case_id} has invalid provider_mode",
        )
        if provider_mode == "stub":
            require(
                isinstance(case.get("stub_scenario"), str)
                and case["stub_scenario"],
                f"{case_id} stub provider mode requires stub_scenario",
            )
        runtime_env = case.get("runtime_env", {})
        require(isinstance(runtime_env, dict), f"{case_id} runtime_env must be an object")
        require(
            "HOLON_SCHEDULER" not in runtime_env,
            f"{case_id} must leave HOLON_SCHEDULER to the process matrix",
        )
        require(
            all(
                isinstance(name, str)
                and name.startswith("HOLON_")
                and isinstance(value, str)
                for name, value in runtime_env.items()
            ),
            f"{case_id} runtime_env must contain HOLON_ string entries",
        )
        model_runtime_override = case.get("model_runtime_override", {})
        require(
            isinstance(model_runtime_override, dict)
            and all(
                name
                in {
                    "prompt_budget_estimated_tokens",
                    "compaction_trigger_estimated_tokens",
                    "compaction_keep_recent_estimated_tokens",
                }
                and isinstance(value, int)
                and value > 0
                for name, value in model_runtime_override.items()
            ),
            f"{case_id} model_runtime_override must contain positive supported integers",
        )
        prompt_budget = model_runtime_override.get("prompt_budget_estimated_tokens")
        compaction_trigger = model_runtime_override.get(
            "compaction_trigger_estimated_tokens"
        )
        keep_recent = model_runtime_override.get(
            "compaction_keep_recent_estimated_tokens"
        )
        require(
            prompt_budget is None
            or compaction_trigger is None
            or compaction_trigger <= prompt_budget,
            f"{case_id} compaction trigger exceeds prompt budget",
        )
        require(
            compaction_trigger is None
            or keep_recent is None
            or keep_recent <= compaction_trigger,
            f"{case_id} compaction keep-recent exceeds trigger",
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
    known_case_ids = {case["id"] for case in cases}
    cases_by_id = {case["id"]: case for case in cases}
    known_coverage_ids = {
        coverage_id
        for case in cases
        for coverage_id in case.get("coverage_ids", [])
    }
    for profile_id, profile in profiles.items():
        profile_case_ids = profile.get("case_ids", [])
        unknown = sorted(set(profile_case_ids) - known_case_ids)
        require(not unknown, f"profile {profile_id} has unknown cases: {unknown}")
        unknown_coverage = sorted(
            set(profile.get("required_coverage_ids", [])) - known_coverage_ids
        )
        require(
            not unknown_coverage,
            f"profile {profile_id} has unknown coverage ids: {unknown_coverage}",
        )
        if profile.get("provider_mode", "live") == "stub":
            missing_stub_scenarios = sorted(
                case_id
                for case_id in profile_case_ids
                if not cases_by_id[case_id].get("stub_scenario")
            )
            require(
                not missing_stub_scenarios,
                f"profile {profile_id} cases require stub_scenario: "
                f"{missing_stub_scenarios}",
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


def resolve_profile(
    manifest: dict[str, Any], profile_id: str | None
) -> dict[str, Any]:
    if profile_id is None:
        return {
            "gate_kind": "suite",
            "provider_mode": "live",
            "tool_assertion_mode": "strict",
            "required_coverage_ids": [],
        }
    profiles = manifest.get("profiles", {})
    require(profile_id in profiles, f"unknown profile: {profile_id}")
    return profiles[profile_id]


def expand_case_matrix(
    cases: list[dict[str, Any]], *, scheduler_matrix: bool
) -> list[tuple[dict[str, Any], str | None]]:
    return [
        (case, engine)
        for case in cases
        for engine in (
            SCHEDULER_ENGINES
            if scheduler_matrix and "scheduler" in case.get("tags", [])
            else (None,)
        )
    ]


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
    behavioral_variances: list[dict[str, Any]] = []
    provider_rounds = 0
    provider_attempts = 0
    seen_event_seqs: set[int] = set()
    token_usage = {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}
    for path in evidence.glob("*-events.json"):
        try:
            events = json.loads(path.read_text()).get("events", [])
        except (OSError, json.JSONDecodeError):
            continue
        for event in events:
            event_seq = event.get("event_seq")
            if isinstance(event_seq, int):
                if event_seq in seen_event_seqs:
                    continue
                seen_event_seqs.add(event_seq)
            event_type = event.get("type")
            if event_type == "tool_executed":
                name = event.get("payload", {}).get("tool_name", "unknown")
                tool_counts[name] = tool_counts.get(name, 0) + 1
            if event_type == "provider_round_completed":
                provider_rounds += 1
                payload = event.get("payload", {})
                provider_attempts += len(
                    (payload.get("provider_attempt_timeline") or {}).get("attempts") or []
                )
                usage = payload.get("token_usage") or {}
                for key in token_usage:
                    token_usage[key] += int(usage.get(key) or 0)
    for path in evidence.glob("*-tool-observation.json"):
        try:
            observation = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        missing = list(observation.get("missing") or [])
        forbidden_actual = list(observation.get("forbidden_actual") or [])
        if missing or forbidden_actual:
            behavioral_variances.append(
                {
                    "scope": path.name.removesuffix("-tool-observation.json"),
                    "missing_tools": missing,
                    "forbidden_tools_used": forbidden_actual,
                }
            )
    return {
        "tool_counts": tool_counts,
        "behavioral_variances": behavioral_variances,
        "provider_rounds": provider_rounds,
        "provider_attempts": provider_attempts,
        "provider_retries": max(0, provider_attempts - provider_rounds),
        "token_usage": token_usage,
    }


def collect_case_schema_revision(evidence: Path) -> int | None:
    revisions = set()
    for path in evidence.glob("*-runtime-db.json"):
        try:
            value = json.loads(path.read_text()).get("schema_revision")
        except (OSError, json.JSONDecodeError):
            continue
        if isinstance(value, int):
            revisions.add(value)
    require(
        len(revisions) <= 1,
        f"runtime schema revision changed during case {evidence.name}: {revisions}",
    )
    return next(iter(revisions), None)


def scheduler_acceptance_report(
    *,
    run_record: dict[str, Any],
    case_results: list[dict[str, Any]],
    fixture_corpus_revision: str,
    required_coverage_ids: set[str],
) -> dict[str, Any]:
    scheduler_results = [
        result
        for result in case_results
        if result.get("scheduler_engine") in SCHEDULER_ENGINES
    ]
    expected_cases = set(required_coverage_ids)
    engines = []
    all_revisions = set()
    diagnostics = []
    missing_schema_revision_cases = sorted(
        result["id"]
        for result in scheduler_results
        if result.get("schema_revision") is None
    )
    if missing_schema_revision_cases:
        diagnostics.append(
            {
                "code": "missing_schema_revision_cases",
                "cases": missing_schema_revision_cases,
                "case_timeouts": sorted(
                    result["id"]
                    for result in scheduler_results
                    if result["id"] in missing_schema_revision_cases
                    and result.get("failure_kind") == "case_timeout"
                ),
                "evidence_collection_failures": sorted(
                    result["id"]
                    for result in scheduler_results
                    if result["id"] in missing_schema_revision_cases
                    and result.get("evidence_collection_error")
                ),
            }
        )
    for engine in SCHEDULER_ENGINES:
        results = [
            result for result in scheduler_results if result["scheduler_engine"] == engine
        ]
        coverage_counts: dict[str, int] = {}
        for result in results:
            for coverage_id in result.get("coverage_ids", [result["base_id"]]):
                coverage_counts[coverage_id] = coverage_counts.get(coverage_id, 0) + 1
        actual_cases = set(coverage_counts)
        matrix_complete = actual_cases == expected_cases and all(
            count == 1 for count in coverage_counts.values()
        )
        revisions = {
            result["schema_revision"]
            for result in results
            if result.get("schema_revision") is not None
        }
        schema_complete = len(revisions) == 1 and all(
            result.get("schema_revision") in revisions for result in results
        )
        if not matrix_complete:
            diagnostics.append(
                {
                    "code": "engine_case_matrix_incomplete",
                    "engine": engine,
                    "missing_cases": sorted(expected_cases - actual_cases),
                    "extra_cases": sorted(actual_cases - expected_cases),
                }
            )
        if not schema_complete:
            diagnostics.append(
                {
                    "code": "engine_schema_revision_invalid",
                    "engine": engine,
                    "schema_revisions": sorted(revisions),
                    "missing_cases": sorted(
                        result["id"]
                        for result in results
                        if result.get("schema_revision") is None
                    ),
                }
            )
        all_revisions.update(revisions)
        engines.append(
            {
                "engine": engine,
                "status": (
                    "pass"
                    if results
                    and matrix_complete
                    and schema_complete
                    and all(result["status"] == "pass" for result in results)
                    else "fail"
                ),
                "schema_revision": next(iter(revisions), None),
                "cases": [
                    {
                        "id": result["base_id"],
                        "status": result["status"],
                        "evidence_id": result["id"],
                    }
                    for result in results
                ],
            }
        )
    if len(all_revisions) > 1:
        diagnostics.append(
            {
                "code": "scheduler_schema_revision_mismatch",
                "schema_revisions": sorted(all_revisions),
            }
        )
    return {
        "schema_version": SCHEDULER_ACCEPTANCE_REPORT_SCHEMA_VERSION,
        "status": (
            "pass"
            if scheduler_results
            and not diagnostics
            and all(engine["status"] == "pass" for engine in engines)
            else "fail"
        ),
        "git_sha": run_record["git_sha"],
        "runtime_schema_revision": (
            next(iter(all_revisions)) if len(all_revisions) == 1 else None
        ),
        "image": run_record["image"],
        "image_digest": run_record["image_digest"],
        "fixture_corpus_revision": fixture_corpus_revision,
        "manifest_sha256": run_record["manifest_sha256"],
        "missing_schema_revision_cases": missing_schema_revision_cases,
        "engines": engines,
        "diagnostics": diagnostics,
    }


def scheduler_coverage_report(
    *,
    run_record: dict[str, Any],
    case_results: list[dict[str, Any]],
    required_coverage_ids: set[str],
    secret_scan_status: str,
) -> dict[str, Any]:
    observed: dict[str, list[str]] = {}
    for result in case_results:
        for coverage_id in result.get("coverage_ids", []):
            observed.setdefault(coverage_id, []).append(result["id"])
    missing = sorted(required_coverage_ids - observed.keys())
    invalid_counts = {
        coverage_id: evidence_ids
        for coverage_id, evidence_ids in sorted(observed.items())
        if coverage_id in required_coverage_ids
        and len(evidence_ids) != len(SCHEDULER_ENGINES)
    }
    failed = sorted(result["id"] for result in case_results if result["status"] != "pass")
    return {
        "schema_version": SCHEDULER_COVERAGE_REPORT_SCHEMA_VERSION,
        "status": "pass" if not missing and not invalid_counts and not failed and secret_scan_status == "pass" else "fail",
        "git_sha": run_record["git_sha"],
        "image_digest": run_record["image_digest"],
        "manifest_sha256": run_record["manifest_sha256"],
        "profile": run_record["profile"],
        "engines": list(SCHEDULER_ENGINES),
        "required_coverage_ids": sorted(required_coverage_ids),
        "observed": observed,
        "missing_coverage_ids": missing,
        "duplicate_or_incomplete_coverage_ids": invalid_counts,
        "unexpected_coverage_ids": sorted(observed.keys() - required_coverage_ids),
        "failed_evidence_ids": failed,
        "secret_scan": secret_scan_status,
    }


def scheduler_live_canary_report(
    *,
    run_record: dict[str, Any],
    case_results: list[dict[str, Any]],
    scheduler_acceptance_status: str,
    scheduler_coverage_status: str,
    secret_scan_status: str,
) -> dict[str, Any]:
    scheduler_results = [
        result
        for result in case_results
        if result.get("scheduler_engine") in SCHEDULER_ENGINES
    ]
    return {
        "schema_version": SCHEDULER_LIVE_CANARY_REPORT_SCHEMA_VERSION,
        "status": (
            "pass"
            if scheduler_results
            and all(result["status"] == "pass" for result in scheduler_results)
            and scheduler_acceptance_status == "pass"
            and scheduler_coverage_status == "pass"
            and secret_scan_status == "pass"
            else "fail"
        ),
        "git_sha": run_record["git_sha"],
        "image_digest": run_record["image_digest"],
        "manifest_sha256": run_record["manifest_sha256"],
        "profile": run_record["profile"],
        "provider_mode": run_record["provider_mode"],
        "model_route": run_record["model_route"],
        "tool_assertion_mode": run_record["tool_assertion_mode"],
        "scheduler_acceptance": scheduler_acceptance_status,
        "scheduler_coverage": scheduler_coverage_status,
        "secret_scan": secret_scan_status,
        "provider_rounds": sum(
            int(result.get("provider_rounds") or 0) for result in scheduler_results
        ),
        "provider_attempts": sum(
            int(result.get("provider_attempts") or 0) for result in scheduler_results
        ),
        "provider_retries": sum(
            int(result.get("provider_retries") or 0) for result in scheduler_results
        ),
        "behavioral_variances": [
            {
                "case_id": result["id"],
                **variance,
            }
            for result in scheduler_results
            for variance in result.get("behavioral_variances", [])
        ],
        "cases": [
            {
                "id": result["id"],
                "base_id": result["base_id"],
                "scheduler_engine": result["scheduler_engine"],
                "status": result["status"],
                "error": result["error"],
                "provider_rounds": result.get("provider_rounds", 0),
                "provider_attempts": result.get("provider_attempts", 0),
                "provider_retries": result.get("provider_retries", 0),
                "tool_counts": result.get("tool_counts", {}),
                "behavioral_variances": result.get("behavioral_variances", []),
            }
            for result in scheduler_results
        ],
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
    parser.add_argument("--profile")
    parser.add_argument("--env-file", type=Path)
    parser.add_argument("--config-file", type=Path)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument("--keep-on-failure", action="store_true")
    parser.add_argument("--timeout", type=int)
    parser.add_argument("--scheduler-matrix", action="store_true")
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--validate-manifest", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    manifest = json.loads(args.manifest.read_text())
    validate_manifest(manifest)
    profile = resolve_profile(manifest, args.profile)
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
        requested=args.cases or profile.get("case_ids") or None,
        suite=args.suite,
        tags=args.tag,
    )
    if args.scheduler_matrix:
        require(
            any("scheduler" in case.get("tags", []) for case in selected),
            "--scheduler-matrix requires at least one scheduler-tagged case",
        )
    requires_model = any(
        case.get("requires_model", True)
        and case.get("provider_mode", profile.get("provider_mode", "live")) == "live"
        for case in selected
    )
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
    config_file_value = args.config_file or first_env("HOLON_E2E_CONFIG_FILE")
    config_file = Path(config_file_value).resolve() if config_file_value else None
    runtime_config = load_runtime_config(config_file)
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
    image_digest = (
        args.image_digest
        or next(iter(identity.get("repo_digests") or []), None)
    )
    run_record = {
        "schema_version": EVIDENCE_SCHEMA_VERSION,
        "started_at": started_at,
        "git_sha": git_sha,
        "image": identity,
        "image_digest": image_digest,
        "previous_image": args.previous_image,
        "model_route": model,
        "suite": args.suite,
        "cases": [case["id"] for case in selected],
        "credential_env_names": credential_envs,
        "env_file_used": env_file is not None,
        "config_file_used": config_file is not None,
        "manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
        "scheduler_matrix": args.scheduler_matrix,
        "profile": args.profile,
        "gate_kind": profile.get("gate_kind", "suite"),
        "provider_mode": profile.get("provider_mode", "live"),
        "tool_assertion_mode": profile.get("tool_assertion_mode", "strict"),
    }
    write_json(evidence_root / "run.json", run_record)

    timeout_override = args.timeout or first_env(
        "HOLON_E2E_TIMEOUT_SECONDS", "HOLON_LIVE_TIMEOUT_SECONDS"
    )
    keep_on_failure = args.keep_on_failure or env_flag(
        "HOLON_E2E_KEEP", "HOLON_LIVE_KEEP"
    )
    case_results: list[dict[str, Any]] = []
    control_tokens: list[str] = []
    expanded_cases = expand_case_matrix(
        selected,
        scheduler_matrix=args.scheduler_matrix,
    )
    if any(
        case.get("provider_mode", profile.get("provider_mode", "live")) == "stub"
        for case, _ in expanded_cases
    ):
        run(
            [
                "docker",
                "build",
                "--tag",
                OPENAI_STUB_IMAGE,
                str(OPENAI_STUB_ROOT),
            ],
            capture=False,
        )
    for case, scheduler_engine in expanded_cases:
        case_id = case["id"]
        evidence_id = (
            f"{case_id}-{scheduler_engine}" if scheduler_engine is not None else case_id
        )
        case_started = time.monotonic()
        engine_suffix = (
            f" using scheduler={scheduler_engine}" if scheduler_engine else ""
        )
        print(f"Running {case_id} with {model}{engine_suffix}")
        runtime_env = dict(case.get("runtime_env", {}))
        if scheduler_engine is not None:
            runtime_env["HOLON_SCHEDULER"] = scheduler_engine
        harness = CaseHarness(
            case_id=evidence_id,
            image=image,
            model=model,
            requires_model=case.get("requires_model", True),
            credential_envs=credential_envs,
            env_file=env_file,
            runtime_config=runtime_config,
            runtime_env=runtime_env,
            evidence_root=evidence_root,
            timeout_seconds=(
                int(timeout_override)
                if timeout_override
                else int(case["timeout_seconds"])
            ),
            keep=False,
            provider_mode=case.get(
                "provider_mode", profile.get("provider_mode", "live")
            ),
            stub_scenario=case.get("stub_scenario", profile.get("stub_scenario")),
            model_runtime_override=case.get("model_runtime_override"),
            tool_assertion_mode=profile.get("tool_assertion_mode", "strict"),
        )
        control_tokens.append(harness.token)
        error_text = ""
        failure_kind: str | None = None
        evidence_collection_error = ""
        try:
            CASE_RUNNERS[case_id](harness, case)
            harness.assert_stub_complete()
            harness.capture_context("final")
            status = "pass"
            print(f"PASS {case_id}")
        except Exception as error:
            status = "fail"
            error_text = f"{type(error).__name__}: {error}"
            if isinstance(error, TimeoutError):
                failure_kind = "case_timeout"
            (harness.evidence / "failure.txt").write_text(error_text + "\n")
            try:
                harness.capture_context("failure")
            except Exception:
                pass
            try:
                harness.runtime_db_snapshot("failure-final")
            except Exception as snapshot_error:
                evidence_collection_error = (
                    f"{type(snapshot_error).__name__}: {snapshot_error}"
                )
                write_json(
                    harness.evidence / "failure-evidence-collection.json",
                    {
                        "status": "failed",
                        "runtime_db_snapshot_error": evidence_collection_error,
                    },
                )
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
            "id": evidence_id,
            "base_id": case_id,
            "scheduler_engine": scheduler_engine,
            "tier": case["tier"],
            "tags": case.get("tags", []),
            "status": status,
            "error": error_text,
            "failure_kind": failure_kind,
            "evidence_collection_error": evidence_collection_error,
            "duration_seconds": round(time.monotonic() - case_started, 3),
            "cleanup": cleanup_result["status"],
            "cleanup_errors": cleanup_result["errors"],
            "schema_revision": collect_case_schema_revision(harness.evidence),
            "coverage_ids": case.get("coverage_ids", []),
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
                "behavioral_variances": [],
                "provider_rounds": 0,
                "provider_attempts": 0,
                "provider_retries": 0,
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
    report_failures: list[str] = []
    if args.scheduler_matrix:
        required_coverage_ids = set(profile.get("required_coverage_ids", []))
        if not required_coverage_ids:
            required_coverage_ids = {
                coverage_id
                for case in selected
                for coverage_id in case.get("coverage_ids", [])
            }
        coverage_report = scheduler_coverage_report(
            run_record=run_record,
            case_results=case_results,
            required_coverage_ids=required_coverage_ids,
            secret_scan_status=scan["status"],
        )
        write_json(evidence_root / "scheduler-coverage-report.json", coverage_report)
        if coverage_report["status"] != "pass":
            report_failures.append("scheduler-coverage-report: fail")
        report = scheduler_acceptance_report(
            run_record=run_record,
            case_results=case_results,
            fixture_corpus_revision=manifest["scheduler_fixture_corpus_revision"],
            required_coverage_ids=required_coverage_ids,
        )
        write_json(evidence_root / "scheduler-acceptance-report.json", report)
        if report["status"] != "pass":
            report_failures.append("scheduler-acceptance-report: fail")
        if profile.get("gate_kind") == "live_canary":
            canary_report = scheduler_live_canary_report(
                run_record=run_record,
                case_results=case_results,
                scheduler_acceptance_status=report["status"],
                scheduler_coverage_status=coverage_report["status"],
                secret_scan_status=scan["status"],
            )
            write_json(
                evidence_root / "scheduler-live-canary-report.json",
                canary_report,
            )
            if canary_report["status"] != "pass":
                report_failures.append("scheduler-live-canary-report: fail")
    write_junit(evidence_root / "junit.xml", case_results, duration)

    print(f"Evidence: {evidence_root}")
    failures = [
        f"{case['id']}: {case['error']}"
        for case in case_results
        if case["status"] != "pass"
    ] + report_failures
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    return 0
