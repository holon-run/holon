"""Deterministic shared-credential deletion runner and stub contract tests."""
from __future__ import annotations

import copy
import importlib.util
import io
import json
import re
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock

from scripts.docker_e2e import runner, shared_credential_delete


SPEC = importlib.util.spec_from_file_location(
    "shared_delete_stub", runner.ROOT / "tests/e2e/docker/openai_stub/server.py",
)
assert SPEC is not None and SPEC.loader is not None
stub = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(stub)


def request(text: str = "", output: dict | None = None, call_id: str = "") -> dict:
    items = []
    if text:
        items.append({
            "type": "message", "content": [{
                "type": "input_text", "text": "## current_input\nCurrent input:\n"
                "- [operator][trigger:operator_prompt][operator_instruction] " + text,
            }],
        })
    if output is not None:
        items.append({
            "type": "function_call_output", "call_id": call_id,
            "output": json.dumps(output),
        })
    return {"input": items}


def operation(label: str, kind: str, data: dict) -> str:
    return (
        f"SHARED-DELETE-OP:{kind} SHARED-DELETE-DATA:{json.dumps(data)}\n"
        f"SHARED-DELETE-RESULT-{label}-abcd"
    )


class FakeHarness:
    tool_assertion_mode = "strict"
    timeout_seconds = 1
    model = "openai/gpt-5.4"
    agent_id = "parent"

    def __init__(self, evidence: Path) -> None:
        self.evidence = evidence
        self.identities: dict[str, dict] = {}
        self.deletion = None
        self.restarts = 0
        self.events_by_label: dict[str, list] = {}
        self.details: dict[str, dict] = {}
        self.current_brief = {}
        self.mutations: list[tuple[str, str, object]] = []

    def initialize_workspace(self) -> None:
        pass

    def start(self) -> None:
        pass

    def restart(self) -> None:
        self.restarts += 1

    def wait_queue_drained(self) -> None:
        pass

    def agent_path(self, suffix: str) -> str:
        return f"/api/agents/{self.agent_id}/{suffix}"

    def request(self, method: str, path: str, body=None, **kwargs):
        if method != "GET":
            self.mutations.append((method, path, body))
        if method == "PUT":
            return {"ok": True}
        if method == "PATCH":
            return {"changed": True, "results": [{"effect": "applied"}]}
        if path.endswith("/create"):
            agent_id = path.split("/")[-2]
            self.identities[agent_id] = {
                "agent_id": agent_id, "incarnation": 2, "status": "active",
            }
            return {"identity": copy.deepcopy(self.identities[agent_id])}
        if path.endswith("/detail"):
            agent_id = path.split("/")[-2]
            return {
                "identity": copy.deepcopy(self.identities[agent_id]),
                "deletion": copy.deepcopy(self.deletion) if agent_id == "target-id" else None,
            }
        if "/tasks/" in path:
            return {"status": "completed"}
        if "briefs?" in path:
            return [self.current_brief]
        raise AssertionError((method, path, body))

    def prompt(self, label: str, text: str):
        data = json.loads(re.search(r"SHARED-DELETE-DATA:(\{[^\n]*\})", text)[1])
        kind = re.search(r"SHARED-DELETE-OP:(\w+)", text)[1]
        marker = re.search(r"SHARED-DELETE-RESULT-[A-Za-z0-9-]+", text)[0]
        names = []
        values = []
        if kind == "spawn":
            role = "target" if "target" in label else "sibling"
            identity = {
                "agent_id": f"{role}-id", "incarnation": 1,
                "parent_agent_id": "parent", "status": "active",
            }
            self.identities[identity["agent_id"]] = identity
            names = ["InvokeAgent", "GetAgent"]
            values = [
                {"agent_id": identity["agent_id"], "task_handle": {"task_id": f"{role}-task"}},
                {"agent": {"identity": identity}},
            ]
        if kind == "delete":
            self.deletion = {
                "deletion_id": "one-job", "agent_id": data["agent_id"],
                "status": "completed",
                "requested_by": "agent:parent", "cascade_private_children": False,
            }
            self.identities[data["agent_id"]]["status"] = "deleted"
            names, values = ["DeleteAgent"], [{"deletion": self.deletion}]
        if kind == "stale":
            names, values = ["DeleteAgent"], [{"error": "agent_cleanup_stale_incarnation"}]
        events = []
        for index, (name, value) in enumerate(zip(names, values)):
            execution_id = f"{label}-{index}"
            events.append({
                "type": "tool_execution_failed" if kind == "stale" else "tool_executed",
                "payload": {
                    "tool_name": name, "tool_execution_id": execution_id, "turn_id": label,
                },
            })
            self.details[execution_id] = {"output": {"result": copy.deepcopy(value)}}
        self.events_by_label[label] = events
        self.current_brief = {"turn_id": label, "kind": "result", "text": marker}
        return 0, {}

    def prompt_scope(self, label: str):
        return {"message_id": label, "turn_id": label}

    def assert_tools(self, label, baseline, tools, **kwargs):
        return self.events_by_label[label]

    def events(self, label: str):
        return self.events_by_label[label.removesuffix("-tools")]

    def tool_detail(self, event, label):
        return self.details[event["payload"]["tool_execution_id"]]


class SharedCredentialDeletionTests(unittest.TestCase):
    def test_runner_complete_lifecycle_uses_no_operator_delete(self):
        with tempfile.TemporaryDirectory() as temporary:
            harness = FakeHarness(Path(temporary))
            shared_credential_delete.run_case(harness, {})
            self.assertEqual(harness.restarts, 3)
            self.assertEqual(harness.agent_id, "parent")
            self.assertEqual(harness.identities["target-id"]["incarnation"], 2)
            self.assertEqual(harness.identities["sibling-id"]["status"], "active")
            self.assertFalse(any(method == "DELETE" for method, _, _ in harness.mutations))
            self.assertEqual(len(list(Path(temporary).glob("*-result.json"))), 12)

    def test_accepted_is_not_completed_and_job_identity_is_exact(self):
        detail = {
            "identity": {"status": "deleted", "incarnation": 1},
            "deletion": {"status": "completed", "deletion_id": "job"},
        }
        shared_credential_delete.require_deleted(detail, "job", 1)
        for change in ("accepted", "wrong-job", "new-incarnation"):
            broken = copy.deepcopy(detail)
            if change == "accepted":
                broken["deletion"]["status"] = "accepted"
            elif change == "wrong-job":
                broken["deletion"]["deletion_id"] = "other"
            else:
                broken["identity"]["incarnation"] = 2
            with self.assertRaises(AssertionError):
                shared_credential_delete.require_deleted(broken, "job", 1)

    def test_runner_rejects_observe_mode(self):
        harness = Mock(tool_assertion_mode="observe")
        with self.assertRaisesRegex(AssertionError, "strict"):
            shared_credential_delete.run_case(harness, {})
        harness.initialize_workspace.assert_not_called()

    def test_runner_rejects_duplicate_successful_tools(self):
        with tempfile.TemporaryDirectory() as temporary:
            harness = FakeHarness(Path(temporary))
            original = harness.assert_tools
            harness.assert_tools = lambda *a, **kw: original(*a, **kw) * 2
            with self.assertRaisesRegex(AssertionError, "tool count/order"):
                shared_credential_delete.run_case(harness, {})

    def test_runner_rejects_new_job_from_stale_request(self):
        with tempfile.TemporaryDirectory() as temporary:
            harness = FakeHarness(Path(temporary))
            original = harness.prompt

            def prompt(label, text):
                value = original(label, text)
                if label == "shared-delete-stale":
                    harness.deletion["deletion_id"] = "unexpected-new-job"
                return value

            harness.prompt = prompt
            with self.assertRaisesRegex(AssertionError, "admitted a job"):
                shared_credential_delete.run_case(harness, {})

    def test_stub_exact_transcript_and_runtime_identity_routing(self):
        scenario = stub.Scenario("runtime-shared-credential-delete")
        for role in ("target", "sibling"):
            text = operation(f"spawn-{role}", "spawn", {
                "child_marker": f"SHARED-DELETE-CHILD-{role}-abcd",
            })
            status, value = scenario.consume(request(text))
            self.assertEqual(status, 200)
            invoke = value["output"][0]
            self.assertEqual(invoke["name"], "InvokeAgent")
            # An independent child can finish between the parent's tool rounds.
            self.assertEqual(scenario.consume(request(f"SHARED-DELETE-CHILD-{role}-abcd"))[0], 200)
            status, value = scenario.consume(request(output={
                "envelope": {"result": {"agent_id": f"actual-{role}"}},
            }, call_id=invoke["call_id"]))
            get_agent = value["output"][0]
            self.assertEqual(json.loads(get_agent["arguments"]), {
                "agent_id": f"actual-{role}",
            })
            status, value = scenario.consume(request(
                output={"agent": {"identity": {"agent_id": f"actual-{role}", "incarnation": 7}}},
                call_id=get_agent["call_id"],
            ))
            self.assertEqual(value["output"][0]["type"], "message")
        for label in ("delete", "repeat", "stale"):
            data = {"agent_id": "actual-target", "incarnation": 7}
            kind = "stale" if label == "stale" else "delete"
            status, value = scenario.consume(request(operation(label, kind, data)))
            tool = value["output"][0]
            self.assertEqual(tool["name"], "DeleteAgent")
            self.assertEqual(json.loads(tool["arguments"]), data)
            output = (
                {"error": "agent_cleanup_stale_incarnation"} if kind == "stale"
                else {"deletion": {"deletion_id": "same-job", "status": "completed"}}
            )
            self.assertEqual(scenario.consume(request(output=output, call_id=tool["call_id"]))[0], 200)
        for index in range(7):
            self.assertEqual(scenario.consume(request(operation(f"alive-{index}", "alive", {})))[0], 200)
        self.assertEqual(scenario.status()["phase"], 21)
        self.assertTrue(scenario.status()["complete"])
        self.assertEqual(scenario.status()["extra_requests"], 0)
        scenario.consume(request(operation("alive-0", "alive", {})))
        self.assertEqual(scenario.status()["extra_requests"], 1)

    def test_stub_rejects_success_instead_of_stale_error(self):
        scenario = stub.Scenario("runtime-shared-credential-delete")
        _, value = scenario.consume(request(operation("stale", "stale", {
            "agent_id": "actual-target", "incarnation": 1,
        })))
        status, _ = scenario.consume(request(
            output={"deletion": {"status": "accepted"}},
            call_id=value["output"][0]["call_id"],
        ))
        self.assertEqual(status, 409)

    def test_http_stub_rejects_environment_credential_without_network(self):
        scenario = stub.Scenario("runtime-shared-credential-delete")
        with tempfile.TemporaryDirectory() as temporary:
            handler_type = stub.make_handler(scenario, Path(temporary) / "requests.jsonl")
            handler = handler_type.__new__(handler_type)
            payload = json.dumps(request(operation("alive", "alive", {}))).encode()
            handler.path = "/v1/responses"
            handler.headers = {
                "Content-Length": str(len(payload)),
                "Authorization": "Bearer deterministic-test-key",
            }
            handler.rfile = io.BytesIO(payload)
            handler.send_json = Mock()
            handler.do_POST()
            self.assertEqual(handler.send_json.call_args.args[0], 401)
            self.assertEqual(scenario.phase, 0)
            self.assertEqual(scenario.extra_requests, 1)
            handler.headers["Authorization"] = "Bearer deterministic-shared-credential"
            handler.rfile = io.BytesIO(payload)
            handler.do_POST()
            self.assertEqual(handler.send_json.call_args.args[0], 200)
            self.assertEqual(scenario.phase, 1)

    def test_stub_does_not_use_history_as_current_operation(self):
        scenario = stub.Scenario("runtime-shared-credential-delete")
        status, _ = scenario.consume({
            "input": [{
                "type": "message", "content": [{
                    "type": "input_text",
                    "text": "## recent_turns\n" + operation("history", "alive", {}),
                }],
            }],
        })
        self.assertEqual(status, 409)
        self.assertEqual(scenario.phase, 0)
        self.assertEqual(scenario.extra_requests, 1)

    def test_case_is_registered_in_core_stub_manifest(self):
        manifest = json.loads(runner.DEFAULT_MANIFEST.read_text())
        case = next(case for case in manifest["cases"]
                    if case["id"] == "runtime-shared-credential-delete")
        self.assertEqual(case["tier"], "core")
        self.assertEqual(case["provider_mode"], "stub")
        self.assertFalse(case["requires_model"])
        self.assertIn(case["id"], runner.CASE_RUNNERS)


if __name__ == "__main__":
    unittest.main()
