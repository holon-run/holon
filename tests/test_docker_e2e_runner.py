#!/usr/bin/env python3

from __future__ import annotations
import importlib.util
import io
import inspect
import json
import copy
import os
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import Mock, patch


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


def stub_batch_request(*items: tuple[dict, str], recent_turns: str = "") -> dict:
    rendered = [
        f"## recent_turns\n{recent_turns}\n## current_input\n"
        "Current input batch (receive order; each item retains its own "
        "authority and reply route):\n"
    ]
    for index, (overrides, body) in enumerate(items):
        provenance = {
            "projection_version": 1,
            "content_version": "sha256:fixture",
            "id": f"msg-batch-{index}",
            "agent_id": "main",
            "created_at": "2026-10-25T00:33:30.393601007Z",
            "kind": "operator_prompt",
            "origin": {"kind": "operator", "actor_id": "control"},
            "authority_class": "operator_instruction",
            "priority": "normal",
            "delivery_surface": "http_control_prompt",
            "admission_context": "control_authenticated",
            "work_item_id": None,
            "task_id": None,
            "routing_context": None,
            "source_refs": {},
            "correlation_id": None,
            "causation_id": None,
            "full_metadata_source_ref": f"message:msg-batch-{index}",
            **overrides,
        }
        rendered.append(
            f"- reply_context_id: {provenance['id']}\n"
            f"  source_ref: message:{provenance['id']}\n"
            f"  provenance: {json.dumps(provenance)}\n"
            f"  representation: full\n  body:\n{body}\n"
        )
    return {
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "".join(rendered)}],
        }],
    }


class DockerE2ERunnerTests(unittest.TestCase):
    def test_remove_evidence_tree_repairs_docker_owned_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            evidence = Path(temp_dir) / "runtime-upgrade-v030"
            evidence.mkdir()
            repaired = subprocess.CompletedProcess([], 0, "", "")
            with (
                patch.object(
                    runner.shutil,
                    "rmtree",
                    side_effect=[PermissionError("docker-owned"), None],
                ) as remove,
                patch.object(runner, "run", return_value=repaired) as run_command,
            ):
                runner.remove_evidence_tree(evidence, "holon:dev")

            self.assertEqual(remove.call_count, 2)
            command = run_command.call_args.args[0]
            self.assertEqual(command[:6], ["docker", "run", "--rm", "--user", "0:0", "--volume"])
            self.assertEqual(command[-2:], ["sh", evidence.name])
            self.assertEqual(run_command.call_args.kwargs["check"], False)
            self.assertEqual(
                run_command.call_args.kwargs["timeout"],
                runner.DOCKER_CONTROL_TIMEOUT_SECONDS,
            )

    def test_remove_evidence_tree_reports_permission_repair_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            evidence = Path(temp_dir) / "runtime-upgrade-v030"
            evidence.mkdir()
            failed = subprocess.CompletedProcess([], 1, "", "permission repair failed")
            with (
                patch.object(
                    runner.shutil,
                    "rmtree",
                    side_effect=PermissionError("docker-owned"),
                ),
                patch.object(runner, "run", return_value=failed),
            ):
                with self.assertRaisesRegex(
                    RuntimeError,
                    "permission repair failed",
                ):
                    runner.remove_evidence_tree(evidence, "holon:dev")

    def test_skip_build_only_covers_candidate_image(self) -> None:
        source = inspect.getsource(runner.main)
        self.assertIn("if not args.skip_build:", source)
        self.assertIn("if any(", source)
        self.assertNotIn("if not args.skip_build and any(", source)

    def test_previous_schema_revision_accepts_supported_release_schemas(self) -> None:
        self.assertEqual(
            runner.require_previous_schema_revision({"schema_revision": 25}),
            25,
        )
        self.assertEqual(
            runner.require_previous_schema_revision({"schema_revision": 46}),
            46,
        )

    def test_previous_schema_revision_rejects_invalid_revision(self) -> None:
        for snapshot in ({"schema_revision": 0}, {}):
            with (
                self.subTest(snapshot=snapshot),
                self.assertRaisesRegex(
                    AssertionError,
                    "previous release produced an invalid schema revision",
                ),
            ):
                runner.require_previous_schema_revision(snapshot)

    def test_case_harness_clears_previous_case_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence_root = Path(directory)
            stale_evidence = evidence_root / "rerun-case"
            stale_evidence.mkdir()
            (stale_evidence / "failure.txt").write_text("stale failure\n")

            harness = runner.CaseHarness(
                case_id="rerun-case",
                image="holon:test",
                model="test/model",
                requires_model=False,
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=evidence_root,
                timeout_seconds=1,
                keep=False,
            )

            self.assertEqual(harness.evidence, stale_evidence)
            self.assertTrue(stale_evidence.is_dir())
            self.assertFalse((stale_evidence / "failure.txt").exists())

    @staticmethod
    def _agent_state(
        status: str, *, turn_index: int, run_id: str | None = None
    ) -> dict:
        return {
            "agent": {
                "agent": {
                    "status": status,
                    "current_run_id": run_id,
                    "turn_index": turn_index,
                }
            },
            "session": {"pending_count": 0},
        }

    def _settle_harness(self, evidence_root: Path) -> runner.CaseHarness:
        harness = runner.CaseHarness(
            case_id="bootstrap-settle",
            image="holon:test",
            model="test/model",
            requires_model=False,
            credential_envs=[],
            env_file=None,
            runtime_env={},
            evidence_root=evidence_root,
            timeout_seconds=10,
            keep=False,
        )
        harness.agent_id = "main"
        return harness

    def test_wait_default_bootstrap_turn_returns_after_first_turn_settles(self) -> None:
        states = [
            self._agent_state("awake_running", turn_index=1, run_id="run_1"),
            self._agent_state("awake_idle", turn_index=1),
        ]
        with tempfile.TemporaryDirectory() as directory:
            harness = self._settle_harness(Path(directory))
            with (
                patch.object(harness, "request", side_effect=states) as request,
                patch.object(runner.time, "sleep"),
            ):
                harness.wait_default_bootstrap_turn_settled(settle_timeout=5)
            self.assertEqual(request.call_count, 2)

    def test_wait_default_bootstrap_turn_skips_when_none_scheduled(self) -> None:
        state = self._agent_state("awake_idle", turn_index=0)
        with tempfile.TemporaryDirectory() as directory:
            harness = self._settle_harness(Path(directory))
            with (
                patch.object(harness, "request", return_value=state),
                patch.object(runner.time, "sleep"),
            ):
                harness.wait_default_bootstrap_turn_settled(
                    settle_timeout=5, missing_grace=0.0
                )
            skipped = json.loads(
                (harness.evidence / "bootstrap-settle-skipped.json").read_text()
            )
            self.assertEqual(skipped["turn_index"], 0)
            self.assertEqual(skipped["reason"], "no default bootstrap turn was scheduled")

    def test_wait_default_bootstrap_turn_times_out_when_never_settling(self) -> None:
        state = self._agent_state("awake_running", turn_index=1, run_id="run_1")
        with tempfile.TemporaryDirectory() as directory:
            harness = self._settle_harness(Path(directory))
            with (
                patch.object(harness, "request", return_value=state),
                patch.object(runner.time, "sleep"),
                patch.object(harness, "capture_logs") as capture,
            ):
                with self.assertRaises(TimeoutError):
                    harness.wait_default_bootstrap_turn_settled(settle_timeout=0.0)
            capture.assert_called_once()

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
                "runtime-agent-lifecycle",
                "runtime-upgrade-v030",
                "runtime-upgrade-interrupted-schema47",
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
                                    "idempotency_key": (
                                        "work_queue:continue_active:work-1:1:"
                                        "generation:1"
                                    ),
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
                    "idempotency_key": (
                        "work_queue:continue_active:work-1:1:generation:1"
                    ),
                    "status": "aborted",
                }
            ],
        )

    @staticmethod
    def provider_retry_snapshot(*, recovered: bool = False) -> dict:
        root = {
            "id": "root", "agent_id": "main", "work_item_id": "work-1",
            "kind": "system_tick",
            "origin": {"kind": "system", "subsystem": "work_queue"},
            "authority_class": "runtime_instruction",
            "metadata": {"work_queue": {
                "reason": "continue_active", "work_item_id": "work-1",
                "work_item_revision": 1, "work_item_generation": 1,
                "idempotency_key": "work_queue:continue_active:work-1:1:generation:1",
            }},
        }
        parked = {
            "id": "parked", "agent_id": "main", "work_item_id": "work-1",
            "kind": "internal_followup",
            "origin": {"kind": "system", "subsystem": "model_lineage_recovery"},
            "authority_class": "runtime_instruction",
            "admission_context": "runtime_owned", "causation_id": "root",
            "source_refs": {"source_message_id": "root", "source_turn_id": "turn-root"},
            "metadata": {"provider_recovery": {
                "root_message_id": "root", "source_message_id": "root",
                "source_turn_id": "turn-root",
                "source_terminal_kind": "deferred_to_fallback", "fallback_attempt": 1,
            }},
        }

        def audit(kind: str, data: dict) -> dict:
            return {
                "agent_id": "main", "kind": kind,
                "created_at": "2026-10-24T19:57:00.381521253Z",
                "data_json": json.dumps({"data": data}),
            }

        snapshot = {
            "work_items": [{
                "agent_id": "main", "work_item_id": "work-1",
                "state": "completed" if recovered else "open", "revision": 1,
            }],
            "messages": [
                {
                    "message_id": message["id"], "agent_id": "main",
                    "work_item_id": "work-1", "turn_id": f"turn-{message['id']}",
                    "payload_json": json.dumps(message),
                } for message in (root, parked)
            ],
            "queue_entries": [
                {
                    "message_id": mid, "agent_id": "main", "status": status,
                    "payload_json": json.dumps({"status": status}),
                } for mid, status in (
                    ("root", "processed"),
                    ("parked", "processed" if recovered else "queued"),
                )
            ],
            "turn_records": [{
                "turn_id": "turn-root", "agent_id": "main",
                "owner_kind": "work_item", "owner_id": "work-1",
                "trigger_message_id": "root", "terminal_kind": "deferred_to_fallback",
            }],
            "audit_events": [
                audit("provider_attempt_failed", {
                    "turn_id": "turn-root", "failure_kind": "connection",
                    "disposition": "retryable",
                }),
                audit("provider_queue_parked", {
                    "message_id": "parked", "reason": "chain_cooling",
                    "episode_key": "episode-root", "request_count": 1,
                    "untried_candidates": False,
                    "not_before": "2026-10-24T19:57:12.309Z",
                }),
            ],
            "execution_protocol_attempts": [],
            "execution_protocol_outcomes": [],
        }
        if recovered:
            snapshot["turn_records"].append({
                "turn_id": "turn-parked", "agent_id": "main",
                "owner_kind": "work_item", "owner_id": "work-1",
                "trigger_message_id": "parked", "terminal_kind": "completed",
            })
        for mid in ("root", "parked") if recovered else ("root",):
            attempt_id = f"activation:message:{mid}"
            outcome_id = f"outcome:message:{mid}"
            snapshot["execution_protocol_attempts"].append({
                "agent_id": "main", "attempt_id": attempt_id,
                "lifecycle_state": "settled", "terminal_outcome_id": outcome_id,
                "payload_json": json.dumps({
                    "attempt_id": attempt_id, "source_message_id": mid,
                    "source": {"identity": {"kind": (
                        "work_item_continuation" if mid == "root" else "runtime_recovery"
                    )}},
                    "binding": {"kind": "work_item", "work_item_id": "work-1"},
                    "turn_id": f"turn-{mid}",
                    "recovery_of_attempt_id": (
                        None if mid == "root" else "activation:message:root"
                    ),
                }),
            })
            snapshot["execution_protocol_outcomes"].append({
                "agent_id": "main", "outcome_id": outcome_id,
                "payload_json": json.dumps({"attempt_id": attempt_id}),
            })
        return snapshot

    def provider_retry_parking(self, snapshot: dict) -> dict:
        return runner.require_provider_retry_parking(
            snapshot, agent_id="main", work_item_id="work-1",
            parked_message_id="parked",
        )

    def test_provider_retry_parking_retains_unclaimed_same_root(self) -> None:
        parking = self.provider_retry_parking(self.provider_retry_snapshot())
        self.assertEqual(parking["root_attempt_id"], "activation:message:root")
        self.assertEqual(parking["parked_message"]["id"], "parked")

    def test_provider_retry_parking_rejects_foreign_or_forged_source(self) -> None:
        for field, value in (
            ("work_item_id", "foreign-work"), ("agent_id", "foreign-agent"),
            ("kind", "system_tick"), ("authority_class", "operator_instruction"),
            ("origin", {"kind": "operator"}), ("admission_context", "external"),
            ("causation_id", "foreign-root"),
            ("source_refs", {"source_message_id": "root", "source_turn_id": "foreign"}),
        ):
            with self.subTest(field=field):
                snapshot = self.provider_retry_snapshot()
                row = snapshot["messages"][1]
                payload = json.loads(row["payload_json"])
                payload[field] = value
                row["payload_json"] = json.dumps(payload)
                with self.assertRaises(AssertionError):
                    self.provider_retry_parking(snapshot)
        for field, value in (
            ("root_message_id", "foreign-root"), ("source_message_id", "foreign-root"),
            ("source_turn_id", "foreign-turn"), ("fallback_attempt", 2),
            ("source_terminal_kind", "runtime_error"),
        ):
            with self.subTest(recovery_field=field):
                snapshot = self.provider_retry_snapshot()
                row = snapshot["messages"][1]
                payload = json.loads(row["payload_json"])
                payload["metadata"]["provider_recovery"][field] = value
                row["payload_json"] = json.dumps(payload)
                with self.assertRaises(AssertionError):
                    self.provider_retry_parking(snapshot)
        for field in ("agent_id", "work_item_id"):
            with self.subTest(row_field=field):
                snapshot = self.provider_retry_snapshot()
                snapshot["messages"][1][field] = "foreign"
                with self.assertRaises(AssertionError):
                    self.provider_retry_parking(snapshot)

    def test_provider_retry_parking_rejects_claimed_terminal_or_fabricated_turn(self) -> None:
        for status in ("dequeued", "processed", "aborted", "interrupted"):
            with self.subTest(status=status):
                snapshot = self.provider_retry_snapshot()
                snapshot["queue_entries"][1]["status"] = status
                with self.assertRaises(AssertionError):
                    self.provider_retry_parking(snapshot)
        for evidence in ("queue_payload", "claim_audit", "turn", "attempt", "loop"):
            with self.subTest(evidence=evidence):
                snapshot = self.provider_retry_snapshot()
                if evidence == "queue_payload":
                    snapshot["queue_entries"][1]["payload_json"] = '{"status":"dequeued"}'
                elif evidence == "claim_audit":
                    row = copy.deepcopy(snapshot["audit_events"][1])
                    row["kind"] = "queue_entry_claimed"
                    snapshot["audit_events"].append(row)
                elif evidence == "turn":
                    row = copy.deepcopy(snapshot["turn_records"][0])
                    row.update(turn_id="turn-parked", trigger_message_id="parked")
                    snapshot["turn_records"].append(row)
                elif evidence == "attempt":
                    row = copy.deepcopy(snapshot["execution_protocol_attempts"][0])
                    payload = json.loads(row["payload_json"])
                    payload["source_message_id"] = "parked"
                    row["payload_json"] = json.dumps(payload)
                    snapshot["execution_protocol_attempts"].append(row)
                else:
                    row = copy.deepcopy(snapshot["messages"][1])
                    row["message_id"] = "another-recovery"
                    snapshot["messages"].append(row)
                with self.assertRaises(AssertionError):
                    self.provider_retry_parking(snapshot)

    def test_provider_retry_parking_requires_real_failure_and_cooldown(self) -> None:
        for index, field, value in (
            (0, "failure_kind", "authentication"), (0, "disposition", "terminal"),
            (0, "turn_id", "foreign-turn"), (1, "message_id", "foreign-message"),
            (1, "reason", "recovery_budget_exhausted"), (1, "untried_candidates", True),
            (1, "not_before", "2026-10-24T19:56:59+00:00"),
            (1, "request_count", 0),
        ):
            with self.subTest(field=field):
                snapshot = self.provider_retry_snapshot()
                row = snapshot["audit_events"][index]
                payload = json.loads(row["data_json"])
                payload["data"][field] = value
                row["data_json"] = json.dumps(payload)
                with self.assertRaises(AssertionError):
                    self.provider_retry_parking(snapshot)
        for evidence in ("runtime_error", "new_generation", "foreign_owner"):
            with self.subTest(evidence=evidence):
                snapshot = self.provider_retry_snapshot()
                if evidence == "runtime_error":
                    row = copy.deepcopy(snapshot["audit_events"][1])
                    row["kind"] = "runtime_error"
                    snapshot["audit_events"].append(row)
                elif evidence == "foreign_owner":
                    snapshot["turn_records"][0]["owner_id"] = "foreign"
                else:
                    row = snapshot["messages"][0]
                    payload = json.loads(row["payload_json"])
                    payload["metadata"]["work_queue"]["idempotency_key"] = (
                        "work_queue:continue_active:work-1:1:generation:2"
                    )
                    row["payload_json"] = json.dumps(payload)
                with self.assertRaises(AssertionError):
                    self.provider_retry_parking(snapshot)

    def test_provider_retry_recovery_processes_exact_parking_source(self) -> None:
        parking = self.provider_retry_parking(self.provider_retry_snapshot())
        self.assertEqual(
            runner.require_provider_retry_recovery(
                self.provider_retry_snapshot(recovered=True), agent_id="main",
                work_item_id="work-1", parking=parking,
            ),
            "turn-parked",
        )

    def test_provider_retry_recovery_rejects_replacement_foreign_root_or_owner(self) -> None:
        parking = self.provider_retry_parking(self.provider_retry_snapshot())
        for evidence in ("replacement", "foreign_root", "foreign_owner", "unprocessed"):
            with self.subTest(evidence=evidence):
                snapshot = self.provider_retry_snapshot(recovered=True)
                if evidence == "replacement":
                    snapshot["messages"][1]["message_id"] = "replacement"
                elif evidence == "unprocessed":
                    snapshot["queue_entries"][1]["status"] = "queued"
                else:
                    row = snapshot["execution_protocol_attempts"][1]
                    payload = json.loads(row["payload_json"])
                    if evidence == "foreign_root":
                        payload["recovery_of_attempt_id"] = "foreign-attempt"
                    else:
                        payload["binding"]["work_item_id"] = "foreign-work"
                    row["payload_json"] = json.dumps(payload)
                with self.assertRaises(AssertionError):
                    runner.require_provider_retry_recovery(
                        snapshot, agent_id="main", work_item_id="work-1", parking=parking,
                    )

    def test_provider_retry_case_restores_endpoint_and_scopes_real_tool_assertions(self) -> None:
        case = next(
            case for case in self.manifest["cases"]
            if case["id"] == "scheduler-provider-failure-work-queue-retry"
        )
        for provider_mode in ("stub", "live"):
            with self.subTest(provider_mode=provider_mode), tempfile.TemporaryDirectory() as directory:
                harness = Mock(spec=runner.CaseHarness)
                harness.agent_id = "main"
                harness.model = "openai/gpt-5.4"
                harness.provider_mode = provider_mode
                harness.runtime_env = {"HOLON_OPENAI_BASE_URL": "http://healthy/v1"}
                harness.stub_provider_base_url = "http://healthy-stub/v1"
                harness.timeout_seconds = 1
                harness.evidence = Path(directory)
                harness.tool_assertion_mode = "strict"
                harness.agent_path.side_effect = lambda path, **kwargs: path
                harness.request.side_effect = lambda method, path, data: (
                    {"id": "work-1"} if path == "work-items" else {}
                )
                endpoints = []
                harness.start.side_effect = lambda **kwargs: endpoints.append(
                    harness.stub_provider_base_url if provider_mode == "stub"
                    else harness.runtime_env["HOLON_OPENAI_BASE_URL"]
                )
                harness.event_cursor.return_value = 0
                harness.events.return_value = [
                    {"type": "message_admitted", "payload": {
                        "message_id": mid, "work_item_id": "work-1",
                    }} for mid in ("root", "parked")
                ] + [
                    {"type": "provider_queue_parked", "payload": {
                        "message_id": "parked", "reason": "chain_cooling",
                    }},
                    {"type": "turn_started", "payload": {
                        "turn_id": "turn-parked", "turn_index": 2, "message_id": "parked",
                    }},
                ] + [
                    {"type": "tool_executed", "payload": {
                        "turn_id": "turn-parked", "tool_name": tool, "status": "success",
                    }} for tool in ("ListWorkItems", "CompleteWorkItem")
                ]
                harness.runtime_db_snapshot.side_effect = [
                    self.provider_retry_snapshot(),
                    self.provider_retry_snapshot(recovered=True),
                ]
                harness.wait_work_item.return_value = {"result_brief_id": "brief-result"}
                harness.brief.return_value = {
                    "text": "SCHEDULER-PROVIDER-RETRY-COMPLETE-12345678",
                    "work_item_id": "work-1", "related_message_id": "parked",
                    "turn_id": "turn-parked",
                }
                # Exercise the actual scope contract, including mutually exclusive selectors.
                harness.successful_tool_events.side_effect = (
                    lambda *args, **kwargs: runner.CaseHarness.successful_tool_events(
                        harness, *args, **kwargs
                    )
                )
                harness.assert_tools = Mock(side_effect=(
                    lambda *args, **kwargs: runner.CaseHarness.assert_tools(
                        harness, *args, **kwargs
                    )
                ))
                with patch.object(runner.secrets, "token_hex", return_value="12345678"):
                    runner.run_scheduler_provider_failure_retry_case(harness, case)
                healthy = (
                    "http://healthy-stub/v1" if provider_mode == "stub" else "http://healthy/v1"
                )
                self.assertEqual(endpoints, [healthy, "http://127.0.0.1:9", healthy])
                harness.assert_tools.assert_called_once_with(
                    "provider-retry-tools", 0, ["ListWorkItems", "CompleteWorkItem"],
                    ["CreateWorkItem", "WaitFor"], message_id="parked",
                )
                calls = harness.method_calls
                failed_snapshot = next(
                    i for i, call in enumerate(calls)
                    if call == unittest.mock.call.runtime_db_snapshot("provider-retry-failed")
                )
                self.assertEqual(calls[failed_snapshot + 1], unittest.mock.call.stop())
                self.assertEqual(
                    [
                        call for call in calls
                        if call == unittest.mock.call.request(
                            "POST", "control", {"action": "stop"}
                        )
                    ],
                    [unittest.mock.call.request("POST", "control", {"action": "stop"})],
                    "agent stop is only safe before creating the recovery source",
                )
                self.assertTrue((Path(directory) / "provider-retry-parking.json").is_file())

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
        self.assertIn("Require dispatched live canary success", nightly)
        self.assertIn('test "${LIVE_CANARY_OUTCOME}" = success', nightly)
        self.assertIn("HOLON_E2E_PROVIDER_CONFIG_FILE:", nightly)
        self.assertNotIn("HOLON_E2E_CONFIG_FILE:", nightly)
        for workflow in (ci, nightly, release):
            live_step = workflow.split("      - name: Run scheduler live canary\n", 1)[
                1
            ].split("      - name:", 1)[0]
            self.assertNotIn("HOLON_E2E_PROVIDER_CONFIG_FILE", live_step)
            self.assertIn("config_file=/tmp/holon-e2e-config.json", live_step)
        nightly_job_env = nightly.split("  nightly:\n", 1)[1].split("    steps:\n", 1)[
            0
        ]
        release_job_env = release.split("  core:\n", 1)[1].split("    steps:\n", 1)[0]
        for job_env in (nightly_job_env, release_job_env):
            self.assertNotIn("HOLON_E2E_PROVIDER_ENV_FILE", job_env)
            self.assertNotIn("HOLON_E2E_PROVIDER_CONFIG_FILE", job_env)

    def test_release_workflow_fails_closed_for_previous_release_inputs(self) -> None:
        release = (ROOT / ".github/workflows/release-e2e.yml").read_text()
        inputs = release.split("    inputs:\n", 1)[1].split("\npermissions:", 1)[0]
        prepare = release.split("      - name: Prepare previous release image\n", 1)[
            1
        ].split("      - name:", 1)[0]

        self.assertNotIn("default: v0.30.0", inputs)
        self.assertIn(
            "previous_image and previous_ref are mutually exclusive",
            prepare,
        )
        self.assertIn(
            'if ! docker manifest inspect "${PREVIOUS_IMAGE_INPUT}"',
            prepare,
        )
        self.assertIn(
            "previous_image is not a resolvable container image",
            prepare,
        )
        self.assertIn(
            "one of previous_image or previous_ref is required",
            prepare,
        )
        self.assertIn("source=previous_image", prepare)
        self.assertIn("source=previous_ref", prepare)
        self.assertIn('"previous_release": {', release)
        self.assertIn('"image": os.environ["PREVIOUS_IMAGE"]', release)
        self.assertIn('"source": os.environ["PREVIOUS_SOURCE"]', release)
        self.assertIn('"ref": os.environ["PREVIOUS_REF"] or None', release)
        self.assertIn('"runtime-upgrade-interrupted-schema47"', release)
        self.assertIn('"interrupted_schema47_upgrade": {', release)
        self.assertIn('"evidence_sha256": recovery_sha256', release)
        self.assertIn(
            'recovery["previous_schema_revision"]\n'
            '              != old_snapshot.get("schema_revision")',
            release,
        )
        self.assertIn(
            'recovery["candidate_schema_revision"]\n'
            '              != migrated_snapshot.get("schema_revision")',
            release,
        )
        self.assertIn('recovery["candidate_schema_revision"] < 47', release)
        self.assertIn(
            'recovery["candidate_schema_revision"]\n'
            '              < recovery["previous_schema_revision"]',
            release,
        )
        self.assertIn(
            'recovery.get("old_snapshot_sha256") != old_snapshot_sha256',
            release,
        )
        self.assertIn(
            'recovery.get("migrated_snapshot_sha256")',
            release,
        )
        self.assertIn("!= migrated_snapshot_sha256", release)
        self.assertIn(
            'not report_before.get("before", {}).get("blockers")',
            release,
        )
        self.assertIn('not apply.get("apply", {}).get("changed")', release)
        self.assertIn('apply.get("after", {}).get("blockers")', release)
        self.assertIn('report_after.get("before", {}).get("blockers")', release)
        self.assertIn('report_after.get("reports")', release)
        publish = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertIn(".previous_release.image", publish)
        self.assertIn(
            '.previous_release.source == "previous_image"',
            publish,
        )
        self.assertIn(
            '.previous_release.source == "previous_ref"',
            publish,
        )

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
        self.assertTrue(all(case["timeout_seconds"] <= 120 for case in selected))

    def test_manifest_rejects_live_canary_with_strict_tools(self) -> None:
        invalid = json.loads(json.dumps(self.manifest))
        invalid["profiles"]["scheduler-live-canary"]["tool_assertion_mode"] = "strict"
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
                    "id": "scheduler-external-wait-resume-canonical",
                    "base_id": "scheduler-external-wait-resume",
                    "scheduler_engine": "canonical",
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
            ],
            scheduler_acceptance_status="pass",
            scheduler_coverage_status="pass",
            secret_scan_status="pass",
        )
        self.assertEqual(report["status"], "pass")
        self.assertEqual(report["model_route"], "provider/model")
        self.assertEqual(report["provider_rounds"], 2)
        self.assertEqual(report["provider_attempts"], 3)
        self.assertEqual(report["provider_retries"], 1)
        self.assertEqual(
            report["behavioral_variances"],
            [
                {
                    "case_id": "scheduler-external-wait-resume-canonical",
                    "scope": "scheduler-external-resume",
                    "missing_tools": ["GetWorkItem"],
                    "forbidden_tools_used": [],
                }
            ],
        )

    def test_collect_case_metrics_deduplicates_overlapping_event_snapshots(
        self,
    ) -> None:
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
            (evidence / "first-events.json").write_text(json.dumps({"events": events}))
            (evidence / "second-events.json").write_text(json.dumps({"events": events}))

            metrics = runner.collect_case_metrics(evidence)

        self.assertEqual(metrics["tool_counts"], {"WaitFor": 1})
        self.assertEqual(metrics["provider_rounds"], 1)
        self.assertEqual(metrics["provider_attempts"], 2)
        self.assertEqual(metrics["provider_retries"], 1)
        self.assertEqual(metrics["token_usage"]["total_tokens"], 5)

    def test_collect_case_schema_revision_uses_declared_result_snapshot(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            (evidence / "old-runtime-db.json").write_text(
                json.dumps({"schema_revision": 46})
            )
            (evidence / "migrated-runtime-db.json").write_text(
                json.dumps({"schema_revision": 47})
            )

            revision = runner.collect_case_schema_revision(evidence, "migrated")

        self.assertEqual(revision, 47)

    def test_collect_case_schema_revision_requires_declared_result_snapshot(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)

            with self.assertRaisesRegex(
                AssertionError,
                "result schema revision snapshot is missing",
            ):
                runner.collect_case_schema_revision(evidence, "migrated")

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
            {
                "input": [
                    {
                        "type": "message",
                        "content": [
                            {
                                "type": "input_text",
                                "text": "SCHEDULER-EXTERNAL-WAIT-abcd SCHEDULER-EXTERNAL-COMPLETE-abcd docker-e2e:abcd",
                            }
                        ],
                    }
                ]
            }
        )
        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["name"], "CreateWorkItem")
        status, response = scenario.consume(
            {
                "input": [
                    {
                        "type": "function_call_output",
                        "output": '{"work_item":{"id":"work_0123456789abcde"}}',
                    }
                ]
            }
        )
        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["name"], "PickWorkItem")
        scenario.phase = 5
        status, response = scenario.consume({"input": []})
        self.assertEqual(status, 200)
        self.assertEqual(response["output"][1]["name"], "CompleteWorkItem")
        self.assertEqual(scenario.phase, 6)
        self.assertTrue(scenario.status()["complete"])
        status, response = scenario.consume({"input": []})
        self.assertEqual(status, 409)
        self.assertEqual(response["error"]["type"], "transcript_exhausted")
        self.assertEqual(scenario.status()["extra_requests"], 1)
        self.assertFalse(scenario.status()["complete"])

    def test_openai_stub_answers_default_bootstrap_intro_without_phase(self) -> None:
        scenario = stub.Scenario("scheduler-task-wait")
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
                                    "[trigger:internal_followup]"
                                    "[runtime_instruction][InternalFollowup]\n"
                                    "  This agent was just created without an "
                                    "initial message. Perform a one-time "
                                    "initialization turn."
                                ),
                            }
                        ],
                    }
                ]
            }
        )
        self.assertEqual(status, 200)
        self.assertEqual(
            response["output"][0]["content"][0]["text"],
            "Agent bootstrap initialization complete.",
        )
        self.assertEqual(response["id"], "resp_agent_bootstrap_intro")
        self.assertEqual(scenario.phase, 0)

    def test_openai_stub_reads_each_live_batch_item_not_history_or_tool_output(self) -> None:
        runtime = {
            "kind": "system_tick",
            "authority_class": "runtime_instruction",
            "origin": {"kind": "system", "subsystem": "work_queue"},
        }
        request = stub_batch_request(
            (runtime, "Resume queued work."),
            ({}, "Operator request.\nKeep both lines."),
            recent_turns="This agent was just created without an initial message.",
        )
        old_request = stub_batch_request(({}, "Old input must not be read."))
        request["input"].insert(0, old_request["input"][0])
        request["input"].append({
            "type": "function_call_output", "output": "This is the first run of Holon",
        })
        # Reply-routing instructions occur between provenance and representation.
        text = request["input"][1]["content"][0]["text"]
        request["input"][1]["content"][0]["text"] = text.replace(
            "  representation: full\n",
            "Runtime message contract:\nReply expected: yes.\n  representation: full\n",
            1,
        )

        items = stub.Scenario.current_input_items(request)

        self.assertEqual([item["body"] for item in items], [
            "Resume queued work.", "Operator request.\nKeep both lines.",
        ])
        for key, value in runtime.items():
            self.assertEqual(items[0]["provenance"][key], value)
        self.assertEqual(items[1]["provenance"]["authority_class"], "operator_instruction")
        self.assertEqual(
            stub.Scenario.current_input(request),
            "Resume queued work.\nOperator request.\nKeep both lines.",
        )

    def test_openai_stub_batch_bootstrap_does_not_advance_any_scheduler_phase(self) -> None:
        for body, subsystem, response_id in [
            (
                "This agent was just created without an initial message. "
                "Perform a one-time initialization turn.",
                "spawn_agent", "resp_agent_bootstrap_intro",
            ),
            (
                "This is the first run of Holon with a model provider configured.",
                "first_run_intro", "resp_intro",
            ),
        ]:
            request = stub_batch_request(({
                "kind": "internal_followup",
                "authority_class": "runtime_instruction",
                "origin": {"kind": "system", "subsystem": subsystem},
                "delivery_surface": "runtime_system",
                "admission_context": "runtime_owned",
            }, body))
            for name in stub.SCENARIOS:
                if not name.startswith("scheduler-"):
                    continue
                with self.subTest(scenario=name, subsystem=subsystem):
                    scenario = stub.Scenario(name)
                    status, response = scenario.consume(request)
                    self.assertEqual(status, 200)
                    self.assertEqual(response["id"], response_id)
                    self.assertEqual(scenario.phase, 0)
                    self.assertEqual(scenario.extra_requests, 0)

    def test_openai_stub_batch_bootstrap_preserves_following_operator_marker(self) -> None:
        scenario = stub.Scenario("scheduler-concurrent")
        bootstrap = stub_batch_request(({
            "kind": "internal_followup",
            "authority_class": "runtime_instruction",
            "origin": {"kind": "system", "subsystem": "spawn_agent"},
        }, "This agent was just created without an initial message."))
        scenario.consume(bootstrap)
        marker = "SCHEDULER-CONCURRENT-A-8c360d95"

        status, response = scenario.consume(stub_batch_request((
            {}, f"Scheduler Docker E2E case: create objective {marker}.",
        )))

        self.assertEqual(status, 200)
        self.assertEqual(response["output"][0]["name"], "CreateWorkItem")
        self.assertEqual(json.loads(response["output"][0]["arguments"])["objective"], marker)
        self.assertEqual(scenario.phase, 1)

    def test_openai_stub_batch_bootstrap_requires_own_runtime_provenance(self) -> None:
        runtime = {
            "kind": "internal_followup",
            "authority_class": "runtime_instruction",
            "origin": {"kind": "system", "subsystem": "spawn_agent"},
        }
        body = (
            "This agent was just created without an initial message.\n"
            "[trigger:internal_followup][runtime_instruction][InternalFollowup]"
        )
        variants = [
            [({**runtime, "kind": "operator_prompt"}, body)],
            [({**runtime, "authority_class": "external_evidence"}, body)],
            [({**runtime, "origin": {"kind": "external"}}, body)],
            [(runtime, "Unrelated runtime instruction."), ({}, body)],
            [(runtime, body), ({}, "Create a real WorkItem.")],
        ]
        for items in variants:
            with self.subTest(items=items):
                scenario = stub.Scenario("scheduler-external")
                status, response = scenario.consume(stub_batch_request(*items))
                self.assertEqual(status, 200)
                self.assertEqual(response["output"][0]["name"], "CreateWorkItem")
                self.assertEqual(scenario.phase, 1)

    def test_openai_stub_batch_ticks_require_kind_authority_and_same_item_marker(self) -> None:
        runtime = {
            "kind": "system_tick",
            "authority_class": "runtime_instruction",
            "origin": {"kind": "system", "subsystem": "work_queue"},
        }
        for name, phase, marker_key, marker, tool in [
            ("scheduler-multi", 7, "multi_b", "SCHEDULER-MULTI-B-abcd", "GetWorkspaceState"),
            ("scheduler-concurrent", 6, "concurrent_a", "SCHEDULER-CONCURRENT-A-abcd", "GetWorkItem"),
        ]:
            quoted = f"[trigger:system_tick] {marker}"
            variants = [
                ([(runtime, marker)], True),
                ([({**runtime, "kind": "operator_prompt"}, quoted)], False),
                ([({**runtime, "authority_class": "external_evidence"}, quoted)], False),
                ([({**runtime, "origin": {"kind": "external"}}, quoted)], False),
                ([(runtime, "Other work is available."), ({}, quoted)], False),
                ([], False),
            ]
            for items, resumes in variants:
                with self.subTest(scenario=name, items=items):
                    scenario = stub.Scenario(name)
                    scenario.phase = phase
                    scenario.work_ids = ["work_0123456789abcde", "work_fedcba987654321"]
                    scenario.markers = {marker_key: marker}
                    request = stub_batch_request(*items, recent_turns=quoted)

                    status, response = scenario.consume(request)

                    self.assertEqual(status, 200)
                    if resumes:
                        self.assertEqual(response["output"][0]["name"], tool)
                        self.assertEqual(scenario.phase, 8)
                    else:
                        self.assertEqual(response["output"][0]["type"], "message")
                        self.assertEqual(scenario.phase, 7)

    def test_openai_stub_upgrade_v030_echoes_current_marker_not_history(self) -> None:
        scenario = stub.Scenario("runtime-upgrade-v030")
        old_prompt = {
            "instructions": "agent instructions",
            "input": [
                {
                    "type": "message",
                    "content": [
                        {
                            "type": "input_text",
                            "text": (
                                "## current_input\nCurrent input:\n"
                                "- [operator] Reply with the exact marker "
                                "UPGRADE-V030-OLD-0011aabb."
                            ),
                        }
                    ],
                }
            ],
        }
        status, response = scenario.consume(old_prompt)
        self.assertEqual(status, 200)
        self.assertEqual(
            response["output"][0]["content"][0]["text"],
            "UPGRADE-V030-OLD-0011aabb",
        )
        self.assertEqual(scenario.phase, 1)

        new_prompt = {
            "instructions": "agent instructions",
            "input": [
                {
                    "type": "message",
                    "content": [
                        {
                            "type": "input_text",
                            "text": (
                                "## recent_turns\n"
                                "- [operator] Reply with the exact marker "
                                "UPGRADE-V030-OLD-0011aabb.\n"
                                "- [assistant] UPGRADE-V030-OLD-0011aabb\n"
                                "## current_input\nCurrent input:\n"
                                "- [operator] Reply with the exact marker "
                                "UPGRADE-V030-NEW-0022ccdd."
                            ),
                        }
                    ],
                }
            ],
        }
        status, response = scenario.consume(new_prompt)
        self.assertEqual(status, 200)
        self.assertEqual(
            response["output"][0]["content"][0]["text"],
            "UPGRADE-V030-NEW-0022ccdd",
        )
        self.assertEqual(scenario.phase, 2)

        status, response = scenario.consume(new_prompt)
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
                                    "- [system][runtime_system][runtime_owned]"
                                    "[trigger:system_tick][runtime_instruction][SystemTick] "
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
                                "- [system][runtime_system][runtime_owned]"
                                "[trigger:system_tick][runtime_instruction][SystemTick] "
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

    def test_openai_stub_interject_resumes_a_immediately_after_b_completion(self) -> None:
        scenario = stub.Scenario("scheduler-interject")
        scenario.phase = 7
        scenario.work_ids = [
            "work_0123456789abcde",
            "work_fedcba987654321",
        ]
        scenario.markers = {
            "interject_complete_a": "SCHEDULER-INTERJECT-COMPLETE-A-abcd",
            "interject_complete_b": "SCHEDULER-INTERJECT-COMPLETE-B-abcd",
        }

        status, response = scenario.consume({"input": []})

        self.assertEqual(status, 200)
        self.assertEqual(
            response["output"][0]["content"][0]["text"],
            scenario.markers["interject_complete_b"],
        )
        self.assertEqual(response["output"][-1]["name"], "CompleteWorkItem")
        self.assertEqual(
            json.loads(response["output"][-1]["arguments"]),
            {"work_item_id": scenario.work_ids[1]},
        )
        self.assertEqual(scenario.phase, 8)

        for phase, name in enumerate(
            ("GetWorkItem", "UpdateWorkItem", "CompleteWorkItem"), start=8
        ):
            with self.subTest(phase=phase, name=name):
                status, response = scenario.consume({"input": []})
                self.assertEqual(status, 200)
                self.assertEqual(
                    [item["type"] for item in response["output"]],
                    ["message", "function_call"]
                    if name == "CompleteWorkItem" else ["function_call"],
                )
                call = response["output"][-1]
                self.assertEqual(call["name"], name)
                args = json.loads(call["arguments"])
                self.assertEqual(args["work_item_id"], scenario.work_ids[0])
                if name == "GetWorkItem":
                    self.assertTrue(args["include_todo_list"])
                elif name == "UpdateWorkItem":
                    self.assertEqual(args["todo_list"], [
                        {"text": "interject-wait", "state": "completed"},
                        {"text": "interject-complete", "state": "completed"},
                    ])
                else:
                    self.assertEqual(
                        response["output"][0]["content"][0]["text"],
                        scenario.markers["interject_complete_a"],
                    )
                self.assertEqual(scenario.phase, phase + 1)

        self.assertEqual(scenario.expected_phase(), 11)
        self.assertTrue(scenario.status()["complete"])
        status, response = scenario.consume({"input": []})
        self.assertEqual(status, 409)
        self.assertEqual(response["error"]["type"], "transcript_exhausted")
        self.assertEqual(scenario.phase, 11)
        self.assertEqual(scenario.extra_requests, 1)

    def test_openai_stub_registers_every_required_scenario(self) -> None:
        selected = [
            case for case in self.manifest["cases"] if case.get("stub_scenario")
        ]
        self.assertEqual(
            {case["stub_scenario"] for case in selected},
            set(stub.SCENARIOS),
        )
        for case in selected:
            scenario = stub.Scenario(case["stub_scenario"])
            if case["stub_scenario"] == "runtime-upgrade-interrupted-schema47":
                self.assertEqual(scenario.expected_phase(), 0)
            else:
                self.assertGreater(scenario.expected_phase(), 0)

    def test_interrupted_schema47_stub_attests_both_blocked_request_kinds(self) -> None:
        scenario = stub.Scenario("runtime-upgrade-interrupted-schema47")
        workers = []
        for marker in (
            "UPGRADE-SCHEMA47-LIFECYCLE-deadbeef",
            "UPGRADE-SCHEMA47-WORKITEM-deadbeef",
        ):
            request = {
                "input": [{"content": [{"text": marker}]}],
            }
            worker = threading.Thread(
                target=scenario.consume,
                args=(request,),
                daemon=True,
            )
            worker.start()
            workers.append(worker)
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline and not scenario.status()["complete"]:
            time.sleep(0.01)

        self.assertTrue(scenario.interrupted_schema47_seen)
        self.assertTrue(scenario.status()["complete"])

    def test_interrupted_schema47_stub_stalls_only_on_live_current_input(self) -> None:
        scenario = stub.Scenario("runtime-upgrade-interrupted-schema47")
        # #3025 keeps interrupted-turn operator input in serialized history,
        # so candidate recovery prompts replay old-phase markers through the
        # full payload while the live current_input carries only the fresh
        # candidate marker. Those requests must be answered, not stalled.
        replayed = stub_batch_request((
            {}, "candidate turn UPGRADE-SCHEMA47-CANDIDATE-ab12cd",
        ))
        old = stub_batch_request((
            {}, "old turn UPGRADE-SCHEMA47-LIFECYCLE-deadbeef "
            "UPGRADE-SCHEMA47-WORKITEM-deadbeef",
        ))
        replayed["input"].insert(0, old["input"][0])
        replayed_result: list[tuple[int, dict]] = []
        replayed_worker = threading.Thread(
            target=lambda: replayed_result.append(scenario.consume(replayed)),
            daemon=True,
        )
        replayed_worker.start()
        replayed_worker.join(timeout=2.0)
        self.assertFalse(replayed_worker.is_alive())
        self.assertEqual(replayed_result[0][0], 200)
        self.assertIn(
            "UPGRADE-SCHEMA47-CANDIDATE-ab12cd", json.dumps(replayed_result[0][1])
        )
        self.assertEqual(
            scenario.interrupted_schema47_kinds,
            {"agent_lifecycle", "work_item"},
        )
        self.assertTrue(scenario.status()["complete"])

        # Old-phase turns whose live current_input still carries the marker
        # must keep stalling so the runner can SIGKILL the runtime mid-turn.
        live = stub_batch_request(({}, "open turn UPGRADE-SCHEMA47-WORKITEM-live"))
        live_worker = threading.Thread(
            target=scenario.consume,
            args=(live,),
            daemon=True,
        )
        live_worker.start()
        live_worker.join(timeout=1.0)
        self.assertTrue(live_worker.is_alive())

    def test_openai_stub_required_scenarios_reach_exact_completion(self) -> None:
        expected_calls = {
            "scheduler-task-wait": [
                "CreateWorkItem",
                "ExecCommand",
                "WaitFor",
                "GetWorkItem",
                "WaitFor",
                "GetWorkItem",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-provider-retry": ["ListWorkItems", "CompleteWorkItem"],
            "scheduler-multi": [
                "CreateWorkItem",
                "CreateWorkItem",
                "GetAgent",
                "ListWorkItems",
                "UpdateWorkItem",
                "CompleteWorkItem",
                "GetWorkspaceState",
                "ListWorkItems",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-external": [
                "CreateWorkItem",
                "PickWorkItem",
                "WaitFor",
                "GetWorkItem",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-operator": [
                "CreateWorkItem",
                "PickWorkItem",
                "WaitFor",
                "GetWorkItem",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-concurrent": [
                "CreateWorkItem",
                "PickWorkItem",
                "WaitFor",
                "ListWorkItems",
                "UpdateWorkItem",
                "CompleteWorkItem",
                "GetWorkItem",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-interject": [
                "CreateWorkItem",
                "PickWorkItem",
                "WaitFor",
                "CreateWorkItem",
                "ListWorkItems",
                "UpdateWorkItem",
                "CompleteWorkItem",
                "GetWorkItem",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-compaction": [
                "CreateWorkItem",
                "PickWorkItem",
                "ExecCommand",
                "ExecCommand",
                "ExecCommand",
                "WaitFor",
                "GetWorkItem",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-worktree": [
                "CreateWorkItem",
                "PickWorkItem",
                "GetWorkspaceState",
                "CreateWorktree",
                "GetWorkspaceState",
                "SwitchWorkspace",
                "GetWorkspaceState",
                "RemoveWorktree",
                "GetWorkspaceState",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-spawn": [
                "CreateWorkItem",
                "PickWorkItem",
                "InvokeAgent",
                "TaskStatus",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
            "scheduler-checkpoint": [
                "CreateWorkItem",
                "CreateWorkItem",
                "ListWorkItems",
                "WaitFor",
                "ListWorkItems",
                "UpdateWorkItem",
                "CompleteWorkItem",
                "GetWorkItem",
                "UpdateWorkItem",
                "CompleteWorkItem",
            ],
        }
        transcript = (
            "Scheduler Docker E2E case "
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
        request = stub_batch_request(({
            "kind": "system_tick",
            "authority_class": "runtime_instruction",
            "origin": {"kind": "system", "subsystem": "work_queue"},
        }, transcript))
        bootstrap = stub_batch_request(({
            "kind": "internal_followup",
            "authority_class": "runtime_instruction",
            "origin": {"kind": "system", "subsystem": "spawn_agent"},
        }, "This agent was just created without an initial message."))
        for name, expected in expected_calls.items():
            scenario = stub.Scenario(name)
            status, value = scenario.consume(bootstrap)
            self.assertEqual(status, 200, name)
            self.assertEqual(value["id"], "resp_agent_bootstrap_intro", name)
            self.assertEqual(scenario.phase, 0, name)
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
                for item in value["output"]:
                    if item["type"] == "function_call" and item["name"] == "WaitFor":
                        self.assertEqual(
                            json.loads(item["arguments"])["delivery"], "silent", name
                        )
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
        value = {"url": "http://localhost/api/callbacks/wake/cb_secret-capability"}
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
                    harness.evidence / "scheduler-runtime-state-copy-failure.json"
                ).read_text()
            )
            self.assertEqual(failure["status"], "timeout")
            self.assertEqual(
                failure["timeout_seconds"],
                runner.RUNTIME_DB_COPY_TIMEOUT_SECONDS,
            )
            self.assertEqual(failure["stdout"], "partial")
            self.assertEqual(failure["stderr"], "daemon stalled")

    def test_offline_runtime_db_snapshot_can_write_host_evidence_directory(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="offline-snapshot-user-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            docker_args: tuple[str, ...] | None = None

            def capture_docker(
                *args: str, **_: object
            ) -> subprocess.CompletedProcess[str]:
                nonlocal docker_args
                docker_args = args
                raise RuntimeError("stop after command capture")

            harness.docker = capture_docker
            with self.assertRaisesRegex(RuntimeError, "command capture"):
                harness.offline_runtime_db_snapshot("scheduler")

            self.assertIsNotNone(docker_args)
            assert docker_args is not None
            user_index = docker_args.index("--user")
            self.assertEqual(docker_args[user_index + 1], "0:0")

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
                any("events?limit=300&order=asc&after_seq=12" in path for path in paths)
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

            def fake_docker(
                *args: str, **_: object
            ) -> subprocess.CompletedProcess[str]:
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

    def test_deferred_completion_requires_its_exact_committed_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="deferred-completion",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            event = {
                "type": "tool_executed",
                "payload": {
                    "status": "deferred",
                    "tool_name": "CompleteWorkItem",
                    "work_item_id": "work-1",
                    "input": {"work_item_id": "work-1"},
                    "turn_id": "turn-1",
                    "turn_index": 4,
                },
            }
            harness.events = lambda _: [event]
            item = {
                "id": "work-1",
                "state": "completed",
                "result_brief_id": "brief-1",
            }
            brief = {
                "work_item_id": "work-1",
                "turn_id": "turn-1",
                "text": "committed completion report",
            }
            harness.work_items = lambda _: [item]
            harness.brief = lambda *_: brief
            self.assertEqual(
                harness.assert_tools("complete", 3, ["CompleteWorkItem"]), [event]
            )
            for target, field, value in (
                (item, "state", "open"),
                (item, "result_brief_id", None),
                (brief, "work_item_id", "foreign-work"),
                (brief, "turn_id", "foreign-turn"),
                (brief, "text", " "),
                (event["payload"]["input"], "work_item_id", "foreign-work"),
                (event["payload"], "tool_name", "WaitFor"),
            ):
                with self.subTest(field=field, value=value):
                    previous = target[field]
                    target[field] = value
                    self.assertEqual(harness.successful_tool_events("complete", 3), [])
                    with self.assertRaisesRegex(AssertionError, "missing successful tools"):
                        harness.assert_tools("complete", 3, ["CompleteWorkItem"])
                    target[field] = previous

            for missing_turn_id in (None, ""):
                with self.subTest(missing_turn_id=missing_turn_id):
                    event["payload"]["turn_id"] = missing_turn_id
                    brief["turn_id"] = missing_turn_id
                    with self.assertRaisesRegex(AssertionError, "missing successful tools"):
                        harness.assert_tools("complete", 3, ["CompleteWorkItem"])
            event["payload"]["turn_id"] = brief["turn_id"] = "turn-1"
            for status in ("failed", "not_dispatched", "pending"):
                with self.subTest(status=status):
                    event["payload"]["status"] = status
                    self.assertEqual(harness.successful_tool_events("complete", 3), [])
            event["payload"]["status"] = "deferred"

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
        self.assertIn("ExecCommand", task_wait["phases"][0]["required_tools"])
        self.assertIn("WaitFor", task_wait["phases"][0]["required_tools"])
        self.assertNotIn("PickWorkItem", task_wait["phases"][0]["required_tools"])
        for case in selected:
            self.assertNotIn(
                "HOLON_SCHEDULER_ACCEPTANCE_FIXTURES",
                case.get("runtime_env", {}),
            )
            self.assertNotIn("HOLON_SCHEDULER", case.get("runtime_env", {}))

    def test_scheduler_interject_keeps_setup_conversation_outside_work_item(self) -> None:
        case = next(
            case
            for case in self.manifest["cases"]
            if case["id"] == "scheduler-operator-interject-during-wait"
        )
        item_a = {"id": "work-a", "state": "completed", "result_brief_id": "brief-a"}
        item_b = {"id": "work-b", "state": "completed", "result_brief_id": "brief-b"}
        with (
            patch.object(runner.secrets, "token_hex", return_value="marker"),
            patch.object(runner, "CaseHarness", autospec=True) as harness_type,
            patch.object(runner, "require_scheduler_engine_activation_chain") as chain,
            patch.object(
                runner, "require_scheduler_interject_setup",
                return_value=("turn-setup", "turn-resume"),
            ) as setup,
            patch.object(
                runner,
                "require_scheduler_wait_terminal",
                return_value=[{"wait_condition_id": "wait-a"}],
            ),
            patch.object(runner, "require_scheduler_engine_wait_resolution"),
            patch.object(runner, "require_execution_work_items_terminal"),
        ):
            harness = harness_type.return_value
            harness.agent_id = "default"
            harness.prompt.return_value = (0, {})
            harness.wait_work_item_scheduling_state.return_value = {"id": "work-a"}
            harness.wait_work_item.side_effect = [item_b, item_a]
            harness.prompt_scope.side_effect = lambda label: {"message_id": label}
            harness.brief.side_effect = [
                {
                    "work_item_id": f"work-{letter.lower()}",
                    "text": f"SCHEDULER-INTERJECT-COMPLETE-{letter}-marker",
                }
                for letter in ("A", "B")
            ]
            harness.runtime_db_snapshot.return_value = {"turn_records": []}
            harness.work_items.return_value = [item_a, item_b]

            runner.run_scheduler_operator_interject_during_wait_case(harness, case)

        self.assertEqual(setup.call_args.kwargs["work_item_id"], "work-a")
        self.assertEqual(
            setup.call_args.kwargs["create_message_id"], "scheduler-interject-create"
        )
        self.assertEqual(
            setup.call_args.kwargs["resume_message_id"], "scheduler-interject-resume"
        )
        checks = {
            call.args[0]: call for call in harness.assert_tools.call_args_list
        }
        self.assertEqual(
            checks["scheduler-interject-create"].args[2],
            ["CreateWorkItem", "PickWorkItem"],
        )
        self.assertEqual(
            checks["scheduler-interject-a-setup"].kwargs["turn_ids"], {"turn-setup"}
        )
        self.assertEqual(
            checks["scheduler-interject-a-resume"].kwargs["turn_ids"], {"turn-resume"}
        )
        self.assertIn("WaitFor", checks["scheduler-interject-a-resume"].args[3])
        self.assertEqual(chain.call_count, 1)
        self.assertEqual(chain.call_args.kwargs["work_item_id"], "work-b")
        self.assertEqual(
            chain.call_args.kwargs["expected_source_kinds"],
            ("work_item_continuation",),
        )

    def _interject_setup_fixture(self, autonomous: bool):
        setup_turn = "turn-setup" if autonomous else "turn-seed"
        snapshot = {
            "turn_records": [],
            "wait_conditions": [{
                "agent_id": "default", "work_item_id": "work-a",
                "wait_condition_id": "wait-a", "kind": "operator",
                "status": "resolved", "last_turn_id": setup_turn,
                "trigger_message_id": "message-resume",
            }],
            "execution_protocol_attempts": [],
            "execution_protocol_outcomes": [],
            "briefs": [{
                "agent_id": "default", "turn_id": setup_turn, "kind": "result",
                "payload_json": json.dumps({
                    "turn_id": setup_turn, "work_item_id": "work-a",
                    "text": "Waiting for operator input.",
                }),
            }],
        }

        def attempt(name, source, outcome, *, conversation=False):
            turn_id, message_id = f"turn-{name}", f"message-{name}"
            binding = (
                {"kind": "conversation", "interaction_id": "interaction-seed"}
                if conversation else {"kind": "work_item", "work_item_id": "work-a"}
            )
            snapshot["turn_records"].append({
                "agent_id": "default", "turn_id": turn_id,
                "turn_index": len(snapshot["turn_records"]) + 1,
                "owner_kind": binding["kind"],
                "owner_id": "interaction-seed" if conversation else "work-a",
                "current_work_item_id": None if conversation else "work-a",
                "trigger_message_id": message_id,
            })
            snapshot["execution_protocol_attempts"].append({
                "agent_id": "default", "attempt_id": name,
                "lifecycle_state": "settled", "terminal_outcome_id": f"outcome-{name}",
                "payload_json": json.dumps({
                    "attempt_id": name, "turn_id": turn_id,
                    "source_message_id": message_id, "source": {"identity": source},
                    "binding": binding,
                }),
            })
            snapshot["execution_protocol_outcomes"].append({
                "agent_id": "default", "outcome_id": f"outcome-{name}",
                "payload_json": json.dumps({"attempt_id": name, "outcome": outcome}),
            })

        attempt(
            "seed", {"kind": "queue_message", "message_id": "message-seed"},
            {"owner": "conversation", "outcome": (
                {"kind": "replied"} if autonomous else {
                    "kind": "handoff_to_work_item_wait", "work_item_id": "work-a",
                    "wait": {"wait_id": "wait-a"},
                }
            )}, conversation=True,
        )
        if autonomous:
            attempt(
                "setup", {"kind": "work_item_continuation", "work_item_id": "work-a"},
                {"owner": "work_item", "outcome": {
                    "kind": "wait", "wait": {"wait_id": "wait-a"},
                }},
            )
        attempt(
            "resume", {
                "kind": "triggered_wait", "wait_id": "wait-a",
                "trigger_message_id": "message-resume",
            },
            {"owner": "work_item", "outcome": {"kind": "complete", "completion": "done"}},
        )
        events = [
            {"type": "tool_executed", "event_seq": index, "payload": {
                "tool_name": name, "turn_id": "turn-seed",
                "status": "success", "work_item_id": "work-a",
                "input": {"work_item_id": "work-a"},
            }}
            for index, name in enumerate(["CreateWorkItem", "PickWorkItem"], 1)
        ]
        events.append({
            "type": "tool_executed", "event_seq": 3, "payload": {
                "tool_name": "WaitFor", "turn_id": setup_turn,
                "status": "deferred", "work_item_id": "work-a",
                "input": {"delivery": "final", "wake": "operator_input"},
            },
        })
        harness = type("InterjectHarness", (), {"agent_id": "default"})()
        harness.events = lambda _: events
        return harness, snapshot, events

    def test_interject_setup_accepts_both_exact_canonical_paths(self) -> None:
        for autonomous in (False, True):
            for status in ("success", "deferred"):
                with self.subTest(autonomous=autonomous, status=status):
                    harness, snapshot, events = self._interject_setup_fixture(autonomous)
                    events[-1]["payload"]["status"] = status
                    self.assertEqual(
                        runner.require_scheduler_interject_setup(
                            harness, snapshot, work_item_id="work-a",
                            create_message_id="message-seed",
                            resume_message_id="message-resume",
                        ),
                        ("turn-setup" if autonomous else "turn-seed", "turn-resume"),
                    )

    def test_interject_setup_rejects_unrelated_wait_owner_trigger_and_order(self) -> None:
        mutations = [
            ("wait", "last_turn_id", "turn-resume"),
            ("wait", "trigger_message_id", "message-b"),
            ("wait", "status", "active"),
            ("wait", "agent_id", "foreign"),
            ("setup-turn", "owner_id", "work-b"),
            ("resume-turn", "owner_id", "work-b"),
            ("resume-turn", "turn_index", 0),
            ("wait-event", "turn_id", "turn-b"),
            ("wait-event", "work_item_id", "work-b"),
            ("wait-event", "status", "failed"),
            ("wait-input", "work_item_id", "work-b"),
            ("wait-input", "wake", "external"),
            ("wait-input", "delivery", "silent"),
            ("pick-input", "work_item_id", "work-b"),
            ("wait-sequence", "event_seq", 1),
            ("brief", "turn_id", "turn-b"),
            ("brief", "work_item_id", "work-b"),
            ("brief", "text", " "),
        ]
        for autonomous in (False, True):
            for target, field, value in mutations:
                with self.subTest(autonomous=autonomous, target=target, field=field):
                    harness, snapshot, events = self._interject_setup_fixture(autonomous)
                    brief = json.loads(snapshot["briefs"][0]["payload_json"])
                    targets = {
                        "wait": snapshot["wait_conditions"][0],
                        "setup-turn": snapshot["turn_records"][-2],
                        "resume-turn": snapshot["turn_records"][-1],
                        "wait-event": events[-1]["payload"],
                        "wait-input": events[-1]["payload"]["input"],
                        "pick-input": events[1]["payload"]["input"],
                        "wait-sequence": events[-1],
                        "brief": brief,
                    }
                    targets[target][field] = value
                    snapshot["briefs"][0]["payload_json"] = json.dumps(brief)
                    with self.assertRaises(AssertionError):
                        runner.require_scheduler_interject_setup(
                            harness, snapshot, work_item_id="work-a",
                            create_message_id="message-seed",
                            resume_message_id="message-resume",
                        )

    def test_interject_setup_rejects_extra_sources_and_uncommitted_wait(self) -> None:
        for autonomous in (False, True):
            for fault in (
                "extra-attempt", "extra-wait", "extra-wait-tool", "missing-wait-tool",
                "missing-brief", "wrong-outcome", "wrong-resume-source",
                "wrong-setup-source", "wait-on-seed-instead-of-continuation",
            ):
                if not autonomous and fault == "wait-on-seed-instead-of-continuation":
                    continue
                with self.subTest(autonomous=autonomous, fault=fault):
                    harness, snapshot, events = self._interject_setup_fixture(autonomous)
                    if fault == "extra-attempt":
                        snapshot["execution_protocol_attempts"].append(
                            copy.deepcopy(snapshot["execution_protocol_attempts"][-1])
                        )
                    elif fault == "extra-wait":
                        snapshot["wait_conditions"].append(
                            copy.deepcopy(snapshot["wait_conditions"][0])
                        )
                    elif fault == "extra-wait-tool":
                        events.append(copy.deepcopy(events[-1]))
                    elif fault == "missing-wait-tool":
                        events.pop()
                    elif fault == "missing-brief":
                        snapshot["briefs"].clear()
                    elif fault == "wrong-outcome":
                        row = snapshot["execution_protocol_outcomes"][-2]
                        payload = json.loads(row["payload_json"])
                        payload["outcome"]["outcome"]["wait"]["wait_id"] = "wait-b"
                        row["payload_json"] = json.dumps(payload)
                    elif fault in {"wrong-resume-source", "wrong-setup-source"}:
                        row = snapshot["execution_protocol_attempts"][
                            -1 if fault == "wrong-resume-source" else -2
                        ]
                        payload = json.loads(row["payload_json"])
                        field = "wait_id" if fault == "wrong-resume-source" else (
                            "work_item_id" if autonomous else "message_id"
                        )
                        payload["source"]["identity"][field] = "foreign"
                        row["payload_json"] = json.dumps(payload)
                    else:
                        snapshot["wait_conditions"][0]["last_turn_id"] = "turn-seed"
                    with self.assertRaises(AssertionError):
                        runner.require_scheduler_interject_setup(
                            harness, snapshot, work_item_id="work-a",
                            create_message_id="message-seed",
                            resume_message_id="message-resume",
                        )

    def test_scheduler_multi_requires_marker_in_same_round_or_deferred_report(self) -> None:
        case = next(
            case
            for case in self.manifest["cases"]
            if case["id"] == "scheduler-multi-workitem-scheduling"
        )
        with patch.object(runner, "CaseHarness", autospec=True) as harness_type:
            harness = harness_type.return_value
            harness.prompt.side_effect = RuntimeError("stop after seed prompt capture")
            with self.assertRaisesRegex(RuntimeError, "seed prompt capture"):
                runner.run_scheduler_multi_workitem_case(harness, case)
            prompt = harness.prompt.call_args.args[1]
        for prefix in ("For the first, use objective ", "For the second, use objective "):
            objective, _ = json.JSONDecoder().raw_decode(prompt.split(prefix, 1)[1])
            self.assertIn(
                "same assistant response as CompleteWorkItem", objective
            )
            self.assertIn("requests a deferred final report, repeat", objective)
            self.assertIn("completion report containing the exact marker", objective)

    def test_scheduler_task_wait_crashes_daemon_before_task_result_rejoin(self) -> None:
        source = inspect.getsource(runner.run_scheduler_task_wait_resume_case)
        self.assertLess(
            source.index('expected_scheduling_state="waiting_task"'),
            source.index("harness.crash_restart(wait_idle=False)"),
        )
        self.assertLess(
            source.index("harness.crash_restart(wait_idle=False)"),
            source.index('expected_scheduling_state="waiting_external"'),
        )
        self.assertNotIn(
            'harness.work_items("scheduler-task-wait-after-restart")',
            source,
        )

    def test_scheduler_task_wait_prompt_expects_interruption_without_retry(self) -> None:
        case = next(
            case
            for case in self.manifest["cases"]
            if case["id"] == "scheduler-task-wait-resume"
        )
        with patch.object(runner, "CaseHarness", autospec=True) as harness_type:
            harness = harness_type.return_value
            harness.prompt.side_effect = RuntimeError("stop after seed prompt capture")
            with self.assertRaisesRegex(RuntimeError, "seed prompt capture"):
                runner.run_scheduler_task_wait_resume_case(harness, case)
            harness.prompt.assert_called_once()
            label, prompt = harness.prompt.call_args.args
        self.assertEqual(label, "scheduler-task-wait-seed")
        objective, _ = json.JSONDecoder().raw_decode(
            prompt.split("whose objective is ", 1)[1]
        )
        for instruction in (
            "intentional crash/restart acceptance drill",
            "Across the entire WorkItem lifecycle, only one ExecCommand call is allowed",
            "ExecCommand exactly once",
            "resource set to that exact task_id",
            "restart TaskResult saying the task was interrupted is the expected result",
            "command success and the stdout marker are not required",
            "Do not repeat or retry ExecCommand after restart",
            "do not run a replacement command",
            "Do not call TaskStatus or TaskOutput in any turn, including through CLI workarounds",
            "On that interrupted task-result rejoin, call GetWorkItem",
            "the current WorkItem first, then call WaitFor with wake=external",
            "Do not run diagnostic or recovery commands on that rejoin",
        ):
            with self.subTest(instruction=instruction):
                self.assertIn(instruction, objective)

    def test_scheduler_external_wait_seed_requests_silent_delivery(self) -> None:
        case = next(
            case
            for case in self.manifest["cases"]
            if case["id"] == "scheduler-external-wait-resume"
        )
        with patch.object(runner, "CaseHarness", autospec=True) as harness_type:
            harness = harness_type.return_value
            harness.prompt.side_effect = RuntimeError("stop after seed prompt capture")
            with self.assertRaisesRegex(RuntimeError, "seed prompt capture"):
                runner.run_scheduler_external_wait_resume_case(harness, case)
            harness.prompt.assert_called_once()
            label, prompt = harness.prompt.call_args.args
        self.assertEqual(label, "scheduler-external-wait")
        self.assertIn("wake=external", prompt)
        self.assertIn("delivery=silent", prompt)
        self.assertIn("tool-only setup wait", prompt)
        self.assertIn("do not request delivery=final or publish a waiting report", prompt)

    def test_crash_restart_uses_docker_sigkill(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="crash-restart-test",
                image="holon:test",
                model="deepseek/deepseek-v4-flash",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=False,
            )
            commands: list[tuple[str, ...]] = []

            def fake_docker(
                *args: str, **_: object
            ) -> subprocess.CompletedProcess[str]:
                commands.append(args)
                if args[:2] == ("inspect", "--format"):
                    return subprocess.CompletedProcess(
                        ["docker", *args], 0, "false\n", ""
                    )
                return subprocess.CompletedProcess(["docker", *args], 0, "", "")

            harness.docker = fake_docker
            harness.capture_logs = lambda: None
            harness.start = lambda *, wait_idle=True: commands.append(
                ("start", str(wait_idle))
            )

            harness.crash_restart(wait_idle=False)

            self.assertEqual(
                commands[0],
                (
                    "kill",
                    "--signal",
                    "KILL",
                    harness.container,
                ),
            )
            self.assertIn(("rm", "-f", harness.container), commands)
            self.assertEqual(commands[-1], ("start", "False"))

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
                ("runtime-agent-lifecycle", None),
                ("runtime-upgrade-v030", None),
                ("runtime-upgrade-interrupted-schema47", None),
                ("memory-agent-home-persistence", None),
                ("workspace-restart-lifecycle", None),
                ("workitem-wait-restart-complete", None),
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
            case for case in selected if case["id"] == "scheduler-compaction-continuity"
        )
        self.assertEqual(
            compaction["model_runtime_override"],
            {
                "prompt_budget_estimated_tokens": 80000,
                "compaction_trigger_estimated_tokens": 16000,
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
            ["canonical"],
        )

    def test_scheduler_acceptance_report_rejects_duplicate_canonical_coverage(
        self,
    ) -> None:
        run_record = {
            "git_sha": "abc123",
            "image": {"ref": "holon:test", "id": None, "repo_digests": []},
            "image_digest": None,
            "manifest_sha256": "manifest-hash",
        }
        case_results = [
            {
                "id": f"scheduler-task-wait-resume-canonical-{revision}",
                "base_id": "scheduler-task-wait-resume",
                "scheduler_engine": "canonical",
                "status": "pass",
                "schema_revision": revision,
            }
            for revision in (40, 40)
        ]
        report = runner.scheduler_acceptance_report(
            run_record=run_record,
            case_results=case_results,
            fixture_corpus_revision="scheduler-release-acceptance-v1",
            required_coverage_ids={"scheduler-task-wait-resume"},
        )
        self.assertEqual(report["status"], "fail")
        self.assertEqual(report["runtime_schema_revision"], 40)
        self.assertIn(
            "engine_case_matrix_incomplete",
            {diagnostic["code"] for diagnostic in report["diagnostics"]},
        )

    def test_scheduler_acceptance_report_distinguishes_missing_schema_evidence(
        self,
    ) -> None:
        run_record = {
            "git_sha": "abc123",
            "image": {"ref": "holon:test", "id": None, "repo_digests": []},
            "image_digest": None,
            "manifest_sha256": "manifest-hash",
        }
        case_results = [
            {
                "id": "scheduler-task-wait-resume-canonical",
                "base_id": "scheduler-task-wait-resume",
                "scheduler_engine": "canonical",
                "status": "fail",
                "schema_revision": None,
                "failure_kind": "case_timeout",
                "evidence_collection_error": "docker cp failed",
            }
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
            ["scheduler-task-wait-resume-canonical"],
        )
        self.assertEqual(
            diagnostic["case_timeouts"],
            ["scheduler-task-wait-resume-canonical"],
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

    def test_canonical_wait_resolution_requires_exact_resolved_owner(self) -> None:
        harness = type(
            "CanonicalHarness",
            (),
            {"canonical_scheduler_enabled": True, "agent_id": "default"},
        )()
        canonical_wait = {
            "wait_condition_id": "wait-1",
            "agent_id": "default",
            "work_item_id": "work-1",
            "status": "resolved",
        }
        snapshot = {"wait_conditions": [canonical_wait]}
        runner.require_scheduler_engine_wait_resolution(
            harness,
            snapshot,
            work_item_id="work-1",
            wait_ids={"wait-1"},
        )

        canonical_wait["work_item_id"] = "work-unexpected"
        with self.assertRaisesRegex(
            AssertionError, "canonical waits did not resolve exactly once"
        ):
            runner.require_scheduler_engine_wait_resolution(
                harness,
                snapshot,
                work_item_id="work-1",
                wait_ids={"wait-1"},
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

    def test_canonical_activation_oracle_distinguishes_conversation_and_work_item(
        self,
    ) -> None:
        harness = type(
            "CanonicalHarness",
            (),
            {
                "canonical_scheduler_enabled": True,
                "agent_id": "default",
            },
        )()
        snapshot = {
            "execution_protocol_attempts": [
                {
                    "agent_id": "default",
                    "attempt_id": "attempt-conversation",
                    "lifecycle_state": "settled",
                    "terminal_outcome_id": "outcome-conversation",
                    "payload_json": json.dumps(
                        {
                            "attempt_id": "attempt-conversation",
                            "source_message_id": "message-create",
                            "source": {
                                "identity": {
                                    "kind": "queue_message",
                                    "message_id": "message-create",
                                }
                            },
                            "binding": {
                                "kind": "conversation",
                                "interaction_id": "interaction-1",
                            },
                        }
                    ),
                },
                {
                    "agent_id": "default",
                    "attempt_id": "attempt-resume",
                    "lifecycle_state": "settled",
                    "terminal_outcome_id": "outcome-resume",
                    "payload_json": json.dumps(
                        {
                            "attempt_id": "attempt-resume",
                            "source_message_id": "message-resume",
                            "source": {
                                "identity": {
                                    "kind": "triggered_wait",
                                    "wait_id": "wait-1",
                                    "trigger_message_id": "message-resume",
                                }
                            },
                            "binding": {
                                "kind": "work_item",
                                "work_item_id": "work-1",
                            },
                        }
                    ),
                },
            ],
            "execution_protocol_outcomes": [
                {
                    "agent_id": "default",
                    "outcome_id": "outcome-conversation",
                    "payload_json": json.dumps({"attempt_id": "attempt-conversation"}),
                },
                {
                    "agent_id": "default",
                    "outcome_id": "outcome-resume",
                    "payload_json": json.dumps({"attempt_id": "attempt-resume"}),
                },
            ],
        }

        runner.require_scheduler_engine_activation_chain(
            harness,
            snapshot,
            work_item_id="work-1",
            expected_source_kinds=("triggered_wait",),
            conversation_message_ids={"message-create"},
        )

        conversation = json.loads(
            snapshot["execution_protocol_attempts"][0]["payload_json"]
        )
        conversation["binding"] = {"kind": "work_item", "work_item_id": "work-1"}
        snapshot["execution_protocol_attempts"][0]["payload_json"] = json.dumps(
            conversation
        )
        with self.assertRaisesRegex(
            AssertionError,
            "Conversation attempts did not settle without claiming a WorkItem",
        ):
            runner.require_scheduler_engine_activation_chain(
                harness,
                snapshot,
                work_item_id="work-1",
                expected_source_kinds=("triggered_wait",),
                conversation_message_ids={"message-create"},
            )

    def test_lifecycle_wait_adoption_accepts_exact_conversation_owner(self) -> None:
        snapshot = {
            "turn_records": [
                {
                    "agent_id": "default",
                    "turn_id": "turn-conversation",
                    "trigger_message_id": "message-conversation",
                    "owner_kind": "conversation",
                    "owner_id": "interaction-1",
                }
            ],
            "execution_protocol_attempts": [
                {
                    "agent_id": "default",
                    "attempt_id": "attempt-conversation",
                    "lifecycle_state": "settled",
                    "terminal_outcome_id": "outcome-conversation",
                    "payload_json": json.dumps(
                        {
                            "attempt_id": "attempt-conversation",
                            "source_message_id": "message-conversation",
                            "binding": {
                                "kind": "conversation",
                                "interaction_id": "interaction-1",
                            },
                        }
                    ),
                }
            ],
            "execution_protocol_outcomes": [
                {
                    "agent_id": "default",
                    "outcome_id": "outcome-conversation",
                    "payload_json": json.dumps(
                        {
                            "attempt_id": "attempt-conversation",
                            "outcome": {
                                "owner": "conversation",
                                "outcome": {
                                    "kind": "handoff_to_work_item_wait",
                                    "work_item_id": "work-1",
                                    "wait": {"wait_id": "wait-1"},
                                },
                            },
                        }
                    ),
                }
            ],
        }

        runner.require_lifecycle_wait_adoption(
            snapshot,
            agent_id="default",
            work_item_id="work-1",
            wait={
                "wait_condition_id": "wait-1",
                "last_turn_id": "turn-conversation",
            },
        )

    def test_turn_record_owner_columns_adapt_to_previous_release_schema(self) -> None:
        source = inspect.getsource(runner.CaseHarness.runtime_db_snapshot)
        self.assertIn("turn_record_owner_columns(connection)", source)
        with tempfile.TemporaryDirectory() as directory:
            database = str(Path(directory) / "runtime.sqlite")
            connection = sqlite3.connect(database)
            try:
                connection.execute(
                    "CREATE TABLE turn_records (turn_id TEXT PRIMARY KEY)"
                )
                connection.commit()
                self.assertEqual(runner.turn_record_owner_columns(connection), "")
                connection.execute(
                    "ALTER TABLE turn_records ADD COLUMN owner_kind TEXT"
                )
                connection.commit()
                self.assertEqual(runner.turn_record_owner_columns(connection), "")
                connection.execute(
                    "ALTER TABLE turn_records ADD COLUMN owner_id TEXT"
                )
                connection.commit()
                self.assertEqual(
                    runner.turn_record_owner_columns(connection),
                    "owner_kind, owner_id, ",
                )
            finally:
                connection.close()

    def test_external_wait_resume_drains_queue_before_runtime_snapshot(self) -> None:
        source = inspect.getsource(runner.run_scheduler_external_wait_resume_case)
        self.assertLess(
            source.index("harness.wait_queue_drained()"),
            source.index('harness.runtime_db_snapshot("scheduler-external")'),
        )

    def test_checkpoint_replay_tracks_wait_and_completion_continuations(self) -> None:
        source = inspect.getsource(runner.run_scheduler_checkpoint_replay_case)
        self.assertIn(
            'expected_source_kinds=(\n'
            '            "work_item_continuation",\n'
            '            "work_item_continuation",\n'
            '            "triggered_wait",\n'
            "        )",
            source,
        )

    def test_result_brief_waits_for_its_source_turn_to_reach_terminal(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness = runner.CaseHarness(
                case_id="terminal-turn-race",
                image="unused",
                model="unused",
                credential_envs=[],
                env_file=None,
                runtime_env={},
                evidence_root=Path(directory),
                timeout_seconds=1,
                keep=True,
            )
            events = [
                [
                    {
                        "type": "tool_execution_completed",
                        "payload": {"turn_id": "turn-source"},
                    }
                ],
                [
                    {
                        "type": "turn_terminal",
                        "payload": {
                            "turn_id": "turn-source",
                            "kind": "completed",
                        },
                    }
                ],
            ]
            with (
                patch.object(harness, "events", side_effect=events) as event_poll,
                patch.object(runner.time, "sleep"),
            ):
                source_turn_id = harness.wait_result_brief_terminal_turn(
                    brief={"turn_id": "turn-source"},
                    label="terminal-race",
                )

            self.assertEqual(source_turn_id, "turn-source")
            self.assertEqual(event_poll.call_count, 2)

    def test_multi_workitem_waits_for_terminal_turns_before_snapshot(self) -> None:
        source = inspect.getsource(runner.run_scheduler_multi_workitem_case)
        self.assertLess(
            source.index("harness.wait_result_brief_terminal_turn"),
            source.index('harness.runtime_db_snapshot("scheduler-multi")'),
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
            {
                "data": {
                    "compacted_rounds": 0,
                    "compacted_tool_results": 1,
                    "exact_tail_rounds": 3,
                }
            }
        )
        events = runner.require_turn_local_compaction(
            {"audit_events": [event]},
            label="test",
        )
        self.assertEqual(events[0]["compacted_tool_results"], 1)

        event["data_json"] = json.dumps(
            {
                "data": {
                    "compacted_rounds": 0,
                    "compacted_tool_results": 0,
                    "exact_tail_rounds": 3,
                }
            }
        )
        with self.assertRaisesRegex(
            AssertionError,
            "compaction stimulus did not compact rounds or tool results",
        ):
            runner.require_turn_local_compaction(
                {
                    "audit_events": [
                        event,
                        {
                            "kind": "provider_round_completed",
                            "data_json": json.dumps({"data": {"compression_epoch": 1}}),
                        },
                    ]
                },
                label="test",
            )

    def test_checkpoint_restart_lineage_binds_wait_generation(self) -> None:
        def attempt(
            attempt_id: str,
            message_id: str,
            generation: int,
            identity: dict[str, object],
        ) -> dict[str, object]:
            return {
                "attempt_id": attempt_id,
                "source_message_id": message_id,
                "source": {"identity": identity},
                "binding": {"kind": "work_item", "work_item_id": "work-1"},
                "admitted_fences": {"work_item_generation": generation},
            }

        before_restart_snapshot = {
            "execution_protocol_attempts": [
                {
                    "payload_json": json.dumps(
                        attempt(
                            "attempt-schedule",
                            "message-schedule",
                            1,
                            {
                                "kind": "work_item_continuation",
                                "work_item_id": "work-1",
                            },
                        )
                    ),
                }
            ]
        }
        snapshot = {
            "execution_protocol_attempts": [
                {
                    "payload_json": json.dumps(
                        attempt(
                            "attempt-schedule",
                            "message-schedule",
                            1,
                            {
                                "kind": "work_item_continuation",
                                "work_item_id": "work-1",
                            },
                        )
                    ),
                },
                {
                    "payload_json": json.dumps(
                        attempt(
                            "attempt-resume",
                            "message-resume",
                            2,
                            {
                                "kind": "triggered_wait",
                                "wait_id": "wait-1",
                                "trigger_message_id": "message-resume",
                            },
                        )
                    ),
                },
                {
                    "payload_json": json.dumps(
                        attempt(
                            "attempt-completion",
                            "message-completion",
                            3,
                            {
                                "kind": "work_item_continuation",
                                "work_item_id": "work-1",
                            },
                        )
                    ),
                },
            ],
            "wait_conditions": [
                {
                    "wait_condition_id": "wait-1",
                    "work_item_id": "work-1",
                    "status": "resolved",
                }
            ],
        }

        runner.require_checkpoint_restart_activation_lineage(
            before_restart_snapshot,
            snapshot,
            work_item_id="work-1",
            wait_id="wait-1",
        )

        resume = json.loads(snapshot["execution_protocol_attempts"][1]["payload_json"])
        resume["source"]["identity"]["wait_id"] = "other-wait"
        snapshot["execution_protocol_attempts"][1]["payload_json"] = json.dumps(resume)
        with self.assertRaisesRegex(
            AssertionError,
            "wait-resume attempt mismatch",
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

    def _route_oracle_harness(self, model: str) -> "runner.CaseHarness":
        harness = runner.CaseHarness(
            case_id="runtime-auth-model-delivery",
            image="unused",
            model=model,
            credential_envs=[],
            env_file=None,
            runtime_env={},
            evidence_root=Path(tempfile.mkdtemp()),
            timeout_seconds=1,
            keep=True,
        )
        harness.agent_id = "default"
        return harness

    def _models_payload(self) -> dict:
        return {
            "model_availability": [
                {
                    "model": "dashscope/qwen3.7-max",
                    "provider_family": "dashscope",
                    "endpoint": "token-plan",
                    "route_provider": "dashscope-token-plan",
                    "available": True,
                    "policy": {"model_ref": "dashscope/qwen3.7-max"},
                },
                {
                    "model": "dashscope/qwen3.7-max",
                    "provider_family": "dashscope",
                    "endpoint": "default",
                    "route_provider": "dashscope",
                    "available": False,
                    "unavailable_reason": "credential_missing",
                    "policy": {"model_ref": "dashscope/qwen3.7-max"},
                },
            ]
        }

    def test_runtime_model_route_accepts_confirmed_canonical_resolution(self) -> None:
        harness = self._route_oracle_harness("dashscope-token-plan/qwen-3.7")
        runner.require_runtime_model_route(
            harness,
            "runtime default",
            "dashscope@token-plan/qwen3.7-max",
            self._models_payload(),
        )

    def test_runtime_model_route_accepts_literal_match(self) -> None:
        harness = self._route_oracle_harness("openai/gpt-5.4")
        runner.require_runtime_model_route(
            harness,
            "winning",
            "openai/gpt-5.4",
            {"model_availability": []},
        )

    def test_runtime_model_route_rejects_unconfirmed_provider(self) -> None:
        harness = self._route_oracle_harness("dashscope-token-plan/qwen-3.7")
        with self.assertRaisesRegex(AssertionError, "model catalog did not confirm"):
            runner.require_runtime_model_route(
                harness,
                "runtime default",
                "deepseek@default/deepseek-chat",
                self._models_payload(),
            )

    def test_runtime_model_route_rejects_unavailable_canonical_route(self) -> None:
        harness = self._route_oracle_harness("dashscope-token-plan/qwen-3.7")
        payload = self._models_payload()
        payload["model_availability"][0]["available"] = False
        payload["model_availability"][0]["unavailable_reason"] = "credential_missing"
        with self.assertRaisesRegex(AssertionError, "not available"):
            runner.require_runtime_model_route(
                harness,
                "runtime default",
                "dashscope@token-plan/qwen3.7-max",
                payload,
            )


class LiveAcceptanceEvidenceTests(unittest.TestCase):
    def test_agent_tail_checks_bounds_scope_cursor_and_null_device(self) -> None:
        for case in ("good", "foreign-agent", "task-scope", "oversized", "null-error"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                calls = []
                end = {
                    "type": "end", "scope": "agent", "agent_id": "receiver",
                    "cursor": "confirmed-cursor", "stop_reason": "snapshot",
                }
                if case == "foreign-agent":
                    end["agent_id"] = "foreign"
                elif case == "task-scope":
                    end["task_id"] = "not-an-agent-observation"
                elif case == "oversized":
                    end["padding"] = "x" * 4096

                def docker(*args, **kwargs):
                    calls.append(args)
                    if len(calls) == 3:
                        return subprocess.CompletedProcess(
                            args, int(case == "null-error"), "", "EPERM"
                        )
                    return subprocess.CompletedProcess(args, 0, json.dumps(end) + "\n", "")

                harness = type("Harness", (), {
                    "evidence": Path(directory), "agent_id": "receiver",
                    "container": "isolated-runtime", "docker": staticmethod(docker),
                })()
                if case != "good":
                    with self.assertRaises(AssertionError):
                        runner.require_live_agent_tail(
                            harness, deadline=time.monotonic() + 10,
                        )
                    continue
                runner.require_live_agent_tail(harness, deadline=time.monotonic() + 10)
                self.assertEqual(len(calls), 3)
                self.assertEqual(calls[1][-3:], ("--follow", "--cursor", "confirmed-cursor"))
                self.assertIn(">/dev/null", calls[2][-3])

    def test_agent_tail_bounds_each_host_exec_by_remaining_deadline(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            end = {
                "type": "end", "scope": "agent", "agent_id": "receiver",
                "cursor": "cursor", "stop_reason": "timeout",
            }
            calls = []

            def docker(*args, **kwargs):
                calls.append(kwargs)
                return subprocess.CompletedProcess(args, 0, json.dumps(end) + "\n", "")

            harness = type("Harness", (), {
                "evidence": Path(directory), "agent_id": "receiver",
                "container": "isolated-runtime", "docker": staticmethod(docker),
            })()
            with patch.object(runner.time, "monotonic", side_effect=[100, 101, 102]):
                runner.require_live_agent_tail(harness, deadline=103)
            self.assertEqual([call["timeout"] for call in calls], [3, 2, 1])
            calls.clear()
            with patch.object(runner.time, "monotonic", return_value=100):
                runner.require_live_agent_tail(harness, deadline=1000)
            self.assertEqual(
                [call["timeout"] for call in calls],
                [runner.DOCKER_CONTROL_TIMEOUT_SECONDS] * 3,
            )
            for clock, completed_calls in (
                ([103], 0), ([100, 103], 1), ([100, 101, 103], 2),
            ):
                with self.subTest(clock=clock):
                    calls.clear()
                    with (
                        patch.object(runner.time, "monotonic", side_effect=clock),
                        self.assertRaisesRegex(TimeoutError, "observing agent tail"),
                    ):
                        runner.require_live_agent_tail(harness, deadline=103)
                    self.assertEqual(len(calls), completed_calls)

    def test_agent_tail_propagates_each_host_exec_timeout(self) -> None:
        for failed_call in (1, 2, 3):
            with self.subTest(failed_call=failed_call):
                with tempfile.TemporaryDirectory() as directory:
                    calls = []

                    def docker(*args, **kwargs):
                        calls.append(kwargs)
                        if len(calls) == failed_call:
                            raise subprocess.TimeoutExpired(args, kwargs["timeout"])
                        return subprocess.CompletedProcess(args, 0, json.dumps({
                            "type": "end", "scope": "agent", "agent_id": "receiver",
                            "cursor": "cursor", "stop_reason": "snapshot",
                        }) + "\n", "")

                    harness = type("Harness", (), {
                        "evidence": Path(directory), "agent_id": "receiver",
                        "container": "isolated-runtime", "docker": staticmethod(docker),
                    })()
                    with self.assertRaises(subprocess.TimeoutExpired):
                        runner.require_live_agent_tail(
                            harness, deadline=time.monotonic() + 10,
                        )
                    self.assertEqual(len(calls), failed_call)
                    self.assertTrue(all(0 < call["timeout"] <= 10 for call in calls))

    def test_live_snapshot_preserves_canonical_queue_table(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            state = evidence / "live-runtime-state"
            state.mkdir()
            with sqlite3.connect(state / "runtime.sqlite") as connection:
                for table in (
                    "tasks", "task_result_settlements", "agent_message_deliveries",
                    "transcript_entries", "queue_entries",
                ):
                    connection.execute(f"CREATE TABLE {table} (message_id TEXT)")
            queues = [{"message_id": "original", "status": "queued"}]
            harness = type("Harness", (), {
                "evidence": evidence,
                "runtime_db_snapshot": lambda self, label: {"queue_entries": queues},
            })()
            snapshot = runner.live_snapshot(harness, "live")
            self.assertEqual(snapshot["queue_entries"], queues)
            self.assertNotIn("queue_entries_current", snapshot)
            self.assertTrue((evidence / "live-live-db.json").is_file())

    def test_live_profile_is_opt_in_and_does_not_expand_required_stub_gate(self) -> None:
        manifest = json.loads((ROOT / "tests/e2e/docker/manifest.json").read_text())
        runner.validate_manifest(manifest)
        profile = runner.resolve_profile(manifest, "output-input-live-acceptance")
        self.assertEqual(profile["provider_mode"], "live")
        self.assertEqual(profile["tool_assertion_mode"], "observe")
        self.assertEqual(len(profile["case_ids"]), 2)
        required = manifest["profiles"]["scheduler-required"]["case_ids"]
        for case in manifest["cases"]:
            if case["id"] not in profile["case_ids"]:
                continue
            self.assertNotIn(case["id"], required)
            self.assertEqual(case["tier"], "extended")
            self.assertEqual(case["timeout_seconds"], 480)
            self.assertIn("TaskOutput", case["phases"][0]["forbidden_tools"])

    def test_bounded_command_reader_uses_confirmed_path_and_finite_reads(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "confirmed output ' file.log"
            path.write_bytes(b"x" * 524288)
            result = subprocess.run(
                ["bash", "-lc", runner.bounded_command_read(str(path))],
                capture_output=True, text=True, check=True,
            )
            self.assertEqual(json.loads(result.stdout), {
                "path": str(path), "size": 524288,
                "head_bytes": 1024, "tail_bytes": 1024, "eof": True,
            })
            self.assertLess(len(result.stdout), 512)

    def test_live_command_requires_final_waiting_report_before_gate_release(self) -> None:
        manifest = json.loads((ROOT / "tests/e2e/docker/manifest.json").read_text())
        case = next(
            case for case in manifest["cases"]
            if case["id"] == "live-command-output-reentry"
        )
        self.assertEqual(len(case["phases"]), 1)
        prompt = case["phases"][0]["prompt"].format(
            command="original-command", read_command="bounded-reader",
            waiting_marker="COMMAND-WAITING-test",
            completion_marker="COMMAND-REENTERED-test",
        )
        self.assertIn("delivery=final", prompt)
        self.assertNotIn("delivery=silent", prompt)
        self.assertIn("awaiting_final_report", prompt)
        self.assertIn("only the unique waiting marker COMMAND-WAITING-test", prompt)
        self.assertIn("Do not call any tools in that pending-report continuation", prompt)
        source = inspect.getsource(runner.run_live_command_observation_case)
        self.assertIn('waiting_marker = f"COMMAND-WAITING-{marker}"', source)
        self.assertIn("waiting_marker=waiting_marker", source)
        self.assertIn('detail.get("status") == "success"', source)
        self.assertIn('wait_detail["input"].get("delivery") == "final"', source)
        self.assertLess(
            source.index("require_live_command_waiting_report("),
            source.index("gate.touch()"),
        )

    def command_waiting_snapshot(self) -> dict:
        brief = {
            "id": "waiting-brief", "agent_id": "parent",
            "turn_id": "parent-turn", "kind": "result",
            "text": "COMMAND-WAITING-test",
        }
        return {
            "wait_conditions": [{
                "agent_id": "parent", "subject_ref": "task-1",
                "status": "active", "kind": "task",
                "last_turn_id": "parent-turn", "work_item_id": None,
            }],
            "task_result_settlements": [],
            "briefs": [{
                "evidence_id": brief["id"], "agent_id": brief["agent_id"],
                "turn_id": brief["turn_id"], "kind": brief["kind"],
                "payload_json": json.dumps(brief),
            }],
        }

    def test_command_waiting_requires_real_unique_parent_result_brief(self) -> None:
        good = self.command_waiting_snapshot()
        args = {
            "agent_id": "parent", "task_id": "task-1",
            "parent_turn_id": "parent-turn", "waiting_marker": "COMMAND-WAITING-test",
        }
        runner.require_live_command_waiting_report(good, **args)
        for case in (
            "missing", "duplicate", "wrong-turn", "payload-wrong-turn",
            "row-wrong-turn", "wrong-marker", "empty", "ack", "foreign-agent",
            "wrong-id", "extra-prose",
        ):
            with self.subTest(case=case):
                broken = copy.deepcopy(good)
                row = broken["briefs"][0]
                brief = json.loads(row["payload_json"])
                if case == "missing":
                    broken["briefs"] = []
                elif case == "duplicate":
                    broken["briefs"] *= 2
                elif case == "wrong-turn":
                    row["turn_id"] = brief["turn_id"] = "reentry-turn"
                elif case == "payload-wrong-turn":
                    brief["turn_id"] = "reentry-turn"
                elif case == "row-wrong-turn":
                    row["turn_id"] = "reentry-turn"
                elif case == "wrong-marker":
                    brief["text"] = "COMMAND-WAITING-other"
                elif case == "empty":
                    brief["text"] = " "
                elif case == "ack":
                    row["kind"] = brief["kind"] = "ack"
                elif case == "foreign-agent":
                    row["agent_id"] = brief["agent_id"] = "foreign"
                elif case == "wrong-id":
                    brief["id"] = "foreign-brief"
                elif case == "extra-prose":
                    brief["text"] += " not the exact waiting report"
                row["payload_json"] = json.dumps(brief)
                with self.assertRaisesRegex(AssertionError, "waiting Result brief"):
                    runner.require_live_command_waiting_report(broken, **args)
        with self.assertRaisesRegex(AssertionError, "non-empty marker"):
            runner.require_live_command_waiting_report(
                good, **{**args, "waiting_marker": " "},
            )

    def test_command_waiting_brief_does_not_replace_active_exact_wait(self) -> None:
        for key, value in (
            ("status", "resolved"), ("kind", "external"),
            ("last_turn_id", "foreign-turn"), ("subject_ref", "foreign-task"),
            ("agent_id", "foreign"), ("work_item_id", "migrated-owner"),
        ):
            with self.subTest(key=key):
                broken = self.command_waiting_snapshot()
                broken["wait_conditions"][0][key] = value
                with self.assertRaisesRegex(AssertionError, "exact wait"):
                    runner.require_live_command_waiting_report(
                        broken, agent_id="parent", task_id="task-1",
                        parent_turn_id="parent-turn", waiting_marker="COMMAND-WAITING-test",
                    )
        for case in ("missing-wait", "duplicate-wait", "early-settlement"):
            with self.subTest(case=case):
                broken = self.command_waiting_snapshot()
                if case == "missing-wait":
                    broken["wait_conditions"] = []
                elif case == "duplicate-wait":
                    broken["wait_conditions"] *= 2
                else:
                    broken["task_result_settlements"] = [{"task_id": "task-1"}]
                with self.assertRaisesRegex(AssertionError, "exact wait"):
                    runner.require_live_command_waiting_report(
                        broken, agent_id="parent", task_id="task-1",
                        parent_turn_id="parent-turn", waiting_marker="COMMAND-WAITING-test",
                    )

    def test_large_output_stimulus_closes_pipes_without_finishing_task(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            gate = Path(directory) / "release"
            process = subprocess.Popen(
                ["bash", "-lc", runner.gated_output_command(str(gate))],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            try:
                with self.assertRaises(subprocess.TimeoutExpired):
                    process.communicate(timeout=1)
                self.assertTrue(process.stdout.closed)
                self.assertTrue(process.stderr.closed)
                self.assertIsNone(process.poll(), "EOF incorrectly ended the stimulus")
            finally:
                gate.touch()
                try:
                    output, error = process.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    output, error = process.communicate()
            self.assertEqual(process.returncode, 0)
            self.assertEqual(len(output), 524288)
            self.assertEqual(error, b"")

    def task_snapshot(self) -> dict:
        return {
            "task_result_settlements": [{
                "task_id": "task-1", "agent_id": "parent",
                "state": "settled", "disposition": "model_delivered",
                "parent_turn_id": "parent-turn", "message_id": "result-1",
                "work_item_id": None, "result_identity": "result-identity-1",
                "rejoin_generation": 1,
            }],
            "wait_conditions": [{
                "agent_id": "parent", "subject_ref": "task-1",
                "status": "resolved", "last_turn_id": "parent-turn",
                "trigger_message_id": "result-1", "work_item_id": None,
            }],
            "messages": [{
                "message_id": "result-1", "payload_json": json.dumps({
                    "kind": "task_result",
                    "origin": {"kind": "task", "task_id": "task-1"},
                    "metadata": {"task_id": "task-1"},
                }),
            }],
            "turn_records": [{
                "agent_id": "parent", "trigger_message_id": "result-1",
                "turn_id": "reentry-turn", "terminal_kind": "completed",
            }],
            "tasks": [{
                "task_id": "task-1",
                "payload_json": json.dumps({
                    "id": "task-1", "status": "completed", "detail": {
                        "rejoin_obligation_id": "task-1", "rejoin_generation": 1,
                        "parent_turn_id": "parent-turn",
                    },
                }),
            }],
        }

    def test_task_reentry_requires_identity_real_turn_settlement_and_stable_owner(self) -> None:
        good = self.task_snapshot()
        runner.require_live_task_reentry(
            good, agent_id="parent", task_id="task-1", parent_turn_id="parent-turn"
        )
        mutations = [
            ("task_result_settlements", 0, "state", "persisted_pending"),
            ("task_result_settlements", 0, "parent_turn_id", "foreign-turn"),
            ("wait_conditions", 0, "trigger_message_id", "foreign-result"),
            ("turn_records", 0, "turn_id", "parent-turn"),
            ("turn_records", 0, "terminal_kind", "interrupted"),
            ("task_result_settlements", 0, "work_item_id", "migrated"),
            ("task_result_settlements", 0, "rejoin_generation", 2),
        ]
        for table, index, key, value in mutations:
            with self.subTest(table=table, key=key):
                broken = copy.deepcopy(good)
                broken[table][index][key] = value
                with self.assertRaises(AssertionError):
                    runner.require_live_task_reentry(
                        broken, agent_id="parent", task_id="task-1",
                        parent_turn_id="parent-turn",
                    )
        duplicated = copy.deepcopy(good)
        duplicated["task_result_settlements"] *= 2
        with self.assertRaisesRegex(AssertionError, "exactly once"):
            runner.require_live_task_reentry(
                duplicated, agent_id="parent", task_id="task-1",
                parent_turn_id="parent-turn",
            )

    def input_snapshot(self) -> dict:
        ids = ["input-1", "input-2", "input-3", "input-4"]
        snapshot = {
            "messages": [], "transcript_entries": [], "queue_entries": [],
            "agent_message_deliveries": [],
        }
        items = []
        for index, message_id in enumerate(ids):
            original = {
                "id": message_id, "message_seq": index + 1,
                "authority_class": "runtime_instruction",
                "body": {
                    "type": "text",
                    "text": f"BATCH-test-{index + 1}: correction value={index + 1}",
                },
                "routing_context": {
                    "sender_agent_id": "sender", "recipient_agent_id": "receiver",
                    "correlation_id": f"correlation-{index}",
                },
            }
            provenance = {**original, "source_refs": {}}
            items.append({
                "message_id": message_id, "source_ref": f"message:{message_id}",
                "rendered": f"  provenance: {json.dumps(provenance)}\n"
                f"  body:\n{original['body']['text']}\n",
            })
            snapshot["messages"].append({
                "message_id": message_id, "agent_id": "receiver",
                "payload_json": json.dumps(original),
            })
            snapshot["messages"].append({
                "message_id": f"reply-{index}", "agent_id": "sender",
                "payload_json": json.dumps({
                    "body": {"type": "text", "text": f"ACK BATCH-test-{index + 1}"},
                    "routing_context": {
                        "in_reply_to_message_id": message_id,
                        "sender_agent_id": "receiver", "recipient_agent_id": "sender",
                        "correlation_id": f"correlation-{index}",
                    },
                }),
            })
            snapshot["transcript_entries"].append({
                "message_id": message_id, "agent_id": "receiver",
                "turn_id": "batch-turn", "kind": "incoming_message",
            })
            snapshot["agent_message_deliveries"].append({
                "message_id": message_id, "state": "consumed",
            })
            snapshot["queue_entries"].append({
                "message_id": message_id,
                "status": "processed" if index == 0 else "interjected",
            })
        turn = {
            "turn_id": "batch-turn", "input_message_ids": ids,
            "trigger": {"message_id": ids[0]},
            "idle_input_batch": {
                "activation_id": "activation-1",
                "sender_agent_id": "sender", "items": items[:2], "rendered_bytes": 1000,
            },
            "live_input_batches": [{
                "activation_id": "activation-1",
                "items": items[2:], "rendered_bytes": 1000,
                "safe_point": {"provider_round": 2, "activation_id": "activation-1"},
            }],
        }
        snapshot["turn_records"] = [{
            "agent_id": "receiver", "trigger_message_id": ids[0],
            "payload_json": json.dumps(turn),
        }]
        return snapshot

    def input_replies(self) -> list[dict]:
        return [
            {"input": {
                "agent_id": "sender", "reply_context_id": f"input-{index}",
                "message": f"ACK BATCH-test-{index}",
            }}
            for index in range(1, 5)
        ]

    def check_batch(self, snapshot: dict, reply_details=None) -> None:
        runner.require_live_input_batch(
            snapshot, recipient="receiver", sender="sender",
            message_ids=["input-1", "input-2", "input-3", "input-4"],
            reply_details=self.input_replies() if reply_details is None else reply_details,
        )

    def test_input_batch_rejects_wrong_or_misrouted_ack_bodies(self) -> None:
        good = self.input_snapshot()
        for index in range(4):
            for text in ("", "ACK", "ACK BATCH-foreign-1", f"ACK BATCH-test-{(index + 1) % 4 + 1}"):
                for source in ("tool", "canonical", "both"):
                    with self.subTest(index=index, text=text, source=source):
                        snapshot = copy.deepcopy(good)
                        details = self.input_replies()
                        if source in ("tool", "both"):
                            details[index]["input"]["message"] = text
                        if source in ("canonical", "both"):
                            row = snapshot["messages"][index * 2 + 1]
                            reply = json.loads(row["payload_json"])
                            reply["body"]["text"] = text
                            row["payload_json"] = json.dumps(reply)
                        with self.assertRaisesRegex(AssertionError, "acknowledge original"):
                            self.check_batch(snapshot, details)
        for key, value in (
            ("reply_context_id", "input-2"),
            ("agent_id", "foreign-sender"),
        ):
            with self.subTest(key=key):
                details = self.input_replies()
                details[0]["input"][key] = value
                with self.assertRaisesRegex(AssertionError, "receive order"):
                    self.check_batch(good, details)

    def test_input_batch_checks_originals_live_boundary_consumption_and_routes(self) -> None:
        good = self.input_snapshot()
        self.check_batch(good)
        for mutation in ("order", "no-live", "provenance", "budget", "body"):
            with self.subTest(mutation=mutation):
                broken = copy.deepcopy(good)
                turn = json.loads(broken["turn_records"][0]["payload_json"])
                if mutation == "order":
                    turn["input_message_ids"].reverse()
                elif mutation == "no-live":
                    turn["live_input_batches"] = []
                elif mutation == "provenance":
                    turn["idle_input_batch"]["items"][0]["rendered"] = (
                        '  provenance: {"id":"forged"}\ncorrection 0\n'
                    )
                elif mutation == "budget":
                    turn["live_input_batches"][0]["rendered_bytes"] = 98305
                else:
                    turn["idle_input_batch"]["items"][0]["rendered"] += "lost correction"
                broken["turn_records"][0]["payload_json"] = json.dumps(turn)
                with self.assertRaises(AssertionError):
                    self.check_batch(broken)
        for table in ("transcript_entries", "agent_message_deliveries", "queue_entries"):
            with self.subTest(duplicate=table):
                broken = copy.deepcopy(good)
                broken[table].append(broken[table][0])
                with self.assertRaisesRegex(AssertionError, "exactly once"):
                    self.check_batch(broken)
        broken = copy.deepcopy(good)
        reply = json.loads(broken["messages"][1]["payload_json"])
        reply["routing_context"]["sender_agent_id"] = "wrong-agent"
        broken["messages"][1]["payload_json"] = json.dumps(reply)
        with self.assertRaisesRegex(AssertionError, "reply route"):
            self.check_batch(broken)

    def test_only_canonical_anchor_may_have_an_ingress_transcript_without_turn(self) -> None:
        good = self.input_snapshot()
        good["transcript_entries"][0]["turn_id"] = None
        self.check_batch(good)
        for index, value in ((0, "foreign-turn"), (1, None), (2, None)):
            with self.subTest(index=index, value=value):
                broken = copy.deepcopy(good)
                broken["transcript_entries"][index]["turn_id"] = value
                with self.assertRaisesRegex(AssertionError, "exactly once"):
                    self.check_batch(broken)
        broken = copy.deepcopy(good)
        broken["queue_entries"][1]["status"] = "processed"
        with self.assertRaisesRegex(AssertionError, "exactly once"):
            self.check_batch(broken)
        broken = copy.deepcopy(good)
        turn = json.loads(broken["turn_records"][0]["payload_json"])
        turn["trigger"]["message_id"] = "foreign-anchor"
        broken["turn_records"][0]["payload_json"] = json.dumps(turn)
        with self.assertRaises(AssertionError):
            self.check_batch(broken)

    def test_observe_mode_does_not_allow_retired_task_output(self) -> None:
        harness = type("Harness", (), {
            "event_batch": lambda *a, **kw: {"events": [{
                "type": "tool_executed", "payload": {"tool_name": "TaskOutput"},
            }]},
        })()
        with self.assertRaisesRegex(AssertionError, "forbidden TaskOutput"):
            runner.live_events(harness, 0)


if __name__ == "__main__":
    unittest.main()
