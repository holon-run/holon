#!/usr/bin/env python3
"""Manual Docker acceptance against a real LLM and the public Holon HTTP API."""

from __future__ import annotations

import argparse
import json
import os
import secrets
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT / "tests/manual/docker-live-acceptance.json"
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
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def inferred_credential_env(model: str) -> str | None:
    provider = model.split("/", 1)[0].split("@", 1)[0]
    return {
        "openai": "OPENAI_API_KEY",
        "anthropic": "ANTHROPIC_AUTH_TOKEN",
        "deepseek": "DEEPSEEK_API_KEY",
        "deepseek-anthropic": "DEEPSEEK_API_KEY",
        "xai": "XAI_API_KEY",
    }.get(provider)


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
            "HOLON_MODEL_FALLBACKS=",
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
                time.sleep(0.25)
        require(bool(port), "failed to resolve the container's published port")
        self.base_url = f"http://127.0.0.1:{port}"
        self.wait_readiness()

    def stop(self) -> None:
        self.capture_logs()
        self.docker("rm", "-f", self.container, check=False)
        self.base_url = ""

    def restart(self) -> None:
        self.stop()
        self.start()

    def cleanup(self) -> None:
        if self.keep:
            print(
                f"Keeping container resources for {self.case_id}: "
                f"container={self.container} volume={self.volume} network={self.network}",
                file=sys.stderr,
            )
            return
        self.docker("rm", "-f", self.container, check=False)
        self.docker("volume", "rm", self.volume, check=False)
        self.docker("network", "rm", self.network, check=False)

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
    ) -> Any:
        data = None
        headers = {"Authorization": f"Bearer {self.token}"}
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
        if not payload:
            return None
        return json.loads(payload)

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
                self.request("GET", "/api/control/runtime/readiness")
                return
            except Exception as error:  # readiness is intentionally polled
                last_error = str(error)
                time.sleep(1)
        self.capture_logs()
        raise TimeoutError(f"Holon did not become ready: {last_error}")

    def state(self, label: str) -> dict[str, Any]:
        value = self.request("GET", "/api/agents/default/state")
        write_json(self.evidence / f"{label}-state.json", value)
        return value

    def work_items(self, label: str) -> list[dict[str, Any]]:
        value = self.request("GET", "/api/agents/default/work-items?limit=50")
        write_json(self.evidence / f"{label}-work-items.json", value)
        return value

    def events(self, label: str) -> list[dict[str, Any]]:
        page = self.request(
            "GET", "/api/agents/default/events?limit=500&order=asc"
        )
        write_json(self.evidence / f"{label}-events.json", page)
        return page["events"]

    def capture_context(self, label: str) -> None:
        write_json(
            self.evidence / f"{label}-briefs.json",
            self.request("GET", "/api/agents/default/briefs?limit=50"),
        )
        write_json(
            self.evidence / f"{label}-transcript.json",
            self.request("GET", "/api/agents/default/transcript?limit=200"),
        )
        self.state(label)
        self.work_items(label)
        self.events(label)

    def prompt(self, label: str, text: str) -> tuple[int, dict[str, Any]]:
        before = self.state(f"{label}-before")
        baseline = int(before["agent"]["agent"]["turn_index"])
        response = self.request(
            "POST",
            "/api/control/agents/default/prompt",
            {"text": text},
        )
        write_json(self.evidence / f"{label}-prompt-response.json", response)
        (self.evidence / f"{label}-prompt.txt").write_text(text + "\n")

        deadline = time.monotonic() + self.timeout_seconds
        last_state = before
        while time.monotonic() < deadline:
            last_state = self.request("GET", "/api/agents/default/state")
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
        failures = [
            event
            for event in events
            if event["event_type"] == "tool_execution_failed"
            and int(event["payload"].get("turn_index", 0)) > baseline_turn
        ]
        require(not failures, f"tool failures occurred in {label}: {failures}")
        return [
            event
            for event in events
            if event["event_type"] == "tool_executed"
            and event["payload"].get("status") == "success"
            and int(event["payload"].get("turn_index", 0)) > baseline_turn
        ]

    def assert_tools(
        self, label: str, baseline_turn: int, expected: list[str]
    ) -> list[dict[str, Any]]:
        events = self.successful_tool_events(label, baseline_turn)
        actual = [event["payload"].get("tool_name") for event in events]
        missing = [name for name in expected if name not in actual]
        require(not missing, f"{label} missing successful tools {missing}; got {actual}")
        return events

    def tool_detail(self, event: dict[str, Any], label: str) -> dict[str, Any]:
        execution_id = event["payload"]["tool_execution_id"]
        detail = self.request(
            "GET",
            f"/api/agents/default/tool-executions/{execution_id}",
        )
        write_json(self.evidence / f"{label}-{execution_id}.json", detail)
        return detail

    def agent_home_file(self, relative_path: str, label: str) -> dict[str, Any]:
        encoded_path = "/".join(
            urllib.parse.quote(part, safe="") for part in relative_path.split("/")
        )
        value = self.request(
            "GET",
            f"/api/workspaces/agent_home%3Adefault/files/{encoded_path}",
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


def run_workspace_case(harness: CaseHarness, case: dict[str, Any]) -> None:
    harness.initialize_workspace()
    harness.start()
    attached = harness.request(
        "POST",
        "/api/control/agents/default/workspace/attach",
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
    create_events = harness.assert_tools(
        "workspace-create", baseline, create_phase["expected_tools"]
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
    harness.assert_tools(
        "workspace-recover", baseline, recover_phase["expected_tools"]
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
    harness.assert_tools("workitem-wait", baseline, wait_phase["expected_tools"])
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
        state["agent"]["agent"].get("current_work_item_id") == work_item_id,
        "created WorkItem was not current after WaitFor",
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
        restart_state["agent"]["agent"].get("current_work_item_id")
        == work_item_id,
        "current WorkItem focus did not survive restart",
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
    harness.assert_tools(
        "workitem-complete", baseline, complete_phase["expected_tools"]
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
    require(
        completion_marker in (completed.get("result_summary") or ""),
        f"completion result did not preserve marker {completion_marker}: {completed}",
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", default="holon:dev")
    parser.add_argument(
        "--case",
        action="append",
        choices=["workspace-restart-lifecycle", "workitem-wait-restart-complete"],
        dest="cases",
    )
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--evidence-dir", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    require(shutil.which("docker") is not None, "docker is required")
    model = os.environ.get("HOLON_LIVE_MODEL", "").strip()
    require(model, "set HOLON_LIVE_MODEL to the real model route to test")

    raw_names = os.environ.get("HOLON_LIVE_CREDENTIAL_ENVS", "").strip()
    credential_envs = [name.strip() for name in raw_names.split(",") if name.strip()]
    env_file_value = os.environ.get("HOLON_LIVE_DOCKER_ENV_FILE", "").strip()
    env_file = Path(env_file_value).resolve() if env_file_value else None
    if not credential_envs and env_file is None:
        inferred = inferred_credential_env(model)
        require(
            inferred is not None,
            "set HOLON_LIVE_CREDENTIAL_ENVS or HOLON_LIVE_DOCKER_ENV_FILE "
            f"for model {model}",
        )
        credential_envs = [inferred]
    for name in credential_envs:
        require(name in os.environ, f"required credential environment {name} is unset")
    if env_file is not None:
        require(env_file.is_file(), f"env file does not exist: {env_file}")

    if not args.skip_build:
        run(["docker", "build", "--tag", args.image, str(ROOT)], capture=False)

    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    evidence_root = (
        args.evidence_dir.resolve()
        if args.evidence_dir
        else ROOT / "target/docker-live-acceptance" / timestamp
    )
    evidence_root.mkdir(parents=True, exist_ok=True)
    manifest = json.loads(args.manifest.read_text())
    write_json(
        evidence_root / "run.json",
        {
            "started_at": datetime.now(timezone.utc).isoformat(),
            "image": args.image,
            "model": model,
            "cases": args.cases
            or [
                "workspace-restart-lifecycle",
                "workitem-wait-restart-complete",
            ],
            "credential_env_names": credential_envs,
            "env_file_used": env_file is not None,
        },
    )

    selected = args.cases or [
        "workspace-restart-lifecycle",
        "workitem-wait-restart-complete",
    ]
    timeout_seconds = int(os.environ.get("HOLON_LIVE_TIMEOUT_SECONDS", "600"))
    keep = os.environ.get("HOLON_LIVE_KEEP", "") == "1"
    failures: list[str] = []
    for case_id in selected:
        case = find_case(manifest, case_id)
        print(f"Running {case_id} with {model}")
        harness = CaseHarness(
            case_id=case_id,
            image=args.image,
            model=model,
            credential_envs=credential_envs,
            env_file=env_file,
            evidence_root=evidence_root,
            timeout_seconds=timeout_seconds,
            keep=keep,
        )
        try:
            if case_id == "workspace-restart-lifecycle":
                run_workspace_case(harness, case)
            else:
                run_workitem_case(harness, case)
            harness.capture_context("final")
            print(f"PASS {case_id}")
        except Exception as error:
            failures.append(f"{case_id}: {error}")
            (harness.evidence / "failure.txt").write_text(f"{type(error).__name__}: {error}\n")
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
            harness.cleanup()

    print(f"Evidence: {evidence_root}")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2)
