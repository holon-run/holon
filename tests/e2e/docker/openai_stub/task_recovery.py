"""Provider-only barriers for real task-result recovery; never mutates runtime state."""
from __future__ import annotations

import json
import secrets
import time
from pathlib import Path

from server import Scenario


CONTROL = Path("/tmp/qa-recovery.json")
COMMAND_GATE = "/var/lib/holon/recovery-command-release"


class RecoveryScenario(Scenario):
    def __init__(self):
        super().__init__("scheduler-task-wait")

    def task_wait(self):
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

    def consume_gated(self, request, side, record, *, timeout=20):
        # Bootstrap and the real CreateWorkItem/ExecCommand/WaitFor setup stay healthy.
        items = self.current_input_items(request)
        bootstrap = items and all(
            item["body"].startswith((
                "This is the first run of Holon",
                "This agent was just created without an initial message",
            ))
            for item in items
        )
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
                    return 400, {"error": {
                        "type": "invalid_request_error", "message": "deterministic recovery failure",
                    }}
                if release != "success":
                    raise ValueError("unknown recovery release action")
                return self.consume(request)
            time.sleep(.05)
        record(layer="recovery_barrier", request_id=request_id, side=side, state="expired")
        return 504, {"error": {"type": "fixture_barrier_timeout"}}

    def expected_phase(self):
        return 6
