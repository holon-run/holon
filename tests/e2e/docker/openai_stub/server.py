#!/usr/bin/env python3
"""Dependency-free deterministic OpenAI Responses API scheduler stub."""
from __future__ import annotations
import argparse, json, re, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

CALLBACK_CAPABILITY_PATTERN = re.compile(
    r"(/api/callbacks/(?:wake|enqueue)/)[A-Za-z0-9_-]+"
)
SCENARIOS = (
    "runtime-upgrade-v030",
    "scheduler-task-wait",
    "scheduler-provider-retry",
    "scheduler-multi",
    "scheduler-external",
    "scheduler-operator",
    "scheduler-concurrent",
    "scheduler-interject",
    "scheduler-compaction",
    "scheduler-worktree",
    "scheduler-spawn",
    "scheduler-checkpoint",
)

def redact_evidence(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: redact_evidence(item) for key, item in value.items()}
    if isinstance(value, list):
        return [redact_evidence(item) for item in value]
    if isinstance(value, str):
        return CALLBACK_CAPABILITY_PATTERN.sub(r"\1<redacted>", value)
    return value

def response(items: list[dict[str, Any]], rid: str) -> dict[str, Any]:
    return {"id": rid, "status": "completed", "usage": {"input_tokens": 100, "output_tokens": 10}, "output": items}

def text_item(text: str) -> dict[str, Any]:
    return {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}

def call_item(cid: str, name: str, args: dict[str, Any]) -> dict[str, Any]:
    return {"type": "function_call", "id": f"fc_{cid}", "call_id": cid, "name": name, "arguments": json.dumps(args, separators=(",", ":"))}

class Scenario:
    def __init__(self, name: str) -> None:
        self.name, self.phase, self.work_ids, self.markers = name, 0, [], {}
        self.task_ids: list[str] = []
        self.workspace_ids: list[str] = []
        self.execution_root_ids: list[str] = []
        self.first_turn_closed = False
        self.current_input_text = ""
        self.extra_requests = 0
        self.lock = threading.Lock()

    def observe(self, request: dict[str, Any]) -> str:
        raw = json.dumps(request, ensure_ascii=False)
        for wid in re.findall(r"work_[0-9a-f]{15}", raw):
            if wid not in self.work_ids:
                self.work_ids.append(wid)
        for task_id in re.findall(r"task_[0-9a-f]{15}", raw):
            if task_id not in self.task_ids:
                self.task_ids.append(task_id)
        for workspace_id in re.findall(r"ws_[0-9a-f]+", raw):
            if workspace_id not in self.workspace_ids:
                self.workspace_ids.append(workspace_id)
        for execution_root_id in re.findall(r"git_worktree_root:[^\"\\\\]+", raw):
            if execution_root_id not in self.execution_root_ids:
                self.execution_root_ids.append(execution_root_id)
        for key, pattern in {
            "multi_a": r"SCHEDULER-MULTI-A-[0-9a-f]+", "multi_b": r"SCHEDULER-MULTI-B-[0-9a-f]+",
            "multi_complete_a": r"SCHEDULER-MULTI-COMPLETE-A-[0-9a-f]+", "multi_complete_b": r"SCHEDULER-MULTI-COMPLETE-B-[0-9a-f]+",
            "external": r"SCHEDULER-EXTERNAL-WAIT-[0-9a-f]+", "external_complete": r"SCHEDULER-EXTERNAL-COMPLETE-[0-9a-f]+",
            "operator": r"SCHEDULER-OPERATOR-WAIT-[0-9a-f]+", "operator_complete": r"SCHEDULER-OPERATOR-COMPLETE-[0-9a-f]+",
            "task": r"SCHEDULER-TASK-WAIT-[0-9a-f]+", "task_result": r"SCHEDULER-TASK-RESULT-[0-9a-f]+",
            "task_complete": r"SCHEDULER-TASK-WAIT-COMPLETE-[0-9a-f]+",
            "provider": r"SCHEDULER-PROVIDER-RETRY-[0-9a-f]+", "provider_complete": r"SCHEDULER-PROVIDER-RETRY-COMPLETE-[0-9a-f]+",
            "concurrent_a": r"SCHEDULER-CONCURRENT-A-[0-9a-f]+", "concurrent_b": r"SCHEDULER-CONCURRENT-B-[0-9a-f]+",
            "concurrent_complete_a": r"SCHEDULER-CONCURRENT-COMPLETE-A-[0-9a-f]+", "concurrent_complete_b": r"SCHEDULER-CONCURRENT-COMPLETE-B-[0-9a-f]+",
            "interject_a": r"SCHEDULER-INTERJECT-A-[0-9a-f]+", "interject_b": r"SCHEDULER-INTERJECT-B-[0-9a-f]+",
            "interject_complete_a": r"SCHEDULER-INTERJECT-COMPLETE-A-[0-9a-f]+", "interject_complete_b": r"SCHEDULER-INTERJECT-COMPLETE-B-[0-9a-f]+",
            "compaction": r"SCHEDULER-COMPACTION-[0-9a-f]+", "compaction_complete": r"SCHEDULER-COMPACTION-COMPLETE-[0-9a-f]+",
            "worktree": r"SCHEDULER-WORKTREE-[0-9a-f]+", "worktree_complete": r"SCHEDULER-WORKTREE-COMPLETE-[0-9a-f]+",
            "spawn": r"SCHEDULER-SPAWN-[0-9a-f]+", "spawn_complete": r"SCHEDULER-SPAWN-COMPLETE-[0-9a-f]+",
            "spawn_child": r"SCHEDULER-SPAWN-CHILD-[0-9a-f]+",
            "checkpoint_a": r"SCHEDULER-REPLAY-A-[0-9a-f]+", "checkpoint_b": r"SCHEDULER-REPLAY-B-[0-9a-f]+",
            "checkpoint_complete_a": r"SCHEDULER-REPLAY-COMPLETE-A-[0-9a-f]+", "checkpoint_complete_b": r"SCHEDULER-REPLAY-COMPLETE-B-[0-9a-f]+",
            "branch": r"e2e-worktree-[0-9a-f]+",
            "callback": r"docker-e2e:[0-9a-f]+",
        }.items():
            match = re.search(pattern, raw)
            if match:
                self.markers[key] = match.group(0)
        return raw

    @staticmethod
    def current_input(request: dict[str, Any]) -> str:
        texts = [
            content["text"]
            for item in request.get("input", [])
            if isinstance(item, dict)
            for content in item.get("content", [])
            if isinstance(content, dict) and isinstance(content.get("text"), str)
        ]
        for text in reversed(texts):
            marker = "## current_input\nCurrent input:\n"
            if marker in text:
                return text.rsplit(marker, 1)[1]
        return ""

    def call(self, name: str, args: dict[str, Any], text: str | None = None) -> tuple[int, dict[str, Any]]:
        cid = f"{self.name}-{self.phase}-{name.lower()}"
        self.phase += 1
        items = ([text_item(text)] if text else []) + [call_item(cid, name, args)]
        return 200, response(items, f"resp_{cid}")

    def consume(self, request: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        with self.lock:
            raw = self.observe(request)
            if request.get("instructions") == "Reply with exactly OK.":
                return 200, response([text_item("OK")], "resp_doctor")
            current_input = self.current_input(request)
            self.current_input_text = current_input
            if (
                self.name == "scheduler-spawn"
                and "SCHEDULER-SPAWN-CHILD-" in current_input
                and "Scheduler Docker E2E case" not in current_input
            ):
                marker = self.markers.get(
                    "spawn_child", "SCHEDULER-SPAWN-CHILD-deterministic"
                )
                return 200, response(
                    [text_item(f"Child completed {marker}.")],
                    "resp_scheduler_spawn_child",
                )
            if (
                "[trigger:internal_followup][runtime_instruction][InternalFollowup]"
                in current_input
                and "This is the first run of Holon" in current_input
            ):
                return 200, response([text_item("Deterministic Holon test runtime ready.")], "resp_intro")
            if self.name == "runtime-upgrade-v030":
                match = re.search(r"UPGRADE-V030-(?:OLD|NEW)-[0-9a-f]+", raw)
                if match and self.phase < self.expected_phase():
                    self.phase += 1
                    return 200, response(
                        [text_item(match.group(0))],
                        f"resp_runtime_upgrade_v030_{self.phase}",
                    )
            if self.phase == self.expected_phase() - 1:
                self.phase += 1
                return 200, response(
                    [text_item("Deterministic scheduler scenario complete.")],
                    f"resp_{self.name}_complete",
                )
            if self.phase >= self.expected_phase():
                self.extra_requests += 1
                return 409, {
                    "error": {
                        "type": "transcript_exhausted",
                        "message": str(self.phase),
                    }
                }
            if self.name == "scheduler-multi":
                return self.multi()
            if self.name == "scheduler-task-wait":
                return self.task_wait()
            if self.name == "scheduler-provider-retry":
                return self.provider_retry()
            if self.name in {"scheduler-external", "scheduler-operator"}:
                kind = self.name.removeprefix("scheduler-")
                wake = "operator_input" if kind == "operator" else kind
                return self.wait(kind, kind, wake)
            if self.name == "scheduler-concurrent":
                return self.concurrent()
            if self.name == "scheduler-compaction":
                return self.compaction()
            if self.name == "scheduler-interject":
                return self.interject("interject", "operator_input")
            if self.name == "scheduler-worktree":
                return self.worktree()
            if self.name == "scheduler-spawn":
                return self.spawn()
            if self.name == "scheduler-checkpoint":
                return self.checkpoint()
            return 409, {"error": {"type": "unknown_scenario", "message": self.name}}

    def multi(self) -> tuple[int, dict[str, Any]]:
        if self.phase == 0:
            return self.call("CreateWorkItem", {"objective": self.markers.get("multi_a", "SCHEDULER-MULTI-A"), "plan_status": "ready", "todo_list": [{"text": "seed-a", "state": "completed"}, {"text": "complete-a", "state": "pending"}]})
        if self.phase == 1:
            return self.call("CreateWorkItem", {"objective": self.markers.get("multi_b", "SCHEDULER-MULTI-B"), "plan_status": "ready", "todo_list": [{"text": "seed-b", "state": "completed"}, {"text": "complete-b", "state": "pending"}]})
        if self.phase == 2:
            self.phase += 1
            return 200, response([text_item("Created two deterministic WorkItems.")], "resp_multi_seeded")
        index = 0 if self.phase < 7 else 1
        if len(self.work_ids) <= index:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        wid = self.work_ids[index]
        if self.phase == 7 and (
            "[trigger:system_tick]" not in self.current_input_text
            or self.markers["multi_b"] not in self.current_input_text
        ):
            if self.first_turn_closed:
                return 409, {
                    "error": {
                        "type": "transcript_exhausted",
                        "message": str(self.phase),
                    }
                }
            self.first_turn_closed = True
            return 200, response(
                [text_item("Completed the first deterministic WorkItem.")],
                "resp_multi_first_complete",
            )
        steps = {
            3: ("AgentGet", {}), 4: ("ListWorkItems", {"filter": "current", "include_todo_list": True}),
            5: ("UpdateWorkItem", {"work_item_id": wid, "todo_list": [{"text": "seed-a", "state": "completed"}, {"text": "complete-a", "state": "completed"}]}),
            7: ("GetWorkspaceState", {}), 8: ("ListWorkItems", {"filter": "current", "include_todo_list": True}),
            9: ("UpdateWorkItem", {"work_item_id": wid, "todo_list": [{"text": "seed-b", "state": "completed"}, {"text": "complete-b", "state": "completed"}]}),
        }
        if self.phase in steps:
            return self.call(*steps[self.phase])
        if self.phase == 6:
            completion = self.markers.get("multi_complete_a") or self.markers[
                "multi_a"
            ].replace("SCHEDULER-MULTI-A-", "SCHEDULER-MULTI-COMPLETE-A-")
            return self.call("CompleteWorkItem", {"work_item_id": wid}, completion)
        if self.phase == 10:
            completion = self.markers.get("multi_complete_b") or self.markers[
                "multi_b"
            ].replace("SCHEDULER-MULTI-B-", "SCHEDULER-MULTI-COMPLETE-B-")
            return self.call("CompleteWorkItem", {"work_item_id": wid}, completion)
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def wait(
        self, marker_key: str, todo_prefix: str, wake: str
    ) -> tuple[int, dict[str, Any]]:
        if self.phase == 0:
            return self.call("CreateWorkItem", {"objective": self.markers.get(marker_key, f"SCHEDULER-{marker_key.upper()}-WAIT"), "plan_status": "ready", "todo_list": [{"text": f"{todo_prefix}-wait", "state": "pending"}, {"text": f"{todo_prefix}-complete", "state": "pending"}]})
        if not self.work_ids:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        wid = self.work_ids[0]
        if self.phase == 1:
            return self.call("PickWorkItem", {"work_item_id": wid})
        if self.phase == 2:
            args: dict[str, Any] = {"wake": wake, "reason": f"deterministic {marker_key} wait"}
            if wake == "external":
                args["resource"] = self.markers.get("callback", "docker-e2e:deterministic")
            return self.call("WaitFor", args)
        if self.phase == 3:
            return self.call("GetWorkItem", {"work_item_id": wid, "include_todo_list": True})
        if self.phase == 4:
            return self.call("UpdateWorkItem", {"work_item_id": wid, "todo_list": [{"text": f"{todo_prefix}-wait", "state": "completed"}, {"text": f"{todo_prefix}-complete", "state": "completed"}]})
        if self.phase == 5:
            completion = self.markers.get(f"{marker_key}_complete") or self.markers[
                marker_key
            ].replace(
                f"SCHEDULER-{marker_key.upper()}-WAIT-",
                f"SCHEDULER-{marker_key.upper()}-COMPLETE-",
            )
            return self.call("CompleteWorkItem", {"work_item_id": wid}, completion)
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def task_wait(self) -> tuple[int, dict[str, Any]]:
        if self.phase == 0:
            return self.call(
                "CreateWorkItem",
                {
                    "objective": self.markers.get("task", "SCHEDULER-TASK-WAIT"),
                    "plan_status": "ready",
                    "todo_list": [
                        {"text": "task-continuation", "state": "pending"},
                        {"text": "external-continuation", "state": "pending"},
                    ],
                },
            )
        if self.phase == 1:
            self.phase += 1
            return 200, response(
                [text_item("Created deterministic task-wait WorkItem.")],
                "resp_task_wait_created",
            )
        if not self.work_ids:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        wid = self.work_ids[0]
        if self.phase == 2:
            marker = self.markers.get("task_result", "SCHEDULER-TASK-RESULT")
            return self.call(
                "ExecCommand",
                {
                    "cmd": f"sleep 15; printf {marker}",
                    "yield_time_ms": 50,
                    "max_output_tokens": 100,
                },
            )
        if self.phase == 3:
            if not self.task_ids:
                return 409, {"error": {"type": "missing_task_id", "message": str(self.phase)}}
            return self.call(
                "WaitFor",
                {
                    "wake": "task_result",
                    "resource": self.task_ids[-1],
                    "reason": "deterministic task completion",
                },
            )
        if self.phase in {4, 6}:
            return self.call(
                "GetWorkItem",
                {"work_item_id": wid, "include_todo_list": True},
            )
        if self.phase == 5:
            return self.call(
                "WaitFor",
                {
                    "wake": "external",
                    "resource": self.markers.get("callback", "docker-e2e:deterministic"),
                    "reason": "deterministic external completion",
                },
            )
        if self.phase == 7:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": wid,
                    "todo_list": [
                        {"text": "task-continuation", "state": "completed"},
                        {"text": "external-continuation", "state": "completed"},
                    ],
                },
            )
        if self.phase == 8:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": wid},
                self.markers.get("task_complete", "SCHEDULER-TASK-WAIT-COMPLETE"),
            )
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def provider_retry(self) -> tuple[int, dict[str, Any]]:
        if not self.work_ids:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        wid = self.work_ids[0]
        if self.phase == 0:
            return self.call(
                "ListWorkItems",
                {"filter": "current", "include_todo_list": True},
            )
        if self.phase == 1:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": wid},
                self.markers.get(
                    "provider_complete", "SCHEDULER-PROVIDER-RETRY-COMPLETE"
                ),
            )
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def concurrent(self) -> tuple[int, dict[str, Any]]:
        if self.phase == 0:
            return self.call(
                "CreateWorkItem",
                {
                    "objective": self.markers.get("concurrent_a", "concurrent-a"),
                    "plan_status": "ready",
                    "todo_list": [
                        {"text": "concurrent-wait", "state": "pending"},
                        {"text": "concurrent-complete", "state": "pending"},
                    ],
                },
            )
        if not self.work_ids:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        if self.phase == 1:
            return self.call("PickWorkItem", {"work_item_id": self.work_ids[0]})
        if self.phase == 2:
            return self.call(
                "WaitFor",
                {
                    "wake": "external",
                    "resource": self.markers.get(
                        "callback", "docker-e2e:deterministic"
                    ),
                    "reason": "deterministic concurrent wait",
                },
            )
        if len(self.work_ids) < 2:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        if self.phase == 3:
            return self.call(
                "ListWorkItems",
                {"filter": "current", "include_todo_list": True},
            )
        if self.phase == 4:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": self.work_ids[1],
                    "todo_list": [
                        {"text": "concurrent-b-seed", "state": "completed"},
                        {"text": "concurrent-b-complete", "state": "completed"},
                    ],
                },
            )
        if self.phase == 5:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": self.work_ids[1]},
                self.markers.get(
                    "concurrent_complete_b", "concurrent-complete-b"
                ),
            )
        if self.phase == 6:
            marker = self.markers.get("concurrent_a", "SCHEDULER-CONCURRENT-A")
            if "[trigger:" not in self.current_input_text or marker not in self.current_input_text:
                self.phase += 1
                return 200, response(
                    [text_item("Completed deterministic concurrent WorkItem B.")],
                    "resp_concurrent_b_complete",
                )
            self.phase += 1
        if self.phase == 7:
            return self.call(
                "GetWorkItem",
                {"work_item_id": self.work_ids[0], "include_todo_list": True},
            )
        if self.phase == 8:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": self.work_ids[0],
                    "todo_list": [
                        {"text": "concurrent-wait", "state": "completed"},
                        {"text": "concurrent-complete", "state": "completed"},
                    ],
                },
            )
        if self.phase == 9:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": self.work_ids[0]},
                self.markers.get(
                    "concurrent_complete_a", "concurrent-complete-a"
                ),
            )
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def compaction(self) -> tuple[int, dict[str, Any]]:
        if self.phase == 0:
            return self.call(
                "CreateWorkItem",
                {
                    "objective": self.markers.get(
                        "compaction", "SCHEDULER-COMPACTION"
                    ),
                    "plan_status": "ready",
                    "todo_list": [
                        {"text": "compaction-wait", "state": "pending"},
                        {"text": "compaction-complete", "state": "pending"},
                    ],
                },
            )
        if not self.work_ids:
            return 409, {
                "error": {"type": "missing_work_item_id", "message": str(self.phase)}
            }
        wid = self.work_ids[0]
        if self.phase == 1:
            return self.call("PickWorkItem", {"work_item_id": wid})
        if self.phase in {2, 3, 4}:
            return self.call(
                "ExecCommand",
                {
                    "cmd": (
                        "i=0; while [ \"$i\" -lt 16000 ]; do "
                        f"printf 'compaction-payload-{self.phase}-%04d\\n' \"$i\"; "
                        "i=$((i+1)); done"
                    ),
                    "max_output_tokens": 64000,
                },
            )
        if self.phase == 5:
            return self.call(
                "WaitFor",
                {
                    "wake": "operator_input",
                    "reason": "deterministic compaction wait",
                },
            )
        if self.phase == 6:
            return self.call(
                "GetWorkItem",
                {"work_item_id": wid, "include_todo_list": True},
            )
        if self.phase == 7:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": wid,
                    "todo_list": [
                        {"text": "compaction-wait", "state": "completed"},
                        {"text": "compaction-complete", "state": "completed"},
                    ],
                },
            )
        if self.phase == 8:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": wid},
                self.markers.get(
                    "compaction_complete", "SCHEDULER-COMPACTION-COMPLETE"
                ),
            )
        return 409, {
            "error": {"type": "transcript_exhausted", "message": str(self.phase)}
        }

    def interject(self, prefix: str, wake: str) -> tuple[int, dict[str, Any]]:
        if self.phase == 0:
            return self.call(
                "CreateWorkItem",
                {
                    "objective": self.markers.get(f"{prefix}_a", prefix),
                    "plan_status": "ready",
                    "todo_list": [
                        {"text": f"{prefix}-wait", "state": "pending"},
                        {"text": f"{prefix}-complete", "state": "pending"},
                    ],
                },
            )
        if not self.work_ids:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        if self.phase == 1:
            return self.call("PickWorkItem", {"work_item_id": self.work_ids[0]})
        if self.phase == 2:
            args: dict[str, Any] = {
                "wake": wake,
                "reason": f"deterministic {prefix} wait",
            }
            if wake == "external":
                args["resource"] = self.markers.get(
                    "callback", "docker-e2e:deterministic"
                )
            return self.call("WaitFor", args)
        if self.phase == 3:
            return self.call(
                "CreateWorkItem",
                {
                    "objective": self.markers.get(f"{prefix}_b", f"{prefix}-b"),
                    "plan_status": "ready",
                    "todo_list": [
                        {"text": f"{prefix}-b-seed", "state": "completed"},
                        {"text": f"{prefix}-b-complete", "state": "pending"},
                    ],
                },
            )
        if self.phase == 4:
            self.phase += 1
            return 200, response(
                [text_item("Created deterministic interject WorkItem.")],
                f"resp_{prefix}_interject_created",
            )
        if len(self.work_ids) < 2:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        if self.phase == 5:
            return self.call(
                "ListWorkItems",
                {"filter": "current", "include_todo_list": True},
            )
        if self.phase == 6:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": self.work_ids[1],
                    "todo_list": [
                        {"text": f"{prefix}-b-seed", "state": "completed"},
                        {"text": f"{prefix}-b-complete", "state": "completed"},
                    ],
                },
            )
        if self.phase == 7:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": self.work_ids[1]},
                self.markers.get(f"{prefix}_complete_b", f"{prefix}-complete-b"),
            )
        if self.phase == 8:
            self.phase += 1
            return 200, response(
                [text_item("Completed deterministic interject WorkItem.")],
                f"resp_{prefix}_interject_complete",
            )
        if self.phase == 9:
            return self.call(
                "GetWorkItem",
                {"work_item_id": self.work_ids[0], "include_todo_list": True},
            )
        if self.phase == 10:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": self.work_ids[0],
                    "todo_list": [
                        {"text": f"{prefix}-wait", "state": "completed"},
                        {"text": f"{prefix}-complete", "state": "completed"},
                    ],
                },
            )
        if self.phase == 11:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": self.work_ids[0]},
                self.markers.get(f"{prefix}_complete_a", f"{prefix}-complete-a"),
            )
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def worktree(self) -> tuple[int, dict[str, Any]]:
        if self.phase == 0:
            return self.call(
                "CreateWorkItem",
                {
                    "objective": self.markers.get("worktree", "SCHEDULER-WORKTREE"),
                    "plan_status": "ready",
                    "todo_list": [
                        {"text": "worktree-create", "state": "pending"},
                        {"text": "worktree-cleanup", "state": "pending"},
                    ],
                },
            )
        if not self.work_ids:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        wid = self.work_ids[0]
        if self.phase == 1:
            return self.call("PickWorkItem", {"work_item_id": wid})
        if self.phase in {2, 4, 6, 8}:
            return self.call("GetWorkspaceState", {})
        if self.phase == 3:
            if not self.workspace_ids:
                return 409, {"error": {"type": "missing_workspace_id", "message": str(self.phase)}}
            return self.call(
                "CreateWorktree",
                {
                    "workspace_id": self.workspace_ids[0],
                    "base_ref": "main",
                    "branch": self.markers.get("branch", "e2e-worktree-deterministic"),
                    "activate": True,
                },
            )
        if self.phase == 5:
            return self.call(
                "SwitchWorkspace",
                {"workspace_id": self.workspace_ids[0]},
            )
        if self.phase == 7:
            if not self.execution_root_ids:
                return 409, {"error": {"type": "missing_execution_root_id", "message": str(self.phase)}}
            return self.call(
                "RemoveWorktree",
                {
                    "execution_root_id": self.execution_root_ids[-1],
                    "branch_policy": "keep",
                },
            )
        if self.phase == 9:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": wid,
                    "todo_list": [
                        {"text": "worktree-create", "state": "completed"},
                        {"text": "worktree-cleanup", "state": "completed"},
                    ],
                },
            )
        if self.phase == 10:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": wid},
                self.markers.get("worktree_complete", "SCHEDULER-WORKTREE-COMPLETE"),
            )
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def spawn(self) -> tuple[int, dict[str, Any]]:
        if self.phase == 0:
            return self.call(
                "CreateWorkItem",
                {
                    "objective": self.markers.get("spawn", "SCHEDULER-SPAWN"),
                    "plan_status": "ready",
                    "todo_list": [
                        {"text": "spawn-child", "state": "pending"},
                        {"text": "verify-child", "state": "pending"},
                    ],
                },
            )
        if not self.work_ids:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        wid = self.work_ids[0]
        if self.phase == 1:
            return self.call("PickWorkItem", {"work_item_id": wid})
        if self.phase == 2:
            marker = self.markers.get(
                "spawn_child", "SCHEDULER-SPAWN-CHILD-deterministic"
            )
            return self.call(
                "SpawnAgent",
                {
                    "preset": "private_child",
                    "initial_message": (
                        f"Respond with one sentence containing the marker {marker} "
                        "and then stop."
                    ),
                },
            )
        if self.phase == 3:
            if not self.task_ids:
                return 409, {"error": {"type": "missing_task_id", "message": str(self.phase)}}
            return self.call("TaskStatus", {"task_id": self.task_ids[-1]})
        if self.phase == 4:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": wid,
                    "todo_list": [
                        {"text": "spawn-child", "state": "completed"},
                        {"text": "verify-child", "state": "completed"},
                    ],
                },
            )
        if self.phase == 5:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": wid},
                self.markers.get("spawn_complete", "SCHEDULER-SPAWN-COMPLETE"),
            )
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def checkpoint(self) -> tuple[int, dict[str, Any]]:
        if self.phase in {0, 1}:
            key = "checkpoint_a" if self.phase == 0 else "checkpoint_b"
            suffix = "a" if self.phase == 0 else "b"
            return self.call(
                "CreateWorkItem",
                {
                    "objective": self.markers.get(key, f"SCHEDULER-REPLAY-{suffix}"),
                    "plan_status": "ready",
                    "todo_list": [
                        {
                            "text": "replay-wait" if suffix == "a" else "replay-seed",
                            "state": "pending" if suffix == "a" else "completed",
                        },
                        {"text": "replay-complete", "state": "pending"},
                    ],
                },
            )
        if self.phase == 2:
            return self.call("ListWorkItems", {"include_todo_list": True})
        if self.phase == 3:
            self.phase += 1
            return 200, response(
                [text_item("Created deterministic checkpoint WorkItems.")],
                "resp_checkpoint_created",
            )
        if len(self.work_ids) < 2:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        if self.phase == 4:
            return self.call(
                "WaitFor",
                {
                    "wake": "external",
                    "resource": self.markers.get("callback", "docker-e2e:deterministic"),
                    "reason": "deterministic checkpoint wait",
                },
            )
        if self.phase == 5:
            return self.call(
                "ListWorkItems",
                {"filter": "current", "include_todo_list": True},
            )
        if self.phase == 6:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": self.work_ids[1],
                    "todo_list": [
                        {"text": "replay-seed", "state": "completed"},
                        {"text": "replay-complete", "state": "completed"},
                    ],
                },
            )
        if self.phase == 7:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": self.work_ids[1]},
                self.markers.get(
                    "checkpoint_complete_b", "SCHEDULER-REPLAY-COMPLETE-B"
                ),
            )
        if self.phase == 8:
            self.phase += 1
            return 200, response(
                [text_item("Completed deterministic checkpoint WorkItem B.")],
                "resp_checkpoint_b_complete",
            )
        if self.phase == 9:
            return self.call(
                "GetWorkItem",
                {"work_item_id": self.work_ids[0], "include_todo_list": True},
            )
        if self.phase == 10:
            return self.call(
                "UpdateWorkItem",
                {
                    "work_item_id": self.work_ids[0],
                    "todo_list": [
                        {"text": "replay-wait", "state": "completed"},
                        {"text": "replay-complete", "state": "completed"},
                    ],
                },
            )
        if self.phase == 11:
            return self.call(
                "CompleteWorkItem",
                {"work_item_id": self.work_ids[0]},
                self.markers.get(
                    "checkpoint_complete_a", "SCHEDULER-REPLAY-COMPLETE-A"
                ),
            )
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def expected_phase(self) -> int:
        return {
            "runtime-upgrade-v030": 2,
            "scheduler-task-wait": 10,
            "scheduler-provider-retry": 3,
            "scheduler-multi": 12,
            "scheduler-external": 7,
            "scheduler-operator": 7,
            "scheduler-concurrent": 11,
            "scheduler-interject": 13,
            "scheduler-compaction": 10,
            "scheduler-worktree": 12,
            "scheduler-spawn": 7,
            "scheduler-checkpoint": 13,
        }[self.name]

    def status(self) -> dict[str, Any]:
        expected = self.expected_phase()
        return {
            "scenario": self.name,
            "phase": self.phase,
            "expected_phase": expected,
            "extra_requests": self.extra_requests,
            "complete": self.phase == expected and self.extra_requests == 0,
        }

def make_handler(scenario: Scenario, log: Path) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        def send_json(self, status: int, value: dict[str, Any]) -> None:
            body = json.dumps(value, separators=(",", ":")).encode()
            self.send_response(status); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
        def do_GET(self) -> None:
            self.send_json(200, {"status": "ok"}) if self.path == "/healthz" else self.send_json(200, scenario.status()) if self.path == "/status" else self.send_json(404, {"error": {"type": "not_found"}})
        def do_POST(self) -> None:
            if self.path != "/v1/responses":
                self.send_json(404, {"error": {"type": "not_found"}}); return
            try:
                request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
                if not isinstance(request, dict): raise ValueError
            except (ValueError, json.JSONDecodeError):
                self.send_json(400, {"error": {"type": "invalid_json"}}); return
            log.parent.mkdir(parents=True, exist_ok=True)
            with log.open("a") as stream:
                stream.write(
                    json.dumps(redact_evidence(request), separators=(",", ":")) + "\n"
                )
            self.send_json(*scenario.consume(request))
        def log_message(self, format: str, *args: Any) -> None: return
    return Handler

def main() -> None:
    parser = argparse.ArgumentParser(); parser.add_argument("--listen", default="0.0.0.0"); parser.add_argument("--port", type=int, default=8080); parser.add_argument("--scenario", required=True, choices=SCENARIOS); parser.add_argument("--request-log", type=Path, default=Path("/data/stub-requests.jsonl")); args = parser.parse_args()
    scenario = Scenario(args.scenario)
    ThreadingHTTPServer((args.listen, args.port), make_handler(scenario, args.request_log)).serve_forever()

if __name__ == "__main__":
    main()
