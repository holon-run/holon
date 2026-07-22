#!/usr/bin/env python3

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RUNNER_PATH = ROOT / "scripts/docker_e2e/runner.py"
SPEC = importlib.util.spec_from_file_location("docker_e2e_runner", RUNNER_PATH)
assert SPEC is not None and SPEC.loader is not None
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class DockerE2ERunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = json.loads(runner.DEFAULT_MANIFEST.read_text())
        runner.validate_manifest(self.manifest)

    def test_core_suite_selection(self) -> None:
        selected = runner.select_cases(
            self.manifest, requested=None, suite="core", tags=[]
        )
        self.assertEqual(
            [case["id"] for case in selected],
            [
                "runtime-auth-model-delivery",
                "memory-agent-home-persistence",
                "workspace-restart-lifecycle",
                "workitem-wait-restart-complete",
            ],
        )

    def test_tag_selection_requires_all_tags(self) -> None:
        selected = runner.select_cases(
            self.manifest,
            requested=None,
            suite="core",
            tags=["restart", "delivery"],
        )
        self.assertEqual(
            [case["id"] for case in selected],
            ["workitem-wait-restart-complete"],
        )

    def test_manifest_rejects_unregistered_case(self) -> None:
        invalid = json.loads(json.dumps(self.manifest))
        invalid["cases"][0]["id"] = "not-implemented"
        with self.assertRaisesRegex(AssertionError, "no registered runner"):
            runner.validate_manifest(invalid)

    def test_secret_scan_reports_value_without_echoing_it(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            secret = "secret-value-for-test"
            (root / "evidence.txt").write_text(f"prefix {secret} suffix")
            result = runner.secret_scan(root, [secret])
            self.assertEqual(result["status"], "fail")
            serialized = (root / "secret-scan.json").read_text()
            self.assertNotIn(secret, serialized)

    def test_secret_scan_ignores_bearer_placeholder(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "evidence.txt").write_text(
                "retry with an Authorization: Bearer <token> header"
            )
            result = runner.secret_scan(root, [])
            self.assertEqual(result["status"], "pass")

    def test_secret_scan_reports_real_bearer_value(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "evidence.txt").write_text(
                '{"authorization":"Bearer actual-secret-token"}'
            )
            result = runner.secret_scan(root, [])
            self.assertEqual(result["status"], "fail")

    def test_secret_scan_reports_unredacted_callback_per_url(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "evidence.txt").write_text(
                "/api/callbacks/wake/<redacted>\n"
                "/api/callbacks/wake/cb_actual_secret\n"
            )
            result = runner.secret_scan(root, [])
            self.assertEqual(result["status"], "fail")
            self.assertEqual(
                result["findings"],
                [{"path": "evidence.txt", "kind": "callback-capability"}],
            )

    def test_secret_scan_quarantines_files_with_findings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            secret = "secret-value-for-test"
            path = root / "evidence.txt"
            path.write_text(f"prefix {secret} suffix")
            result = runner.secret_scan(root, [secret])
            self.assertEqual(result["status"], "fail")
            self.assertNotIn(secret, path.read_text())
            self.assertIn("quarantined", path.read_text())

    def test_memory_value_unwraps_memory_get_envelope(self) -> None:
        memory = {"source_ref": "agent_memory:self", "content": "marker"}
        self.assertEqual(runner.memory_value({"memory": memory}), memory)
        self.assertEqual(runner.memory_value(memory), memory)

    def test_evidence_redacts_callback_capability(self) -> None:
        value = {
            "url": "http://localhost/api/callbacks/wake/cb_secret-capability"
        }
        redacted = runner.redact_evidence(value)
        self.assertEqual(
            redacted["url"],
            "http://localhost/api/callbacks/wake/<redacted>",
        )

    def test_cleanup_fails_when_resource_still_exists(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="cleanup-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )

            def fake_docker(*args: str, **_: object) -> subprocess.CompletedProcess[str]:
                if args[:2] == ("volume", "inspect"):
                    return subprocess.CompletedProcess(["docker", *args], 0, "", "")
                return subprocess.CompletedProcess(["docker", *args], 1, "", "")

            harness.docker = fake_docker
            result = harness.cleanup()
            self.assertEqual(result["status"], "fail")
            self.assertIn("volume still exists", result["errors"][0])

    def test_tool_assertion_reports_runtime_failure_before_missing_tools(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="runtime-failure-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            harness.events = lambda _: [
                {
                    "type": "turn_started",
                    "payload": {"turn_id": "turn-failed", "turn_index": 4},
                },
                {
                    "type": "runtime_error",
                    "payload": {
                        "turn_id": "turn-failed",
                        "domain": "provider",
                        "error": "provider request failed",
                        "source_chain": ["connection closed"],
                    },
                },
            ]

            with self.assertRaisesRegex(
                AssertionError,
                "runtime failure occurred in complete: provider: connection closed",
            ):
                harness.assert_tools("complete", 3, ["CompleteWorkItem"])

    def test_scheduler_extended_cases_declare_explicit_feature_flag(self) -> None:
        selected = runner.select_cases(
            self.manifest, requested=None, suite="extended", tags=["scheduler"]
        )
        self.assertEqual(
            [case["id"] for case in selected],
            [
                "scheduler-autonomous-legacy",
                "scheduler-autonomous-authoritative",
            ],
        )
        self.assertEqual(
            [
                (
                    case["scheduler_protocol_commands_enabled"],
                    case["runtime_env"][
                        "HOLON_SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS"
                    ],
                )
                for case in selected
            ],
            [(False, "false"), (True, "true")],
        )
        for case in selected:
            phase = case["phases"][0]
            self.assertEqual(
                phase["required_tools"],
                [
                    "CreateWorkItem",
                    "ListWorkItems",
                    "UpdateWorkItem",
                    "CompleteWorkItem",
                ],
            )
            self.assertNotIn("GetWorkItem", phase["required_tools"])
            self.assertNotIn("PickWorkItem", phase["forbidden_tools"])

    def test_scheduler_queue_oracle_uses_current_processed_state(self) -> None:
        runner.require_processed_queue_entries(
            [
                {"message_id": "other", "status": "queued"},
                {"message_id": "scheduler-tick", "status": "processed"},
            ],
            {"scheduler-tick"},
        )
        with self.assertRaisesRegex(
            AssertionError, "did not reach processed current state"
        ):
            runner.require_processed_queue_entries(
                [{"message_id": "scheduler-tick", "status": "dequeued"}],
                {"scheduler-tick"},
            )


if __name__ == "__main__":
    unittest.main()
