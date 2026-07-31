#!/usr/bin/env python3

import importlib.util
import io
import json
import copy
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
        )
        self.assertEqual(report["status"], "fail")
        self.assertIsNone(report["runtime_schema_revision"])
        self.assertIn(
            "scheduler_schema_revision_mismatch",
            {diagnostic["code"] for diagnostic in report["diagnostics"]},
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
