"""Real provider faults for continuation/recovery acceptance; no database writes."""
from __future__ import annotations

import json
import re
import time

from server import Scenario, call_item, response, text_item

NATIVE_GATE = "qa-3772-native-gate"
NATIVE_RESULT = "QA_NATIVE_TOOL_DONE"
NATIVE_REASONING = {
    "type": "reasoning", "id": "rs_cooldown",
    "summary": [],
    "content": [{"type": "reasoning_text", "text": "QA_NATIVE_REASONING"}],
    "encrypted_content": "qa-native-opaque",
}


def consume(handler, payload, side, setting, record):
    """Handle only explicitly selected scenarios; return False for other suites."""
    scenario = setting("SCENARIO", "")
    if scenario not in ("native-cooldown", "recovery-cooldown", "park-before-io"):
        return False
    body = json.dumps(payload)
    items = payload.get("input", [])
    outputs = [item for item in items if isinstance(item, dict)
               and item.get("type") == "function_call_output"]
    current = Scenario.current_input(payload) or body
    recovery = [item for item in Scenario.current_input_items(payload)
                if item["provenance"].get("origin") == {
                    "kind": "system", "subsystem": "model_lineage_recovery",
                }]
    row = {
        "layer": "mock_http", "side": side, "scenario": scenario,
        "model": payload.get("model"), "tool_output": bool(outputs),
        "tool_outputs": outputs,
        "native_context": [item for item in items if isinstance(item, dict)
                           and item.get("type") == "reasoning"],
        "previous_response_id": payload.get("previous_response_id"),
        "recovery_input": [item["provenance"].get("id") for item in recovery],
        "has_tool_evidence": NATIVE_RESULT in body,
        "tool_evidence_refs": sorted(set(re.findall(
            r"tool_execution_id=([A-Za-z0-9_-]+)", body))),
    }

    def reply(status, marker, output=None, headers=None):
        record(**row, marker=marker, status=status, response=marker,
               retry_after=(headers or {}).get("Retry-After"))
        handler.reply(status, output if output is not None else response(
            [text_item(marker)], f"resp_{side}_{time.monotonic_ns()}"), headers)

    def fail(marker, seconds):
        reply(503, marker, {"error": {
            "message": "fixture-only diagnostic must not reach cooldown summaries",
            "type": "fixture",
        }}, {"Retry-After": str(seconds)})

    if "QA_COOL_NATIVE" in current and side == "a":
        fail("cooling", 12)
    elif "QA_COOL_RECOVERY" in current:
        fail("cooling", 12)
    elif (scenario == "recovery-cooldown" and side == "b" and recovery
          and not row["has_tool_evidence"] and setting("HOLD_SOURCE_FAILURE", False)):
        # The helper reaches B through a runtime repair input, not its operator marker.
        fail("cooling", 12)
    elif recovery:
        reply(200, "QA_RECOVERY_DONE")
    elif "QA_NATIVE" in current and not outputs:
        reply(200, "native_tool_call", response([
            call_item("call-native", "ExecCommand", {
                "cmd": f"while [ ! -f /var/lib/holon/{NATIVE_GATE} ]; "
                       f"do sleep 0.1; done; printf {NATIVE_RESULT}",
                "yield_time_ms": 20000,
            }),
            NATIVE_REASONING,
        ], f"resp_{side}_native"))
    elif outputs and scenario == "recovery-cooldown":
        record(**row, response="source_failure_held")
        deadline = time.monotonic() + 60
        while setting("HOLD_SOURCE_FAILURE", False):
            if time.monotonic() >= deadline:
                reply(504, "source_failure_gate_timeout")
                return True
            time.sleep(0.05)
        fail("source_failure", 12)
    elif outputs:
        reply(200, "QA_NATIVE_DONE")
    else:
        reply(200, "QA_HELPER_DONE")
    return True
