#!/usr/bin/env python3

import json
import stat
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from scripts.docker_e2e import scheduler_drill as drill


class SchedulerDrillTests(unittest.TestCase):
    def test_seed_wait_prepares_dynamic_resource_after_work_item_creation(self) -> None:
        events: list[str] = []
        harness = Mock()
        harness.prompt.side_effect = lambda label, *_args, **_kwargs: (
            events.append(f"prompt:{label}") or (17, {})
        )
        harness.wait_work_item.side_effect = lambda **_kwargs: (
            events.append("work-item-created")
            or {"id": "work_timer", "state": "open"}
        )
        harness.work_items.return_value = [
            {
                "id": "work_timer",
                "objective": "DRILL-WAIT-timer-marker",
                "state": "open",
            }
        ]
        harness.wait_work_item_scheduling_state.return_value = {
            "id": "work_timer",
            "scheduling_state": "waiting_timer",
        }

        seed = drill.seed_wait(
            harness,
            label="timer",
            marker="marker",
            wake="timer",
            prepare_resource=lambda: events.append("timer-created") or "timer_1",
            expected_scheduling_state="waiting_timer",
        )

        self.assertEqual(
            events,
            [
                "prompt:timer-create",
                "work-item-created",
                "timer-created",
                "prompt:timer-seed",
            ],
        )
        self.assertEqual(seed["work_item_id"], "work_timer")
        seed_prompt = harness.prompt.call_args_list[1].args[1]
        self.assertIn('resource="timer_1"', seed_prompt)

    def test_drill_prefix_is_a_scoped_operator_request(self) -> None:
        self.assertIn("current operator turn", drill.DRILL_PREFIX)
        self.assertNotIn("OVERRIDES", drill.DRILL_PREFIX)
        self.assertNotIn("system prompt", drill.DRILL_PREFIX)

    def test_default_models_match_candidate_route(self) -> None:
        args = drill.parse_args(["preflight", "--skip-build"])
        if args.fallback_model is None:
            args.fallback_model = list(drill.DEFAULT_FALLBACK_MODELS)
        self.assertEqual(args.primary_model, "volcengine@plan/glm-5.2")
        self.assertEqual(
            args.fallback_model,
            ["dashscope@token-plan/qwen3.8-max-preview"],
        )

    def test_control_token_is_separate_and_private(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            previous = drill.os.environ.get("HOLON_DRILL_SECRET_ROOT")
            drill.os.environ["HOLON_DRILL_SECRET_ROOT"] = directory
            try:
                drill.write_control_token("drill-test", "secret-token")
                path = drill.token_path("drill-test")
                self.assertEqual(drill.read_control_token("drill-test"), "secret-token")
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
                drill.delete_control_token("drill-test")
                self.assertFalse(path.exists())
            finally:
                if previous is None:
                    drill.os.environ.pop("HOLON_DRILL_SECRET_ROOT", None)
                else:
                    drill.os.environ["HOLON_DRILL_SECRET_ROOT"] = previous

    def test_evidence_summary_requires_all_scenarios_and_clean_tail(self) -> None:
        evidence = {
            "protocol_config": [
                {
                    "protocol_mode": "authoritative",
                    "config_revision": 1,
                }
            ],
            "scenario_authorities": [],
            "hard_blockers": [],
            "work_demands": [],
            "activations": [
                {
                    "activation_id": "activation-1",
                    "lifecycle_state": "settled",
                }
            ],
            "settlements": [
                {
                    "activation_id": "activation-1",
                    "payload_json": "{}",
                }
            ],
            "missing_settlements": [],
            "slots": [{"slot_kind": "idle"}],
            "wait_generations": [{"lifecycle_state": "resolved"}],
            "briefs": [],
            "operator_deliveries": [],
            "protocol_conflicts": [],
            "incomplete_turns": [],
            "queue_status": [{"status": "processed", "count": 8}],
        }
        stress = {
            "scenario_completed": {
                scenario: 1 for scenario in drill.PRODUCTION_SCENARIOS
            },
            "scenario_shortfalls": {},
            "failed_count": 0,
            "latest_phase_status": "completed",
            "injection_shortfalls": {},
            "missing_required_injections": [],
        }
        summary = drill.evidence_summary(evidence, stress=stress)
        self.assertEqual(summary["status"], "go")
        self.assertTrue(all(summary["checks"].values()))

        evidence["activations"][0]["lifecycle_state"] = "running"
        summary = drill.evidence_summary(evidence, stress=stress)
        self.assertEqual(summary["status"], "no-go")
        self.assertFalse(summary["checks"]["no_active_activation"])

    def test_report_is_external_evidence_only(self) -> None:
        evidence = {
            "schema_revision": 33,
            "hard_blockers": [],
            "missing_settlements": [],
            "protocol_conflicts": [],
            "incomplete_turns": [],
        }
        summary = {
            "status": "go",
            "scenario_counts": {
                scenario: 1 for scenario in drill.PRODUCTION_SCENARIOS
            },
            "checks": {"all_scenarios_observed": True},
            "current_hard_blockers": [],
            "active_activations": [],
            "occupied_slots": [],
            "active_waits": [],
            "needs_settlement": [],
            "settlement_inconsistencies": [],
            "delivery_inconsistencies": [],
            "queue_tail": [],
        }
        report = drill.render_report(
            {"drill_run_id": "drill-test", "last_mode": "shadow"},
            "shadow-final",
            evidence,
            summary,
        )
        self.assertIn("Decision: **GO**", report)
        self.assertIn("not imported into the runtime", report)

    def test_run_record_does_not_need_credential_path_or_value(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            paths = drill.DrillPaths.from_root(Path(directory))
            record = {
                "schema_version": drill.RUN_SCHEMA_VERSION,
                "credential_env_names": list(drill.REQUIRED_CREDENTIAL_ENVS),
                "env_file_used": True,
            }
            drill.save_record(paths, record)
            serialized = json.dumps(json.loads(paths.run_json.read_text()))
            self.assertNotIn("VOLCENGINE_SECRET", serialized)
            self.assertNotIn("/credentials/", serialized)

    def test_stress_plan_expands_iterations_and_is_reproducible(self) -> None:
        arguments = {
            "scenarios": [
                "reducer_only_candidates",
                "exact_wait_resume",
                "explicitly_bound_operator_input",
            ],
            "iterations": 4,
            "concurrency": 3,
            "duplicate_ratio": 0.25,
            "stale_ratio": 0.5,
            "seed": "drill-test-seed",
        }
        first = drill.build_stress_plan(**arguments)
        second = drill.build_stress_plan(**arguments)

        self.assertEqual(first, second)
        self.assertEqual(len(first), 12)
        self.assertEqual(
            [operation.worker for operation in first],
            [index % 3 for index in range(12)],
        )
        self.assertEqual(sum(operation.duplicate for operation in first), 1)
        self.assertEqual(sum(operation.fault is not None for operation in first), 6)
        self.assertEqual(
            {operation.fault for operation in first if operation.fault},
            {"stale", "out_of_order", "wrong_fence"},
        )
        self.assertFalse(
            any(operation.duplicate and operation.fault for operation in first)
        )

    def test_stress_plan_rejects_overlapping_duplicate_and_fault_slots(self) -> None:
        with self.assertRaisesRegex(
            AssertionError,
            "independent operations for duplicate and fault injections",
        ):
            drill.build_stress_plan(
                scenarios=["exact_wait_resume"],
                iterations=1,
                concurrency=1,
                duplicate_ratio=1.0,
                stale_ratio=1.0,
                seed="overlap-test",
            )

    def test_stress_executor_respects_worker_bound_and_collects_failures(self) -> None:
        plan = drill.build_stress_plan(
            scenarios=["reducer_only_candidates"],
            iterations=8,
            concurrency=3,
            duplicate_ratio=0.0,
            stale_ratio=0.0,
            seed="executor-test",
        )
        lock = threading.Lock()
        active = 0
        peak = 0
        seen_by_worker: dict[int, list[int]] = {}

        def run_operation(operation: drill.StressOperation) -> dict[str, int]:
            nonlocal active, peak
            with lock:
                active += 1
                peak = max(peak, active)
                seen_by_worker.setdefault(operation.worker, []).append(
                    operation.sequence
                )
            time.sleep(0.01)
            with lock:
                active -= 1
            if operation.sequence == 4:
                raise RuntimeError("injected failure")
            return {"sequence": operation.sequence}

        results = drill.execute_stress_plan(
            plan,
            concurrency=3,
            run_operation=run_operation,
        )

        self.assertLessEqual(peak, 3)
        self.assertGreaterEqual(peak, 2)
        self.assertEqual(len(results), len(plan))
        self.assertEqual(
            [result["sequence"] for result in results],
            list(range(len(plan))),
        )
        self.assertEqual(results[4]["status"], "failed")
        self.assertIn("injected failure", results[4]["error"])
        for sequences in seen_by_worker.values():
            self.assertEqual(sequences, sorted(sequences))

    def test_stress_executor_aborts_remaining_work_after_docker_breaker(self) -> None:
        plan = drill.build_stress_plan(
            scenarios=["reducer_only_candidates"],
            iterations=6,
            concurrency=1,
            duplicate_ratio=0.0,
            stale_ratio=0.0,
            seed="breaker-test",
        )

        def run_operation(operation: drill.StressOperation) -> None:
            if operation.sequence == 1:
                raise drill.DockerCircuitBreakerOpen("daemon unavailable")

        results = drill.execute_stress_plan(
            plan,
            concurrency=1,
            run_operation=run_operation,
        )

        self.assertEqual(results[0]["status"], "completed")
        self.assertEqual(results[1]["status"], "failed")
        self.assertTrue(
            all(result["status"] == "aborted" for result in results[2:])
        )

    def test_docker_engine_identity_requires_native_engine(self) -> None:
        native = {
            "server_version": "29.0.2",
            "driver": "overlay2",
            "docker_root_dir": "/var/lib/docker",
            "operating_system": "Ubuntu",
        }
        with (
            patch.dict(
                drill.os.environ,
                {"DOCKER_HOST": drill.DEFAULT_NATIVE_DOCKER_HOST},
                clear=False,
            ),
            patch.object(
                drill,
                "run",
                return_value=subprocess.CompletedProcess(
                    ["docker", "info"],
                    0,
                    json.dumps(native),
                    "",
                ),
            ),
        ):
            self.assertEqual(
                drill.docker_engine_identity(),
                {
                    **native,
                    "docker_host": drill.DEFAULT_NATIVE_DOCKER_HOST,
                },
            )

        with patch.dict(
            drill.os.environ,
            {"DOCKER_HOST": "unix:///tmp/docker-desktop.sock"},
            clear=False,
        ):
            with self.assertRaisesRegex(AssertionError, "requires native"):
                drill.docker_engine_identity()

    def test_stress_summary_counts_only_executed_injections(self) -> None:
        plan = [
            drill.StressOperation(
                sequence=0,
                iteration=1,
                worker=0,
                scenario="reducer_only_candidates",
                marker="marker-a",
                duplicate=True,
                fault="out_of_order",
            ),
            drill.StressOperation(
                sequence=1,
                iteration=1,
                worker=1,
                scenario="exact_wait_resume",
                marker="marker-b",
                duplicate=False,
                fault="stale",
            ),
        ]
        results = [
            {
                **plan[0].as_dict(),
                "status": "completed",
                "detail": {
                    "injections": {
                        "duplicate": {"requests": 4},
                        "out_of_order": {"requests": 2},
                    }
                },
            },
            {
                **plan[1].as_dict(),
                "status": "failed",
                "error": "TimeoutError: stale path did not settle",
            },
        ]

        summary = drill.stress_result_summary(plan, results)

        self.assertEqual(summary["operation_count"], 2)
        self.assertEqual(summary["completed_count"], 1)
        self.assertEqual(summary["failed_count"], 1)
        self.assertEqual(summary["injection_planned"]["stale"], 1)
        self.assertEqual(summary["injection_completed"]["stale"], 0)
        self.assertEqual(summary["injection_completed"]["duplicate"], 1)
        self.assertEqual(summary["injection_completed"]["out_of_order"], 1)

    def test_wait_rearm_always_requires_canonical_evidence(self) -> None:
        self.assertTrue(drill.canonical_wait_evidence_required(Mock()))

    def test_exercise_records_failed_phase_when_harness_setup_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            paths = drill.DrillPaths.from_root(Path(directory))
            record = {
                "schema_version": drill.RUN_SCHEMA_VERSION,
                "drill_run_id": "drill-setup-failure",
                "last_mode": "shadow",
                "mode_session": 1,
                "phase_history": [],
                "parameters": {
                    "scenarios": ["reducer_only_candidates"],
                    "iterations": 1,
                    "concurrency": 1,
                    "duplicate_ratio": 0.0,
                    "stale_ratio": 0.0,
                },
                "resources": {"container": "candidate"},
            }
            drill.save_record(paths, record)
            args = drill.argparse.Namespace(run_dir=paths.root, scenario=None)

            with (
                patch.object(drill, "validate_record"),
                patch.object(drill, "container_running", return_value=True),
                patch.object(
                    drill,
                    "make_harness",
                    side_effect=RuntimeError("injected harness failure"),
                ),
            ):
                with self.assertRaisesRegex(
                    AssertionError,
                    "stress setup failed",
                ):
                    drill.exercise_scenarios(args)

            persisted = drill.load_record(paths)
            self.assertEqual(len(persisted["phase_history"]), 1)
            phase = persisted["phase_history"][0]
            self.assertEqual(phase["action"], "exercise")
            self.assertEqual(phase["status"], "failed")
            self.assertIn(
                "injected harness failure",
                phase["detail"]["stress"]["setup_error"],
            )
            self.assertTrue(
                (paths.phases / "exercise-1" / "stress-summary.json").is_file()
            )

    def test_restart_checkpoint_records_three_process_verification(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            paths = drill.DrillPaths.from_root(Path(directory))
            record = {
                "schema_version": drill.RUN_SCHEMA_VERSION,
                "drill_run_id": "drill-restart-success",
                "last_mode": "shadow",
                "mode_session": 1,
                "phase_history": [],
                "parameters": {"timeout_seconds": 30},
                "resources": {"container": "candidate"},
            }
            drill.save_record(paths, record)
            args = drill.argparse.Namespace(
                run_dir=paths.root,
                checkpoint="queue_claim_activation_admission",
            )
            calls = []

            class Harness:
                evidence = paths.phases / "restart-fixture"

                def seed_scheduler_restart_fixture(
                    self,
                    label,
                    *,
                    agent,
                    checkpoint,
                    stage,
                    objective,
                ):
                    calls.append((label, agent, checkpoint, stage, objective))
                    return {
                        "message_id": "message-1",
                        "work_item_id": "work-1",
                        "activation_id": "activation-1",
                        "replay_applied": stage == "replay",
                        "replay_exactly_once": stage in {"replay", "verify"},
                    }

            with (
                patch.object(drill, "validate_record"),
                patch.object(drill, "container_running", return_value=False),
                patch.object(drill, "make_harness", return_value=Harness()),
            ):
                self.assertEqual(drill.exercise_restart_checkpoint(args), 0)

            self.assertEqual(
                [call[3] for call in calls],
                ["prepare", "replay", "verify"],
            )
            persisted = drill.load_record(paths)
            phase = persisted["phase_history"][0]
            self.assertEqual(phase["action"], "restart_checkpoint")
            self.assertEqual(phase["status"], "completed")
            restart = phase["detail"]["restart"]
            self.assertTrue(restart["first_restart_recovered"])
            self.assertTrue(restart["second_restart_idempotent"])
            self.assertTrue(restart["replay_exactly_once"])
            self.assertTrue(restart["subsequent_progress"])

    def test_restart_checkpoint_records_failed_stage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            paths = drill.DrillPaths.from_root(Path(directory))
            record = {
                "schema_version": drill.RUN_SCHEMA_VERSION,
                "drill_run_id": "drill-restart-failure",
                "last_mode": "shadow",
                "mode_session": 1,
                "phase_history": [],
                "parameters": {"timeout_seconds": 30},
                "resources": {"container": "candidate"},
            }
            drill.save_record(paths, record)
            args = drill.argparse.Namespace(
                run_dir=paths.root,
                checkpoint="ingress_queue_admission",
            )

            class Harness:
                evidence = paths.phases / "restart-fixture"

                def seed_scheduler_restart_fixture(self, label, **kwargs):
                    if kwargs["stage"] == "replay":
                        raise RuntimeError("injected replay failure")
                    return {"replay_exactly_once": False}

            with (
                patch.object(drill, "validate_record"),
                patch.object(drill, "container_running", return_value=False),
                patch.object(drill, "make_harness", return_value=Harness()),
            ):
                with self.assertRaisesRegex(
                    AssertionError,
                    "restart checkpoint failed",
                ):
                    drill.exercise_restart_checkpoint(args)

            persisted = drill.load_record(paths)
            phase = persisted["phase_history"][0]
            self.assertEqual(phase["action"], "restart_checkpoint")
            self.assertEqual(phase["status"], "failed")
            self.assertIn(
                "injected replay failure",
                phase["detail"]["restart"]["error"],
            )

    def test_aggregate_stress_coverage_detects_missing_executions(self) -> None:
        def stress(
            *,
            exact_completed: int,
            stale_completed: int,
            failed_count: int = 0,
        ) -> dict[str, object]:
            return {
                "operation_count": 4,
                "completed_count": 4 - failed_count,
                "failed_count": failed_count,
                "scenario_planned": {
                    "reducer_only_candidates": 2,
                    "exact_wait_resume": 2,
                },
                "scenario_completed": {
                    "reducer_only_candidates": 2,
                    "exact_wait_resume": exact_completed,
                },
                "injection_planned": {
                    "duplicate": 1,
                    "stale": 1,
                    "out_of_order": 1,
                    "wrong_fence": 0,
                },
                "injection_completed": {
                    "duplicate": 1,
                    "stale": stale_completed,
                    "out_of_order": 1,
                    "wrong_fence": 0,
                },
            }

        record = {
            "parameters": {
                "scenarios": [
                    "reducer_only_candidates",
                    "exact_wait_resume",
                ],
                "iterations": 2,
                "duplicate_ratio": 0.1,
                "stale_ratio": 0.1,
            },
            "phase_history": [
                {
                    "action": "exercise",
                    "status": "completed",
                    "at": "2026-07-27T00:00:00Z",
                    "detail": {
                        "mode": "shadow",
                        "mode_session": 1,
                        "evidence": "/tmp/evidence",
                        "stress": stress(exact_completed=1, stale_completed=0),
                    },
                },
                {
                    "action": "exercise",
                    "status": "completed",
                    "at": "2026-07-27T00:10:00Z",
                    "detail": {
                        "mode": "authoritative",
                        "mode_session": 2,
                        "evidence": "/tmp/authoritative",
                        "stress": stress(exact_completed=2, stale_completed=1),
                    },
                },
                {
                    "action": "exercise",
                    "status": "failed",
                    "at": "2026-07-27T00:20:00Z",
                    "detail": {
                        "mode": "shadow",
                        "mode_session": 1,
                        "evidence": "/tmp/shadow-failed",
                        "stress": stress(
                            exact_completed=1,
                            stale_completed=0,
                            failed_count=1,
                        ),
                    },
                },
            ],
        }

        coverage = drill.aggregate_stress_coverage(
            record,
            expected_mode="shadow",
            expected_mode_session=1,
        )

        self.assertEqual(
            coverage["scenario_shortfalls"]["exact_wait_resume"],
            {"required": 2, "completed": 1},
        )
        self.assertEqual(
            set(coverage["missing_required_injections"]),
            {"stale"},
        )
        self.assertEqual(coverage["failed_count"], 0)
        self.assertEqual(coverage["latest_phase_status"], "failed")
        self.assertEqual(len(coverage["failed_phases"]), 1)

        authoritative = drill.aggregate_stress_coverage(
            record,
            expected_mode="authoritative",
            expected_mode_session=2,
        )
        self.assertFalse(authoritative["scenario_shortfalls"])
        self.assertFalse(authoritative["missing_required_injections"])
        self.assertEqual(authoritative["latest_phase_status"], "completed")

    def test_aggregate_restart_coverage_requires_current_complete_matrix(self) -> None:
        def restart_phase(
            checkpoint: str,
            *,
            mode: str = "shadow",
            mode_session: int = 1,
            status: str = "completed",
            cut_kind=None,
            replay_exactly_once: bool = True,
        ) -> dict[str, object]:
            return {
                "action": "restart_checkpoint",
                "status": status,
                "at": f"2026-07-27T00:00:0{mode_session}Z",
                "detail": {
                    "mode": mode,
                    "mode_session": mode_session,
                    "evidence": f"/tmp/{checkpoint}",
                    "restart": {
                        "checkpoint": checkpoint,
                        "cut_kind": cut_kind
                        or drill.RESTART_CHECKPOINT_CUT_KINDS[checkpoint],
                        "first_restart_recovered": True,
                        "second_restart_idempotent": True,
                        "replay_exactly_once": replay_exactly_once,
                        "subsequent_progress": True,
                    },
                },
            }

        phases = [
            restart_phase(checkpoint)
            for checkpoint in drill.RESTART_CHECKPOINTS
        ]
        phases.extend(
            [
                restart_phase(
                    "turn_terminal_settlement",
                    mode="authoritative",
                    mode_session=2,
                ),
                restart_phase(
                    "authority_rollback",
                    status="failed",
                    cut_kind="durable_recovery",
                    replay_exactly_once=False,
                ),
            ]
        )
        coverage = drill.aggregate_restart_coverage(
            {"phase_history": phases},
            expected_mode="shadow",
            expected_mode_session=1,
        )

        self.assertEqual(
            coverage["completed_checkpoints"],
            list(drill.RESTART_CHECKPOINTS[:-1]),
        )
        self.assertFalse(coverage["missing_checkpoints"])
        self.assertEqual(
            set(coverage["failed_checkpoints"]),
            {"authority_rollback"},
        )
        self.assertEqual(
            coverage["cut_kind_mismatches"]["authority_rollback"],
            {
                "expected": "atomic_rollback",
                "actual": "durable_recovery",
            },
        )
        self.assertEqual(
            coverage["verification_failures"]["authority_rollback"],
            ["replay_exactly_once"],
        )

        authoritative = drill.aggregate_restart_coverage(
            {"phase_history": phases},
            expected_mode="authoritative",
            expected_mode_session=2,
        )
        self.assertEqual(
            authoritative["completed_checkpoints"],
            ["turn_terminal_settlement"],
        )
        self.assertEqual(
            set(authoritative["missing_checkpoints"]),
            set(drill.RESTART_CHECKPOINTS) - {"turn_terminal_settlement"},
        )


if __name__ == "__main__":
    unittest.main()
