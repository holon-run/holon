"""Provider-only barriers for real task-result recovery; never mutates runtime state."""
from __future__ import annotations

import json
import secrets
import time
from pathlib import Path

from server import Scenario, response, text_item


CONTROL = Path("/tmp/qa-recovery.json")
COMMAND_GATE = "/var/lib/holon/recovery-command-release"
FAIRNESS_GATE = "/var/lib/holon/recovery-fairness-command-release"


class RecoveryScenario(Scenario):
    def __init__(self):
        super().__init__("scheduler-task-wait")
        self.fairness = False
        self.scanner_first = False

    def task_wait(self):
        if self.phase == 3 and self.scanner_first:
            # A wait triggered by the held original reserves it against scanner.
            self.phase = 4
            return 200, response(
                [text_item("SCHEDULER-TASK-SCANNER-READY")], "resp_scanner_ready",
            )
        if self.fairness:
            if self.phase == 6:
                return self.call("ExecCommand", {
                    "cmd": f"while [ ! -f {FAIRNESS_GATE} ]; do sleep 0.1; done; "
                           "printf SCHEDULER-TASK-FAIRNESS-RESULT",
                    "yield_time_ms": 50, "max_output_tokens": 100,
                })
            if self.phase == 7:
                if len(self.task_ids) < 2 or self.task_ids[-1] == self.task_ids[0]:
                    return 409, {"error": {"type": "missing_independent_fairness_task"}}
                return self.call("WaitFor", {
                    "wake": "task_result", "resource": self.task_ids[-1],
                    "reason": "consume the new task without rearming the exhausted result",
                    "delivery": "silent",
                })
            if self.phase == 8:
                return self.call("WaitFor", {
                    "wake": "operator_input", "delivery": "final",
                    "reason": "preserve the shared owner and its exhausted obligation",
                })
            if self.phase == 9:
                self.phase += 1
                return 200, response(
                    [text_item("SCHEDULER-TASK-FAIRNESS-COMPLETE")], "resp_task_fairness",
                )
        if self.phase == 2:
            marker = self.markers.get("task_result", "SCHEDULER-TASK-RESULT")
            return self.call("ExecCommand", {
                "cmd": f"while [ ! -f {COMMAND_GATE} ]; do sleep 0.1; done; printf {marker}",
                "yield_time_ms": 50, "max_output_tokens": 100,
            })
        if self.phase == 4:
            return self.call("UpdateWorkItem", {
                "work_item_id": self.work_ids[0],
                "todo_list": [
                    {"text": "task-continuation", "state": "completed"},
                    {"text": "external-continuation", "state": "completed"},
                ],
            })
        if self.phase == 5:
            return self.call(
                "CompleteWorkItem", {"work_item_id": self.work_ids[0]},
                self.markers.get("task_complete", "SCHEDULER-TASK-WAIT-COMPLETE"),
            )
        return super().task_wait()

    def consume_gated(self, request, side, record, *, timeout=75):
        # Bootstrap and the real CreateWorkItem/ExecCommand/WaitFor setup stay healthy.
        items = self.current_input_items(request)
        control = json.loads(CONTROL.read_text()) if CONTROL.exists() else {}
        if (self.phase == 4 and not self.fairness and control.get("fairness_owner")
                and any("SCHEDULER-TASK-FAIRNESS" in item["body"]
                        and control["fairness_owner"] in item["body"] for item in items)):
            self.fairness, self.phase = True, 6
        bootstrap = items and all(
            item["body"].startswith((
                "This is the first run of Holon",
                "This agent was just created without an initial message",
            ))
            for item in items
        )
        if self.phase == 3:
            self.scanner_first = bool(control.get("scanner_first"))
        if self.phase != 4 or bootstrap:
            return self.consume(request)
        request_id = secrets.token_hex(12)
        record(layer="recovery_barrier", request_id=request_id, side=side, state="waiting")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            control = json.loads(CONTROL.read_text()) if CONTROL.exists() else {}
            release = control.get("releases", {}).get(request_id)
            if release:
                record(layer="recovery_barrier", request_id=request_id,
                       side=side, state="released", action=release)
                if release == "fail":
                    return 503, {"error": {
                        "type": "service_unavailable", "message": "deterministic recovery failure",
                    }}
                if release != "success":
                    raise ValueError("unknown recovery release action")
                return self.consume(request)
            time.sleep(.05)
        record(layer="recovery_barrier", request_id=request_id, side=side, state="expired")
        return 504, {"error": {"type": "fixture_barrier_timeout"}}

    def expected_phase(self):
        return 10 if self.fairness else 6
