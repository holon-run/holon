#!/usr/bin/env python3

import importlib.util
import io
import inspect
import json
import copy
import os
import subprocess
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
RUNNER_PATH = ROOT / "scripts/docker_e2e/runner.py"
SPEC = importlib.util.spec_from_file_location("docker_e2e_runner", RUNNER_PATH)
assert SPEC is not None and SPEC.loader is not None
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)
STUB_PATH = ROOT / "tests/e2e/docker/openai_stub/server.py"
STUB_SPEC = importlib.util.spec_from_file_location("openai_stub", STUB_PATH)
assert STUB_SPEC is not None and STUB_SPEC.loader is not None
stub = importlib.util.module_from_spec(STUB_SPEC)
STUB_SPEC.loader.exec_module(stub)


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

    def test_provider_base_url_env_matches_builtin_provider_contract(self) -> None:
        self.assertEqual(
            runner.provider_base_url_env("deepseek/deepseek-v4-flash"),
            "HOLON_DEEPSEEK_BASE_URL",
        )
        self.assertEqual(
            runner.provider_base_url_env("volcengine@plan/glm-5.2"),
            "HOLON_VOLCENGINE_AGENT_BASE_URL",
        )
        self.assertEqual(
            runner.provider_base_url_env("anthropic/claude-sonnet-4-6"),
            "ANTHROPIC_BASE_URL",
        )

    def test_runtime_config_merges_model_override_without_mutating_source(self) -> None:
        source = {"providers": {"custom": {"base_url": "https://example.invalid"}}}
        merged = runner.merged_runtime_config(
            source,
            "custom/model",
            {"max_output_tokens": 4096},
        )

        self.assertEqual(
            merged["models"]["catalog"]["custom/model"]["max_output_tokens"],
            4096,
        )
        self.assertNotIn("models", source)

    def test_load_runtime_config_requires_json_object(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "config.json"
            path.write_text("[]")
            with self.assertRaisesRegex(AssertionError, "JSON object"):
                runner.load_runtime_config(path)

    def test_provider_file_paths_accept_new_names_before_legacy_aliases(self) -> None:
        with patch.dict(
            os.environ,
            {
                "HOLON_E2E_PROVIDER_ENV_FILE": "provider.env",
                "HOLON_E2E_DOCKER_ENV_FILE": "legacy.env",
                "HOLON_E2E_PROVIDER_CONFIG_FILE": "provider.json",
                "HOLON_E2E_CONFIG_FILE": "legacy.json",
            },
            clear=True,
        ):
            env_file, config_file = runner.provider_file_paths(None, None)

        self.assertEqual(env_file, Path("provider.env").resolve())
        self.assertEqual(config_file, Path("provider.json").resolve())

    def test_provider_file_paths_keep_legacy_aliases(self) -> None:
        with patch.dict(
            os.environ,
            {
                "HOLON_E2E_DOCKER_ENV_FILE": "legacy.env",
                "HOLON_E2E_CONFIG_FILE": "legacy.json",
            },
            clear=True,
        ):
            env_file, config_file = runner.provider_file_paths(None, None)

        self.assertEqual(env_file, Path("legacy.env").resolve())
        self.assertEqual(config_file, Path("legacy.json").resolve())

    def test_work_queue_message_evidence_extracts_retry_identity(self) -> None:
        snapshot = {
            "messages": [
                {
                    "message_id": "message-1",
                    "work_item_id": "work-1",
                    "payload_json": json.dumps(
                        {
                            "metadata": {
                                "work_queue": {
                                    "reason": "continue_active",
                                    "idempotency_key": "work_queue:continue_active:work-1:1",
                                }
                            }
                        }
                    ),
                },
                {
                    "message_id": "message-other",
                    "work_item_id": "work-1",
                    "payload_json": json.dumps(
                        {
                            "metadata": {
                                "work_queue": {
                                    "reason": "queued_available",
                                    "idempotency_key": "other",
                                }
                            }
                        }
                    ),
                },
            ],
            "queue_entries": [
                {"message_id": "message-1", "status": "aborted"},
                {"message_id": "message-other", "status": "processed"},
            ],
        }

        self.assertEqual(
            runner.work_queue_message_evidence(
                snapshot,
                work_item_id="work-1",
                reason="continue_active",
            ),
            [
                {
                    "message_id": "message-1",
                    "idempotency_key": "work_queue:continue_active:work-1:1",
                    "status": "aborted",
                }
            ],
        )

    def test_recovered_retry_ticks_accepts_existing_interrupted_tick(self) -> None:
        key = "work_queue:continue_active:work-1:1"
        failed = [
            {
                "message_id": "message-1",
                "idempotency_key": key,
                "status": "aborted",
            },
            {
                "message_id": "message-2",
                "idempotency_key": key,
                "status": "interrupted",
            },
        ]
        recovered = [
            failed[0],
            {
                "message_id": "message-2",
                "idempotency_key": key,
                "status": "processed",
            },
        ]

        self.assertEqual(
            runner.recovered_retry_ticks(failed, recovered),
            [recovered[1]],
        )

    def test_recovered_retry_ticks_rejects_changed_idempotency(self) -> None:
        failed = [
            {
                "message_id": "message-1",
                "idempotency_key": "work_queue:continue_active:work-1:1",
                "status": "aborted",
            }
        ]
        recovered = [
            {
                "message_id": "message-2",
                "idempotency_key": "work_queue:continue_active:work-1:2",
                "status": "processed",
            }
        ]

        self.assertEqual(runner.recovered_retry_ticks(failed, recovered), [])

    def test_manifest_rejects_unregistered_case(self) -> None:
        invalid = json.loads(json.dumps(self.manifest))
        invalid["cases"][0]["id"] = "not-implemented"
        with self.assertRaisesRegex(AssertionError, "no registered runner"):
            runner.validate_manifest(invalid)

    def test_scheduler_live_canary_profile_is_observational(self) -> None:
        profile = runner.resolve_profile(self.manifest, "scheduler-live-canary")
        self.assertEqual(profile["gate_kind"], "live_canary")
        self.assertEqual(profile["provider_mode"], "live")
        self.assertEqual(profile["tool_assertion_mode"], "observe")
        self.assertEqual(
            profile["case_ids"],
            ["scheduler-external-wait-resume"],
        )

    def test_workflows_keep_required_gate_independent_and_publish_canary(self) -> None:
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        nightly = (ROOT / ".github/workflows/e2e-scheduler-nightly.yml").read_text()
        release = (ROOT / ".github/workflows/release-e2e.yml").read_text()

        self.assertLess(
            nightly.index("Run deterministic scheduler matrix"),
            nightly.index("Prepare provider configuration"),
        )
        self.assertIn("if: steps.provider-env.outcome == 'success'", nightly)
        self.assertIn("continue-on-error: true", nightly)
        for workflow in (ci, nightly, release):
            self.assertIn("scheduler-live-canary-report.json", workflow)
            self.assertIn("behavioral_variances", workflow)
            self.assertIn("tool_counts", workflow)
            self.assertIn("HOLON_E2E_SCHEDULER_CREDENTIAL", workflow)
            self.assertIn("HOLON_E2E_SCHEDULER_CONFIG_JSON", workflow)
            self.assertIn("--config-file", workflow)
        self.assertIn("name: scheduler-live-canary", ci)
        self.assertIn("name: scheduler-e2e-nightly", nightly)
        self.assertIn("Detect scheduler code changes", nightly)
        self.assertIn("needs.changes.outputs.should-run == 'true'", nightly)
        self.assertIn("Unable to detect scheduler code changes", nightly)
        self.assertIn("comparison.data.truncation === true", nightly)
        self.assertIn("files.length >= 300", nightly)
        self.assertIn("HOLON_E2E_PROVIDER_CONFIG_FILE:", nightly)
        self.assertNotIn("HOLON_E2E_CONFIG_FILE:", nightly)

    def test_scheduler_required_profile_selects_all_stub_cases(self) -> None:
        profile = runner.resolve_profile(self.manifest, "scheduler-required")
        selected = runner.select_cases(
            self.manifest,
            requested=profile["case_ids"],
            suite="core",
            tags=[],
        )
        expected = [
            case["id"]
            for case in self.manifest["cases"]
            if "scheduler" in case.get("tags", [])
        ]
        self.assertEqual(profile["gate_kind"], "required")
        self.assertEqual(profile["provider_mode"], "stub")
        self.assertEqual(profile["tool_assertion_mode"], "strict")
        self.assertEqual([case["id"] for case in selected], expected)
        self.assertEqual(profile["required_coverage_ids"], expected)
        self.assertTrue(all(case.get("stub_scenario") for case in selected))
        self.assertTrue(
            all(case["timeout_seconds"] <= 120 for case in selected)
        )

    def test_manifest_rejects_live_canary_with_strict_tools(self) -> None:
        invalid = json.loads(json.dumps(self.manifest))
        invalid["profiles"]["scheduler-live-canary"][
            "tool_assertion_mode"
        ] = "strict"
        with self.assertRaisesRegex(
            AssertionError,
            "live canary must observe tool assertions",
        ):
            runner.validate_manifest(invalid)

    def test_manifest_rejects_required_case_without_stub_scenario(self) -> None:
        invalid = json.loads(json.dumps(self.manifest))
        invalid["cases"][-1].pop("stub_scenario")
        with self.assertRaisesRegex(
            AssertionError,
            "cases require stub_scenario",
        ):
            runner.validate_manifest(invalid)

    def test_scheduler_live_canary_report_records_model_and_retries(self) -> None:
        report = runner.scheduler_live_canary_report(
            run_record={
                "git_sha": "git-sha",
                "image_digest": "image@sha256:digest",
                "manifest_sha256": "manifest-hash",
                "profile": "scheduler-live-canary",
                "provider_mode": "live",
                "model_route": "provider/model",
                "tool_assertion_mode": "observe",
            },
            case_results=[
                {
                    "id": "scheduler-external-wait-resume-legacy",
                    "base_id": "scheduler-external-wait-resume",
                    "scheduler_engine": "legacy",
                    "status": "pass",
                    "error": "",
                    "provider_rounds": 2,
                    "provider_attempts": 3,
                    "provider_retries": 1,
                    "tool_counts": {"WaitFor": 1},
                    "behavioral_variances": [
                        {
                            "scope": "scheduler-external-resume",
                            "missing_tools": ["GetWorkItem"],
                            "forbidden_tools_used": [],
                        }
                    ],
                },
                {
                    "id": "scheduler-external-wait-resume-canonical",
                    "base_id": "scheduler-external-wait-resume",
                    "scheduler_engine": "canonical",
                    "status": "pass",
                    "error": "",
                    "provider_rounds": 2,
                    "provider_attempts": 2,
                    "provider_retries": 0,
                    "tool_counts": {"WaitFor": 1},
                    "behavioral_variances": [],
                },
            ],
            scheduler_acceptance_status="pass",
            scheduler_coverage_status="pass",
            secret_scan_status="pass",
        )
        self.assertEqual(report["status"], "pass")
        self.assertEqual(report["model_route"], "provider/model")
        self.assertEqual(report["provider_rounds"], 4)
        self.assertEqual(report["provider_attempts"], 5)
        self.assertEqual(report["provider_retries"], 1)
        self.assertEqual(
            report["behavioral_variances"],
            [
                {
                    "case_id": "scheduler-external-wait-resume-legacy",
                    "scope": "scheduler-external-resume",
                    "missing_tools": ["GetWorkItem"],
                    "forbidden_tools_used": [],
                }
            ],
        )

    def test_collect_case_metrics_deduplicates_overlapping_event_snapshots(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            events = [
                {
                    "event_seq": 10,
                    "type": "tool_executed",
                    "payload": {"tool_name": "WaitFor"},
                },
                {
                    "event_seq": 11,
                    "type": "provider_round_completed",
                    "payload": {
                        "provider_attempt_timeline": {
                            "attempts": [{"status": "failed"}, {"status": "success"}]
                        },
                        "token_usage": {
                            "input_tokens": 3,
                            "output_tokens": 2,
                            "total_tokens": 5,
                        },
                    },
                },
            ]
            (evidence / "first-events.json").write_text(
                json.dumps({"events": events})
            )
            (evidence / "second-events.json").write_text(
                json.dumps({"events": events})
            )

            metrics = runner.collect_case_metrics(evidence)

        self.assertEqual(metrics["tool_counts"], {"WaitFor": 1})
        self.assertEqual(metrics["provider_rounds"], 1)
        self.assertEqual(metrics["provider_attempts"], 2)
        self.assertEqual(metrics["provider_retries"], 1)
        self.assertEqual(metrics["token_usage"]["total_tokens"], 5)

    def test_scheduler_coverage_reports_missing_and_duplicate_ids(self) -> None:
        report = runner.scheduler_coverage_report(
            run_record={
                "git_sha": "git-sha",
                "image_digest": "image@sha256:digest",
                "manifest_sha256": "manifest-hash",
                "profile": "scheduler-required",
            },
            case_results=[
                {"id": "a-legacy", "coverage_ids": ["a"], "status": "pass"},
                {"id": "a-canonical", "coverage_ids": ["a"], "status": "pass"},
                {"id": "a-extra", "coverage_ids": ["a"], "status": "pass"},
            ],
            required_coverage_ids={"a", "b"},
            secret_scan_status="pass",
        )
        self.assertEqual(report["status"], "fail")
        self.assertEqual(report["missing_coverage_ids"], ["b"])
        self.assertEqual(
            report["duplicate_or_incomplete_coverage_ids"]["a"],
            ["a-legacy", "a-canonical", "a-extra"],
        )

    def test_openai_stub_consumes_transcript_deterministically(self) -> None:
        scenario = stub.Scenario("scheduler-external")
        status, response = scenario.consume(
            {
                "instructions": "Reply with exactly OK.",
                "input": [
                    {
                        "type": "message",
                        "content": [
                            {"type": "input_text", "text": "Reply with exactly OK."}
                        ],
                    }
                ],
            }
        )
        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["content"][0]["text"], "OK")
        self.assertEqual(scenario.phase, 0)
        status, response = scenario.consume(
            {
                "instructions": "normal agent instructions mentioning Reply with exactly OK.",
                "input": [
                    {
                        "type": "message",
                        "content": [
                            {
                                "type": "input_text",
                                "text": (
                                    "## recent_turns\nThis is the first run of Holon\n"
                                    "## current_input\nCurrent input:\n"
                                    "- [operator] SCHEDULER-EXTERNAL-WAIT-abcd "
                                    "SCHEDULER-EXTERNAL-COMPLETE-abcd docker-e2e:abcd"
                                ),
                            }
                        ],
                    }
                ],
            }
        )
        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["name"], "CreateWorkItem")
        self.assertEqual(scenario.phase, 1)
        scenario.phase = 0
        status, response = scenario.consume(
            {
                "input": [
                    {
                        "type": "message",
                        "content": [
                            {
                                "type": "input_text",
                                "text": (
                                    "## current_input\nCurrent input:\n"
                                    "- [system][runtime_system][runtime_owned]"
                                    "[trigger:internal_followup][runtime_instruction]"
                                    "[InternalFollowup]\n"
                                    "  This is the first run of Holon"
                                ),
                            }
                        ],
                    }
                ],
            }
        )
        self.assertEqual(status, 200)
        self.assertEqual(
            response["output"][0]["content"][0]["text"],
            "Deterministic Holon test runtime ready.",
        )
        self.assertEqual(scenario.phase, 0)
        status, response = scenario.consume(
            {"input": [{"type": "message", "content": [{"type": "input_text", "text": "SCHEDULER-EXTERNAL-WAIT-abcd SCHEDULER-EXTERNAL-COMPLETE-abcd docker-e2e:abcd"}]}]}
        )
        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["name"], "CreateWorkItem")
        status, response = scenario.consume({"input": [{"type": "function_call_output", "output": "{\"work_item\":{\"id\":\"work_0123456789abcde\"}}"}]})
        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["name"], "PickWorkItem")
        scenario.phase = 6
        status, response = scenario.consume({"input": []})
        self.assertEqual(status, 200)
        self.assertEqual(
            response["output"][0]["content"][0]["text"],
            "Deterministic scheduler scenario complete.",
        )
        self.assertEqual(scenario.phase, 7)
        self.assertTrue(scenario.status()["complete"])
        status, response = scenario.consume({"input": []})
        self.assertEqual(status, 409)
        self.assertEqual(response["error"]["type"], "transcript_exhausted")
        self.assertEqual(scenario.status()["extra_requests"], 1)
        self.assertFalse(scenario.status()["complete"])

    def test_openai_stub_multi_waits_for_second_autonomous_turn(self) -> None:
        scenario = stub.Scenario("scheduler-multi")
        scenario.phase = 7
        scenario.work_ids = [
            "work_0123456789abcde",
            "work_fedcba987654321",
        ]
        scenario.markers = {
            "multi_a": "SCHEDULER-MULTI-A-abcd",
            "multi_b": "SCHEDULER-MULTI-B-abcd",
        }
        status, response = scenario.consume({"input": []})
        self.assertEqual(status, 200)
        self.assertEqual(
            response["output"][0]["content"][0]["text"],
            "Completed the first deterministic WorkItem.",
        )
        self.assertEqual(scenario.phase, 7)
        status, response = scenario.consume({"input": []})
        self.assertEqual(status, 409)
        self.assertEqual(response["error"]["type"], "transcript_exhausted")
        status, response = scenario.consume(
            {
                "input": [
                    {
                        "type": "message",
                        "content": [
                            {
                                "type": "input_text",
                                "text": (
                                    "## current_input\nCurrent input:\n"
                                    "- [system][trigger:system_tick] "
                                    "SCHEDULER-MULTI-B-abcd"
                                ),
                            }
                        ],
                    }
                ],
            }
        )
        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["name"], "GetWorkspaceState")
        self.assertEqual(scenario.phase, 8)

    def test_openai_stub_operator_wait_uses_operator_input_wake(self) -> None:
        scenario = stub.Scenario("scheduler-operator")
        scenario.phase = 2
        scenario.work_ids = ["work_0123456789abcde"]

        status, response = scenario.consume({"input": []})

        self.assertEqual(status, 200)
        call = response["output"][0]
        self.assertEqual(call["name"], "WaitFor")
        self.assertEqual(json.loads(call["arguments"])["wake"], "operator_input")

    def test_openai_stub_task_wait_closes_creation_turn(self) -> None:
        scenario = stub.Scenario("scheduler-task-wait")
        scenario.phase = 1
        scenario.work_ids = ["work_0123456789abcde"]
        scenario.markers = {"task_result": "SCHEDULER-TASK-RESULT-abcd"}

        status, response = scenario.consume({"input": []})

        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["type"], "message")
        self.assertEqual(scenario.phase, 2)

        status, response = scenario.consume({"input": []})

        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["name"], "ExecCommand")

    def test_openai_stub_concurrent_callback_starts_a_resume_tools(self) -> None:
        scenario = stub.Scenario("scheduler-concurrent")
        scenario.phase = 5
        scenario.work_ids = [
            "work_0123456789abcde",
            "work_fedcba987654321",
        ]
        scenario.markers = {"concurrent_a": "SCHEDULER-CONCURRENT-A-abcd"}
        callback = {
            "input": [
                {
                    "type": "message",
                    "content": [
                        {
                            "type": "input_text",
                            "text": (
                                "## current_input\nCurrent input:\n"
                                "- [system][trigger:system_tick] "
                                "SCHEDULER-CONCURRENT-A-abcd"
                            ),
                        }
                    ],
                }
            ]
        }

        status, response = scenario.consume({"input": []})

        self.assertEqual(status, 200)
        self.assertEqual(response["output"][-1]["name"], "CompleteWorkItem")
        self.assertEqual(scenario.phase, 6)

        status, response = scenario.consume(callback)

        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["name"], "GetWorkItem")
        self.assertEqual(scenario.phase, 8)

    def test_openai_stub_registers_every_required_scenario(self) -> None:
        profile = runner.resolve_profile(self.manifest, "scheduler-required")
        selected = runner.select_cases(
            self.manifest,
            requested=profile["case_ids"],
            suite="core",
            tags=[],
        )
        self.assertEqual(
            {case["stub_scenario"] for case in selected},
            set(stub.SCENARIOS),
        )
        for case in selected:
            scenario = stub.Scenario(case["stub_scenario"])
            self.assertGreater(scenario.expected_phase(), 0)

    def test_openai_stub_required_scenarios_reach_exact_completion(self) -> None:
        expected_calls = {
            "scheduler-task-wait": [
                "CreateWorkItem", "ExecCommand", "WaitFor", "GetWorkItem",
                "WaitFor", "GetWorkItem", "UpdateWorkItem", "CompleteWorkItem",
            ],
            "scheduler-provider-retry": ["ListWorkItems", "CompleteWorkItem"],
            "scheduler-multi": [
                "CreateWorkItem", "CreateWorkItem", "AgentGet", "ListWorkItems",
                "UpdateWorkItem", "CompleteWorkItem", "GetWorkspaceState",
                "ListWorkItems", "UpdateWorkItem", "CompleteWorkItem",
            ],
            "scheduler-external": [
                "CreateWorkItem", "PickWorkItem", "WaitFor", "GetWorkItem",
                "UpdateWorkItem", "CompleteWorkItem",
            ],
            "scheduler-operator": [
                "CreateWorkItem", "PickWorkItem", "WaitFor", "GetWorkItem",
                "UpdateWorkItem", "CompleteWorkItem",
            ],
            "scheduler-concurrent": [
                "CreateWorkItem", "PickWorkItem", "WaitFor",
                "ListWorkItems", "UpdateWorkItem", "CompleteWorkItem",
                "GetWorkItem", "UpdateWorkItem", "CompleteWorkItem",
            ],
            "scheduler-interject": [
                "CreateWorkItem", "PickWorkItem", "WaitFor", "CreateWorkItem",
                "ListWorkItems", "UpdateWorkItem", "CompleteWorkItem",
                "GetWorkItem", "UpdateWorkItem", "CompleteWorkItem",
            ],
            "scheduler-compaction": [
                "CreateWorkItem", "PickWorkItem", "ExecCommand", "ExecCommand",
                "ExecCommand", "WaitFor", "GetWorkItem", "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-worktree": [
                "CreateWorkItem", "PickWorkItem", "GetWorkspaceState",
                "CreateWorktree", "GetWorkspaceState", "SwitchWorkspace",
                "GetWorkspaceState", "RemoveWorktree", "GetWorkspaceState",
                "UpdateWorkItem", "CompleteWorkItem",
            ],
            "scheduler-spawn": [
                "CreateWorkItem", "PickWorkItem", "SpawnAgent", "TaskStatus",
                "UpdateWorkItem", "CompleteWorkItem",
            ],
            "scheduler-checkpoint": [
                "CreateWorkItem", "CreateWorkItem", "ListWorkItems", "WaitFor",
                "ListWorkItems", "UpdateWorkItem", "CompleteWorkItem",
                "GetWorkItem", "UpdateWorkItem", "CompleteWorkItem",
            ],
        }
        transcript = (
            "## current_input\nCurrent input:\n"
            "- [system][trigger:system_tick] Scheduler Docker E2E case "
            "SCHEDULER-TASK-WAIT-abcd SCHEDULER-TASK-RESULT-abcd "
            "SCHEDULER-TASK-WAIT-COMPLETE-abcd "
            "SCHEDULER-PROVIDER-RETRY-abcd "
            "SCHEDULER-PROVIDER-RETRY-COMPLETE-abcd "
            "SCHEDULER-MULTI-A-abcd SCHEDULER-MULTI-B-abcd "
            "SCHEDULER-MULTI-COMPLETE-A-abcd "
            "SCHEDULER-MULTI-COMPLETE-B-abcd "
            "SCHEDULER-EXTERNAL-WAIT-abcd "
            "SCHEDULER-EXTERNAL-COMPLETE-abcd "
            "SCHEDULER-OPERATOR-WAIT-abcd "
            "SCHEDULER-OPERATOR-COMPLETE-abcd "
            "SCHEDULER-CONCURRENT-A-abcd SCHEDULER-CONCURRENT-B-abcd "
            "SCHEDULER-CONCURRENT-COMPLETE-A-abcd "
            "SCHEDULER-CONCURRENT-COMPLETE-B-abcd "
            "SCHEDULER-INTERJECT-A-abcd SCHEDULER-INTERJECT-B-abcd "
            "SCHEDULER-INTERJECT-COMPLETE-A-abcd "
            "SCHEDULER-INTERJECT-COMPLETE-B-abcd "
            "SCHEDULER-COMPACTION-abcd "
            "SCHEDULER-COMPACTION-COMPLETE-abcd "
            "SCHEDULER-WORKTREE-abcd "
            "SCHEDULER-WORKTREE-COMPLETE-abcd "
            "SCHEDULER-SPAWN-abcd SCHEDULER-SPAWN-CHILD-abcd "
            "SCHEDULER-SPAWN-COMPLETE-abcd "
            "SCHEDULER-REPLAY-A-abcd SCHEDULER-REPLAY-B-abcd "
            "SCHEDULER-REPLAY-COMPLETE-A-abcd "
            "SCHEDULER-REPLAY-COMPLETE-B-abcd "
            "docker-e2e:abcd e2e-worktree-abcd "
            "work_0123456789abcde work_fedcba987654321 "
            "task_0123456789abcde ws_0123456789abcdef "
            "git_worktree_root:deterministic"
        )
        request = {
            "input": [{
                "type": "message",
                "content": [{"type": "input_text", "text": transcript}],
            }]
        }
        for name, expected in expected_calls.items():
            scenario = stub.Scenario(name)
            actual = []
            for _ in range(scenario.expected_phase()):
                phase_request = request
                if name == "scheduler-concurrent" and scenario.phase == 6:
                    phase_request = {
                        "input": [
                            {
                                "type": "function_call_output",
                                "output": "{}",
                            }
                        ]
                    }
                status, value = scenario.consume(phase_request)
                self.assertEqual(status, 200, name)
                actual.extend(
                    item["name"]
                    for item in value["output"]
                    if item["type"] == "function_call"
                )
            self.assertEqual(actual, expected, name)
            self.assertTrue(scenario.status()["complete"], name)
            status, value = scenario.consume(request)
            self.assertEqual(status, 409, name)
            self.assertEqual(value["error"]["type"], "transcript_exhausted")

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

    def test_secret_scan_accepts_redacted_callback_with_log_punctuation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "evidence.txt").write_text(
                "callback=/api/callbacks/wake/<redacted>.\n"
                'callback="/api/callbacks/enqueue/<redacted>\\u001b[0m"\n'
            )
            result = runner.secret_scan(root, [])
            self.assertEqual(result["status"], "pass")

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

    def test_capture_logs_redacts_callback_capability(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="log-redaction-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            harness.docker = lambda *args, **kwargs: subprocess.CompletedProcess(
                ["docker", *args],
                0,
                "callback=/api/callbacks/wake/cb_secret-capability\n",
                "",
            )

            harness.capture_logs()

            captured = (harness.evidence / "container-1.log").read_text()
            self.assertNotIn("cb_secret-capability", captured)
            self.assertIn("/api/callbacks/wake/<redacted>", captured)

    def test_request_retries_retryable_projection_busy(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="projection-retry-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            harness.base_url = "http://127.0.0.1:7878"
            busy = urllib.error.HTTPError(
                harness.base_url + "/api/agents/default/state",
                429,
                "Too Many Requests",
                {"Retry-After": "0"},
                io.BytesIO(
                    json.dumps(
                        {
                            "code": "projection_busy",
                            "retryable": True,
                        }
                    ).encode()
                ),
            )

            class Response:
                status = 200
                headers: dict[str, str] = {}

                def __enter__(self) -> "Response":
                    return self

                def __exit__(self, *_: object) -> None:
                    return None

                def read(self) -> bytes:
                    return b'{"ok":true}'

            with (
                patch.object(
                    runner.urllib.request,
                    "urlopen",
                    side_effect=[busy, Response()],
                ) as urlopen,
                patch.object(runner.time, "sleep"),
            ):
                result = harness.request("GET", "/api/agents/default/state")

            self.assertEqual(result, {"ok": True})
            self.assertEqual(urlopen.call_count, 2)

    def test_runtime_db_snapshot_records_docker_copy_timeout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="snapshot-timeout-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )

            def timeout_docker(
                *args: str, **kwargs: object
            ) -> subprocess.CompletedProcess[str]:
                self.assertEqual(
                    kwargs["timeout"],
                    runner.RUNTIME_DB_COPY_TIMEOUT_SECONDS,
                )
                raise subprocess.TimeoutExpired(
                    ["docker", *args],
                    runner.RUNTIME_DB_COPY_TIMEOUT_SECONDS,
                    output="partial",
                    stderr="daemon stalled",
                )

            harness.docker = timeout_docker
            with self.assertRaises(subprocess.TimeoutExpired):
                harness.runtime_db_snapshot("scheduler")

            failure = json.loads(
                (
                    harness.evidence
                    / "scheduler-runtime-state-copy-failure.json"
                ).read_text()
            )
            self.assertEqual(failure["status"], "timeout")
            self.assertEqual(
                failure["timeout_seconds"],
                runner.RUNTIME_DB_COPY_TIMEOUT_SECONDS,
            )
            self.assertEqual(failure["stdout"], "partial")
            self.assertEqual(failure["stderr"], "daemon stalled")

    def test_docker_circuit_breaker_opens_after_consecutive_timeouts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="docker-breaker-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            with patch.object(
                runner,
                "run",
                side_effect=subprocess.TimeoutExpired(["docker", "version"], 1),
            ) as command:
                for _ in range(runner.DOCKER_CIRCUIT_BREAKER_THRESHOLD):
                    with self.assertRaises(subprocess.TimeoutExpired):
                        harness.docker("version")
                with self.assertRaises(runner.DockerCircuitBreakerOpen):
                    harness.docker("version")
            self.assertEqual(
                command.call_count,
                runner.DOCKER_CIRCUIT_BREAKER_THRESHOLD,
            )

    def test_checkpoint_claims_are_shared_by_worker_copies(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="checkpoint-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            worker = copy.copy(harness)
            self.assertTrue(harness.claim_checkpoint("wait-rearm-db"))
            self.assertFalse(worker.claim_checkpoint("wait-rearm-db"))

    def test_capture_context_uses_incremental_event_cursor(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="incremental-context-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            harness.agent_id = "default"
            paths: list[str] = []

            def request(method: str, path: str, *_: object, **__: object) -> object:
                self.assertEqual(method, "GET")
                paths.append(path)
                if "/events?" in path:
                    return {"events": [], "cursor_seq": 17}
                if path.endswith("/state"):
                    return {}
                if "/work-items?" in path:
                    return []
                raise AssertionError(f"unexpected path: {path}")

            harness.request = request
            harness.capture_context(
                "operation-final",
                after_seq=12,
                include_conversation=False,
            )
            self.assertTrue(
                any(
                    "events?limit=300&order=asc&after_seq=12" in path
                    for path in paths
                )
            )
            self.assertFalse(any("/briefs?" in path for path in paths))
            self.assertFalse(any("/transcript?" in path for path in paths))

    def test_event_batch_pages_until_the_latest_event(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="event-pagination-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            harness.agent_id = "default"
            pages = [
                {
                    "events": [{"type": "turn_started", "payload": {}}],
                    "newest_seq": 12,
                    "has_newer": True,
                },
                {
                    "events": [{"type": "turn_terminal", "payload": {}}],
                    "newest_seq": 14,
                    "has_newer": False,
                },
            ]
            paths: list[str] = []

            def request(method: str, path: str, *_: object, **__: object) -> object:
                self.assertEqual(method, "GET")
                paths.append(path)
                return pages.pop(0)

            harness.request = request
            batch = harness.event_batch("paged", after_seq=10, limit=2)

            self.assertEqual(
                [event["type"] for event in batch["events"]],
                ["turn_started", "turn_terminal"],
            )
            self.assertEqual(batch["newest_seq"], 14)
            self.assertIn("after_seq=10", paths[0])
            self.assertIn("after_seq=12", paths[1])

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

    def test_tool_assertion_uses_turn_started_index_for_all_events(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="turn-index-scope-test",
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
                    "payload": {"turn_id": "turn-target", "turn_index": 4},
                },
                {
                    "type": "runtime_error",
                    "payload": {
                        "turn_id": "turn-target",
                        "turn_index": 1,
                        "domain": "provider",
                        "error": "provider request failed",
                    },
                },
            ]

            with self.assertRaisesRegex(
                AssertionError,
                "runtime failure occurred in complete: provider",
            ):
                harness.assert_tools("complete", 3, ["CompleteWorkItem"])

    def test_prompt_waits_for_the_submitted_message_terminal(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="prompt-target-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            harness.agent_id = "default"
            state = {
                "agent": {
                    "agent": {
                        "turn_index": 8,
                        "status": "awake_idle",
                        "current_run_id": None,
                        "last_runtime_failure": None,
                    }
                },
                "session": {"pending_count": 4},
            }
            requests: list[tuple[str, str]] = []

            def request(
                method: str,
                path: str,
                body: object = None,
                **_: object,
            ) -> object:
                requests.append((method, path))
                if method == "POST":
                    self.assertEqual(body, {"text": "target prompt"})
                    return {"ok": True, "message_id": "message-target"}
                if "events?limit=1" in path:
                    return {"events": [], "cursor_seq": 10}
                if path.endswith("/state"):
                    return state
                if "events?limit=300" in path:
                    return {
                        "events": [
                            {
                                "type": "turn_started",
                                "payload": {
                                    "message_id": "message-target",
                                    "turn_id": "turn-target",
                                    "turn_index": 8,
                                },
                            },
                            {
                                "type": "turn_terminal",
                                "payload": {
                                    "turn_id": "turn-target",
                                    "turn_index": 8,
                                    "kind": "completed",
                                },
                            },
                        ]
                    }
                if "/work-items?" in path:
                    return []
                raise AssertionError(f"unexpected request: {method} {path}")

            harness.request = request
            with patch.object(harness, "capture_context"):
                baseline, final_state = harness.prompt(
                    "target",
                    "target prompt",
                )

            self.assertEqual(baseline, 8)
            self.assertIs(final_state, state)
            self.assertEqual(
                harness.prompt_scope("target"),
                {
                    "message_id": "message-target",
                    "turn_id": "turn-target",
                    "turn_index": 8,
                    "terminal_kind": "completed",
                },
            )
            self.assertEqual(state["session"]["pending_count"], 4)

    def test_tool_assertion_can_scope_to_prompt_message(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="tool-scope-test",
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
                    "payload": {
                        "message_id": "message-target",
                        "turn_id": "turn-target",
                        "turn_index": 4,
                    },
                },
                {
                    "type": "turn_started",
                    "payload": {
                        "message_id": "message-other",
                        "turn_id": "turn-other",
                        "turn_index": 5,
                    },
                },
                {
                    "type": "tool_executed",
                    "payload": {
                        "turn_id": "turn-target",
                        "turn_index": 4,
                        "tool_name": "CreateWorkItem",
                        "status": "success",
                    },
                },
                {
                    "type": "tool_executed",
                    "payload": {
                        "turn_id": "turn-other",
                        "turn_index": 5,
                        "tool_name": "CompleteWorkItem",
                        "status": "success",
                    },
                },
            ]

            events = harness.assert_tools(
                "create",
                3,
                ["CreateWorkItem"],
                ["CompleteWorkItem"],
                message_id="message-target",
            )

            self.assertEqual(
                [event["payload"]["tool_name"] for event in events],
                ["CreateWorkItem"],
            )

    def test_no_model_harness_uses_inert_provider_bootstrap(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="offline-test",
                image="holon:test",
                model="openai/gpt-test",
                requires_model=False,
                credential_envs=["OPENAI_API_KEY"],
                env_file=Path(directory) / "credentials.env",
                runtime_env={"HOLON_TEST_MARKER": "true"},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )

            self.assertEqual(harness.model, runner.DEFAULT_MODEL)
            self.assertEqual(harness.credential_envs, [])
            self.assertIsNone(harness.env_file)
            self.assertEqual(
                harness.runtime_env[runner.OFFLINE_MODEL_CREDENTIAL_ENV],
                runner.OFFLINE_MODEL_CREDENTIAL,
            )
            self.assertEqual(harness.runtime_env["HOLON_TEST_MARKER"], "true")

    def test_harness_accepts_stable_resources_and_model_fallbacks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory) / "workspace"
            harness = runner.CaseHarness(
                case_id="scheduler-drill",
                image="holon:test",
                model="volcengine@plan/glm-5.2",
                model_fallbacks=["dashscope@token-plan/qwen3.8-max-preview"],
                disable_provider_fallback=False,
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=True,
                resource_names={
                    "volume": "drill-volume",
                    "network": "drill-network",
                    "container": "drill-container",
                    "workspace_parent": str(workspace),
                },
                control_token="stable-control-token",
            )
            commands: list[tuple[str, ...]] = []

            def fake_docker(
                *args: str, **_: object
            ) -> subprocess.CompletedProcess[str]:
                commands.append(args)
                if args[:2] == ("network", "inspect"):
                    return subprocess.CompletedProcess(["docker", *args], 1, "", "")
                if args[:2] == ("port", "drill-container"):
                    return subprocess.CompletedProcess(
                        ["docker", *args], 0, "127.0.0.1:49152\n", ""
                    )
                return subprocess.CompletedProcess(["docker", *args], 0, "", "")

            harness.docker = fake_docker
            harness.wait_readiness = lambda: None
            harness.wait_agent_idle = lambda: None
            harness.start()

            docker_run = next(command for command in commands if command[0] == "run")
            self.assertEqual(harness.volume, "drill-volume")
            self.assertEqual(harness.network, "drill-network")
            self.assertEqual(harness.container, "drill-container")
            self.assertEqual(harness.workspace_parent, workspace)
            self.assertEqual(harness.token, "stable-control-token")
            self.assertIn("HOLON_DISABLE_PROVIDER_FALLBACK=false", docker_run)
            self.assertIn(
                'HOLON_MODEL_FALLBACKS=["dashscope@token-plan/qwen3.8-max-preview"]',
                docker_run,
            )

    def test_stub_start_forces_endpoint_and_seeds_model_override_once(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="stub-model-override",
                image="holon:test",
                model="external/model",
                credential_envs=[],
                env_file=None,
                runtime_env={
                    "HOLON_OPENAI_BASE_URL": "https://external.invalid/v1",
                },
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=True,
                provider_mode="stub",
                stub_scenario="scheduler-compaction",
                model_runtime_override={
                    "prompt_budget_estimated_tokens": 80_000,
                    "compaction_trigger_estimated_tokens": 70_000,
                    "compaction_keep_recent_estimated_tokens": 8_000,
                },
            )
            commands: list[tuple[str, ...]] = []

            def fake_docker(
                *args: str, **_: object
            ) -> subprocess.CompletedProcess[str]:
                commands.append(args)
                if args[:2] in {
                    ("network", "inspect"),
                    ("inspect", "--format"),
                }:
                    return subprocess.CompletedProcess(["docker", *args], 1, "", "")
                if args[:2] == ("port", harness.container):
                    return subprocess.CompletedProcess(
                        ["docker", *args], 0, "127.0.0.1:49152\n", ""
                    )
                return subprocess.CompletedProcess(["docker", *args], 0, "", "")

            harness.docker = fake_docker
            harness.wait_readiness = lambda: None
            harness.wait_agent_idle = lambda: None

            harness.start()
            harness.start()

            self.assertEqual(harness.model, "openai/gpt-5.4")
            self.assertEqual(
                harness.runtime_env["HOLON_OPENAI_BASE_URL"],
                "http://provider-stub:8080/v1",
            )
            seed_runs = [
                command
                for command in commands
                if command[0] == "run"
                and "/var/lib/holon/config.json" in " ".join(command)
            ]
            self.assertEqual(len(seed_runs), 1)
            seeded = json.loads(seed_runs[0][-1])
            self.assertEqual(
                seeded["models"]["catalog"]["openai/gpt-5.4"],
                harness.model_runtime_override,
            )

    def test_scheduler_extended_cases_use_real_lifecycle_fixtures(self) -> None:
        selected = runner.select_cases(
            self.manifest, requested=None, suite="extended", tags=["scheduler"]
        )
        self.assertEqual(
            [case["id"] for case in selected],
            [
                "scheduler-task-wait-resume",
                "scheduler-provider-failure-work-queue-retry",
                "scheduler-multi-workitem-scheduling",
                "scheduler-external-wait-resume",
                "scheduler-operator-wait-resume",
                "scheduler-concurrent-claim-fencing",
                "scheduler-operator-interject-during-wait",
                "scheduler-compaction-continuity",
                "scheduler-worktree-isolation",
                "scheduler-spawn-agent-supervision",
                "scheduler-checkpoint-replay",
            ],
        )
        task_wait = selected[0]
        self.assertEqual(len(task_wait["phases"]), 1)
        self.assertIn(
            "ExecCommand", task_wait["phases"][0]["required_tools"]
        )
        self.assertIn("WaitFor", task_wait["phases"][0]["required_tools"])
        self.assertNotIn(
            "PickWorkItem", task_wait["phases"][0]["required_tools"]
        )
        for case in selected:
            self.assertNotIn(
                "HOLON_SCHEDULER_ACCEPTANCE_FIXTURES",
                case.get("runtime_env", {}),
            )
            self.assertNotIn("HOLON_SCHEDULER", case.get("runtime_env", {}))

    def test_scheduler_matrix_expands_only_scheduler_cases(self) -> None:
        selected = runner.select_cases(
            self.manifest, requested=None, suite="core", tags=[]
        ) + runner.select_cases(
            self.manifest,
            requested=["scheduler-task-wait-resume"],
            suite="extended",
            tags=[],
        )
        expanded = runner.expand_case_matrix(selected, scheduler_matrix=True)
        self.assertEqual(
            [(case["id"], engine) for case, engine in expanded],
            [
                ("runtime-auth-model-delivery", None),
                ("memory-agent-home-persistence", None),
                ("workspace-restart-lifecycle", None),
                ("workitem-wait-restart-complete", None),
                ("scheduler-task-wait-resume", "legacy"),
                ("scheduler-task-wait-resume", "canonical"),
            ],
        )

    def test_e2e_tier_1_cases_have_correct_config(self) -> None:
        selected = runner.select_cases(
            self.manifest, requested=None, suite="extended", tags=["e2e-tier-1"]
        )
        self.assertEqual(
            [case["id"] for case in selected],
            [
                "scheduler-multi-workitem-scheduling",
                "scheduler-external-wait-resume",
                "scheduler-operator-wait-resume",
            ],
        )
        for case in selected:
            self.assertNotIn(
                "HOLON_SCHEDULER_ACCEPTANCE_FIXTURES",
                case.get("runtime_env", {}),
                f"{case['id']} should not use acceptance fixtures",
            )
            self.assertEqual(len(case["phases"]), 1)

    def test_e2e_tier_2_cases_have_correct_config(self) -> None:
        selected = runner.select_cases(
            self.manifest, requested=None, suite="extended", tags=["e2e-tier-2"]
        )
        self.assertEqual(
            [case["id"] for case in selected],
            [
                "scheduler-concurrent-claim-fencing",
                "scheduler-operator-interject-during-wait",
                "scheduler-compaction-continuity",
                "scheduler-worktree-isolation",
                "scheduler-spawn-agent-supervision",
                "scheduler-checkpoint-replay",
            ],
        )
        for case in selected:
            self.assertNotIn(
                "HOLON_SCHEDULER_ACCEPTANCE_FIXTURES",
                case.get("runtime_env", {}),
                f"{case['id']} should not use acceptance fixtures",
            )
            self.assertEqual(len(case["phases"]), 1)
        compaction = next(
            case
            for case in selected
            if case["id"] == "scheduler-compaction-continuity"
        )
        self.assertEqual(
            compaction["model_runtime_override"],
            {
                "prompt_budget_estimated_tokens": 80000,
                "compaction_trigger_estimated_tokens": 70000,
                "compaction_keep_recent_estimated_tokens": 8000,
            },
        )

    def test_manifest_rejects_non_boolean_requires_model(self) -> None:
        invalid = json.loads(json.dumps(self.manifest))
        invalid["cases"][-1]["requires_model"] = "false"
        with self.assertRaisesRegex(AssertionError, "requires_model must be boolean"):
            runner.validate_manifest(invalid)

    def test_scheduler_acceptance_report_binds_build_and_fixture_identity(self) -> None:
        run_record = {
            "git_sha": "abc123",
            "image": {
                "ref": "ghcr.io/holon-run/holon@sha256:deadbeef",
                "id": "sha256:image",
                "repo_digests": ["ghcr.io/holon-run/holon@sha256:deadbeef"],
            },
            "image_digest": "ghcr.io/holon-run/holon@sha256:deadbeef",
            "manifest_sha256": "manifest-hash",
        }
        case_results = [
            {
                "id": f"scheduler-task-wait-resume-{engine}",
                "base_id": "scheduler-task-wait-resume",
                "scheduler_engine": engine,
                "status": "pass",
                "schema_revision": 40,
            }
            for engine in runner.SCHEDULER_ENGINES
        ]
        report = runner.scheduler_acceptance_report(
            run_record=run_record,
            case_results=case_results,
            fixture_corpus_revision="scheduler-release-acceptance-v1",
            required_coverage_ids={"scheduler-task-wait-resume"},
        )
        self.assertEqual(
            report["schema_version"],
            runner.SCHEDULER_ACCEPTANCE_REPORT_SCHEMA_VERSION,
        )
        self.assertEqual(report["status"], "pass")
        self.assertEqual(report["git_sha"], "abc123")
        self.assertEqual(report["runtime_schema_revision"], 40)
        self.assertEqual(
            report["image_digest"],
            "ghcr.io/holon-run/holon@sha256:deadbeef",
        )
        self.assertEqual(
            report["fixture_corpus_revision"],
            "scheduler-release-acceptance-v1",
        )
        self.assertEqual(
            [engine["engine"] for engine in report["engines"]],
            ["legacy", "canonical"],
        )

    def test_scheduler_acceptance_report_rejects_schema_drift(self) -> None:
        run_record = {
            "git_sha": "abc123",
            "image": {"ref": "holon:test", "id": None, "repo_digests": []},
            "image_digest": None,
            "manifest_sha256": "manifest-hash",
        }
        case_results = [
            {
                "id": f"scheduler-task-wait-resume-{engine}",
                "base_id": "scheduler-task-wait-resume",
                "scheduler_engine": engine,
                "status": "pass",
                "schema_revision": revision,
            }
            for engine, revision in (("legacy", 39), ("canonical", 40))
        ]
        report = runner.scheduler_acceptance_report(
            run_record=run_record,
            case_results=case_results,
            fixture_corpus_revision="scheduler-release-acceptance-v1",
            required_coverage_ids={"scheduler-task-wait-resume"},
        )
        self.assertEqual(report["status"], "fail")
        self.assertIsNone(report["runtime_schema_revision"])
        self.assertIn(
            "scheduler_schema_revision_mismatch",
            {diagnostic["code"] for diagnostic in report["diagnostics"]},
        )

    def test_scheduler_acceptance_report_distinguishes_missing_schema_evidence(self) -> None:
        run_record = {
            "git_sha": "abc123",
            "image": {"ref": "holon:test", "id": None, "repo_digests": []},
            "image_digest": None,
            "manifest_sha256": "manifest-hash",
        }
        case_results = [
            {
                "id": f"scheduler-task-wait-resume-{engine}",
                "base_id": "scheduler-task-wait-resume",
                "scheduler_engine": engine,
                "status": "fail",
                "schema_revision": None,
                "failure_kind": (
                    "case_timeout" if engine == "legacy" else None
                ),
                "evidence_collection_error": (
                    "docker cp failed" if engine == "canonical" else ""
                ),
            }
            for engine in ("legacy", "canonical")
        ]

        report = runner.scheduler_acceptance_report(
            run_record=run_record,
            case_results=case_results,
            fixture_corpus_revision="scheduler-release-acceptance-v1",
            required_coverage_ids={"scheduler-task-wait-resume"},
        )

        diagnostic = next(
            value
            for value in report["diagnostics"]
            if value["code"] == "missing_schema_revision_cases"
        )
        self.assertEqual(
            report["missing_schema_revision_cases"],
            [
                "scheduler-task-wait-resume-canonical",
                "scheduler-task-wait-resume-legacy",
            ],
        )
        self.assertEqual(
            diagnostic["case_timeouts"],
            ["scheduler-task-wait-resume-legacy"],
        )
        self.assertEqual(
            diagnostic["evidence_collection_failures"],
            ["scheduler-task-wait-resume-canonical"],
        )
        self.assertNotIn(
            "scheduler_schema_revision_mismatch",
            {value["code"] for value in report["diagnostics"]},
        )

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

    def test_canonical_wait_resolution_rejects_consuming_activation(self) -> None:
        harness = type(
            "CanonicalHarness",
            (),
            {"canonical_scheduler_enabled": True},
        )()
        canonical_wait = {
            "wait_id": "wait-1",
            "owner_work_item_id": "work-1",
            "lifecycle_state": "resolved",
            "consuming_activation_id": None,
        }
        snapshot = {"scheduler_wait_generations": [canonical_wait]}
        runner.require_scheduler_engine_wait_resolution(
            harness,
            snapshot,
            work_item_id="work-1",
            wait_ids={"wait-1"},
        )

        canonical_wait["consuming_activation_id"] = "activation-unexpected"
        with self.assertRaisesRegex(
            AssertionError, "canonical waits did not resolve exactly once"
        ):
            runner.require_scheduler_engine_wait_resolution(
                harness,
                snapshot,
                work_item_id="work-1",
                wait_ids={"wait-1"},
            )

    def test_legacy_wait_terminal_accepts_completion_cancellation_evidence(self) -> None:
        harness = type(
            "LegacyHarness",
            (),
            {"canonical_scheduler_enabled": False},
        )()
        snapshot = {
            "wait_conditions": [
                {
                    "wait_condition_id": "wait-1",
                    "work_item_id": "work-1",
                    "kind": "external",
                    "status": "cancelled",
                }
            ],
            "audit_events": [
                {
                    "kind": "callback_delivered",
                    "data_json": json.dumps(
                        {
                            "data": {
                                "disposition": "triggered",
                                "external_trigger_id": "trigger-unrelated",
                            }
                        }
                    ),
                },
                {
                    "kind": "callback_delivered",
                    "data_json": json.dumps(
                        {
                            "data": {
                                "disposition": "triggered",
                                "external_trigger_id": "trigger-target",
                            }
                        }
                    ),
                },
                {
                    "kind": "wait_conditions_cancelled",
                    "data_json": json.dumps(
                        {
                            "data": {
                                "work_item_id": "work-1",
                                "reason": "work_item_completed",
                                "wait_condition_ids": ["wait-1"],
                            }
                        }
                    ),
                },
            ],
        }

        waits = runner.require_scheduler_wait_terminal(
            harness,
            snapshot,
            work_item_id="work-1",
            wait_kind="external",
            require_callback_trigger=True,
            callback_external_trigger_id="trigger-target",
        )
        self.assertEqual(waits[0]["status"], "cancelled")

        snapshot["audit_events"] = snapshot["audit_events"][:2]
        with self.assertRaisesRegex(
            AssertionError,
            "cancellation lacked completion evidence",
        ):
            runner.require_scheduler_wait_terminal(
                harness,
                snapshot,
                work_item_id="work-1",
                wait_kind="external",
                require_callback_trigger=True,
                callback_external_trigger_id="trigger-target",
            )

        snapshot["audit_events"] = [
            snapshot["audit_events"][0],
            {
                "kind": "wait_conditions_cancelled",
                "data_json": json.dumps(
                    {
                        "data": {
                            "work_item_id": "work-1",
                            "reason": "work_item_completed",
                            "wait_condition_ids": ["wait-1"],
                        }
                    }
                ),
            },
        ]
        with self.assertRaisesRegex(
            AssertionError,
            "lacked callback trigger evidence",
        ):
            runner.require_scheduler_wait_terminal(
                harness,
                snapshot,
                work_item_id="work-1",
                wait_kind="external",
                require_callback_trigger=True,
                callback_external_trigger_id="trigger-target",
            )

    def test_canonical_wait_terminal_requires_callback_evidence(self) -> None:
        harness = type(
            "CanonicalHarness",
            (),
            {"canonical_scheduler_enabled": True},
        )()
        snapshot = {
            "wait_conditions": [
                {
                    "wait_condition_id": "wait-1",
                    "work_item_id": "work-1",
                    "kind": "external",
                    "status": "resolved",
                }
            ],
            "audit_events": [],
        }

        with self.assertRaisesRegex(
            AssertionError,
            "lacked callback trigger evidence",
        ):
            runner.require_scheduler_wait_terminal(
                harness,
                snapshot,
                work_item_id="work-1",
                wait_kind="external",
                require_callback_trigger=True,
                callback_external_trigger_id="trigger-target",
            )

        snapshot["audit_events"] = [
            {
                "kind": "callback_delivered",
                "data_json": json.dumps(
                    {
                        "data": {
                            "disposition": "triggered",
                            "external_trigger_id": "trigger-target",
                        }
                    }
                ),
            }
        ]
        waits = runner.require_scheduler_wait_terminal(
            harness,
            snapshot,
            work_item_id="work-1",
            wait_kind="external",
            require_callback_trigger=True,
            callback_external_trigger_id="trigger-target",
        )
        self.assertEqual(waits[0]["status"], "resolved")

    def test_canonical_activation_oracle_distinguishes_lifecycle_and_work_item(self) -> None:
        harness = type(
            "CanonicalHarness",
            (),
            {
                "canonical_scheduler_enabled": True,
                "agent_id": "default",
            },
        )()
        snapshot = {
            "scheduler_activations": [
                {
                    "agent_id": "default",
                    "activation_id": "activation:message:message-create",
                    "work_item_id": None,
                    "admission_kind": "lifecycle_external_nudge",
                    "lifecycle_state": "settled",
                },
                {
                    "agent_id": "default",
                    "activation_id": "activation:message:message-resume",
                    "work_item_id": "work-1",
                    "admission_kind": "wait_resume",
                    "lifecycle_state": "settled",
                },
            ],
            "scheduler_activation_settlements": [
                {"activation_id": "activation:message:message-create"},
                {"activation_id": "activation:message:message-resume"},
            ],
            "scheduler_missing_settlements": [],
            "scheduler_agent_slots": [
                {
                    "agent_id": "default",
                    "slot_kind": "idle",
                    "activation_id": None,
                }
            ],
        }

        runner.require_scheduler_engine_activation_chain(
            harness,
            snapshot,
            work_item_id="work-1",
            expected_admission_kinds=("wait_resume",),
            lifecycle_message_ids={"message-create"},
        )

        snapshot["scheduler_activations"][0]["work_item_id"] = "work-1"
        with self.assertRaisesRegex(
            AssertionError,
            "lifecycle activations did not settle without claiming a WorkItem",
        ):
            runner.require_scheduler_engine_activation_chain(
                harness,
                snapshot,
                work_item_id="work-1",
                expected_admission_kinds=("wait_resume",),
                lifecycle_message_ids={"message-create"},
            )

    def test_external_wait_resume_drains_queue_before_runtime_snapshot(self) -> None:
        source = inspect.getsource(runner.run_scheduler_external_wait_resume_case)
        self.assertLess(
            source.index("harness.wait_queue_drained()"),
            source.index('harness.runtime_db_snapshot("scheduler-external")'),
        )

    def test_compaction_oracle_requires_actual_compaction_evidence(self) -> None:
        event = {
            "kind": "turn_local_compaction_applied",
            "data_json": json.dumps(
                {"data": {"compacted_rounds": 2, "exact_tail_rounds": 1}}
            ),
        }
        events = runner.require_turn_local_compaction(
            {"audit_events": [event]},
            label="test",
        )
        self.assertEqual(events[0]["compacted_rounds"], 2)

        event["data_json"] = json.dumps(
            {"data": {"compacted_rounds": 0, "exact_tail_rounds": 3}}
        )
        with self.assertRaisesRegex(
            AssertionError,
            "compaction stimulus did not produce compacted rounds",
        ):
            runner.require_turn_local_compaction(
                {
                    "audit_events": [
                        event,
                        {
                            "kind": "provider_round_completed",
                            "data_json": json.dumps(
                                {"data": {"compression_epoch": 1}}
                            ),
                        },
                    ]
                },
                label="test",
            )

    def test_checkpoint_restart_lineage_binds_wait_generation(self) -> None:
        before_restart_snapshot = {
            "scheduler_activations": [
                {
                    "activation_id": "activation:schedule",
                    "work_item_id": "work-1",
                    "admitted_generation": 1,
                    "admission_kind": "scheduling",
                    "idempotency_key": "work-queue-attempt:activation:schedule",
                }
            ]
        }
        snapshot = {
            "scheduler_activations": [
                {
                    "activation_id": "activation:schedule",
                    "work_item_id": "work-1",
                    "admitted_generation": 1,
                    "admission_kind": "scheduling",
                    "idempotency_key": "work-queue-attempt:activation:schedule",
                },
                {
                    "activation_id": "activation:resume",
                    "work_item_id": "work-1",
                    "admitted_generation": 2,
                    "admission_kind": "wait_resume",
                    "idempotency_key": "wait-resume:wait-1:2:activation:resume",
                },
            ],
            "scheduler_wait_generations": [
                {
                    "wait_id": "wait-1",
                    "owner_work_item_id": "work-1",
                    "generation": 2,
                    "lifecycle_state": "resolved",
                }
            ],
        }

        runner.require_checkpoint_restart_activation_lineage(
            before_restart_snapshot,
            snapshot,
            work_item_id="work-1",
            wait_id="wait-1",
        )

        snapshot["scheduler_activations"][1]["idempotency_key"] = (
            "wait-resume:other-wait:2:activation:resume"
        )
        with self.assertRaisesRegex(
            AssertionError,
            "wait-resume activation mismatch",
        ):
            runner.require_checkpoint_restart_activation_lineage(
                before_restart_snapshot,
                snapshot,
                work_item_id="work-1",
                wait_id="wait-1",
            )

    def test_wait_work_item_fails_fast_on_duplicate_objective_marker(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="duplicate-work-items",
                image="unused",
                model="unused",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=True,
            )
            harness.agent_id = "default"
            duplicates = [
                {"id": "work-1", "objective": "DRILL-DUPLICATE", "state": "open"},
                {"id": "work-2", "objective": "DRILL-DUPLICATE", "state": "open"},
            ]
            with (
                patch.object(harness, "request", return_value=duplicates),
                patch.object(harness, "capture_context"),
                self.assertRaisesRegex(AssertionError, "multiple WorkItems matched"),
            ):
                harness.wait_work_item(
                    objective_marker="DRILL-DUPLICATE",
                    expected_state="completed",
                    label="duplicate",
                )


if __name__ == "__main__":
    unittest.main()
