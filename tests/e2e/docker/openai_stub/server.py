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
SCENARIOS = ("scheduler-multi", "scheduler-external", "scheduler-operator")

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
        self.first_turn_closed = False
        self.current_input_text = ""
        self.extra_requests = 0
        self.lock = threading.Lock()

    def observe(self, request: dict[str, Any]) -> str:
        raw = json.dumps(request, ensure_ascii=False)
        for wid in re.findall(r"work_[0-9a-f]{15}", raw):
            if wid not in self.work_ids:
                self.work_ids.append(wid)
        for key, pattern in {
            "multi_a": r"SCHEDULER-MULTI-A-[0-9a-f]+", "multi_b": r"SCHEDULER-MULTI-B-[0-9a-f]+",
            "multi_complete_a": r"SCHEDULER-MULTI-COMPLETE-A-[0-9a-f]+", "multi_complete_b": r"SCHEDULER-MULTI-COMPLETE-B-[0-9a-f]+",
            "external": r"SCHEDULER-EXTERNAL-WAIT-[0-9a-f]+", "external_complete": r"SCHEDULER-EXTERNAL-COMPLETE-[0-9a-f]+",
            "operator": r"SCHEDULER-OPERATOR-WAIT-[0-9a-f]+", "operator_complete": r"SCHEDULER-OPERATOR-COMPLETE-[0-9a-f]+",
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
                "[trigger:internal_followup][runtime_instruction][InternalFollowup]"
                in current_input
                and "This is the first run of Holon" in current_input
            ):
                return 200, response([text_item("Deterministic Holon test runtime ready.")], "resp_intro")
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
            if self.name in {"scheduler-external", "scheduler-operator"}:
                return self.wait(self.name.removeprefix("scheduler-"))
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

    def wait(self, kind: str) -> tuple[int, dict[str, Any]]:
        if self.phase == 0:
            return self.call("CreateWorkItem", {"objective": self.markers.get(kind, f"SCHEDULER-{kind.upper()}-WAIT"), "plan_status": "ready", "todo_list": [{"text": f"{kind}-wait", "state": "pending"}, {"text": f"{kind}-complete", "state": "pending"}]})
        if not self.work_ids:
            return 409, {"error": {"type": "missing_work_item_id", "message": str(self.phase)}}
        wid = self.work_ids[0]
        if self.phase == 1:
            return self.call("PickWorkItem", {"work_item_id": wid})
        if self.phase == 2:
            args: dict[str, Any] = {"wake": "external" if kind == "external" else "operator_input", "reason": f"deterministic {kind} wait"}
            if kind == "external":
                args["resource"] = self.markers.get("callback", "docker-e2e:deterministic")
            return self.call("WaitFor", args)
        if self.phase == 3:
            return self.call("GetWorkItem", {"work_item_id": wid, "include_todo_list": True})
        if self.phase == 4:
            return self.call("UpdateWorkItem", {"work_item_id": wid, "todo_list": [{"text": f"{kind}-wait", "state": "completed"}, {"text": f"{kind}-complete", "state": "completed"}]})
        if self.phase == 5:
            completion = self.markers.get(f"{kind}_complete") or self.markers[
                kind
            ].replace(
                f"SCHEDULER-{kind.upper()}-WAIT-",
                f"SCHEDULER-{kind.upper()}-COMPLETE-",
            )
            return self.call("CompleteWorkItem", {"work_item_id": wid}, completion)
        return 409, {"error": {"type": "transcript_exhausted", "message": str(self.phase)}}

    def expected_phase(self) -> int:
        return {
            "scheduler-multi": 12,
            "scheduler-external": 7,
            "scheduler-operator": 7,
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
