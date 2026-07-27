#!/usr/bin/env python3

import json
import stat
import tempfile
import threading
import time
import unittest
from pathlib import Path

from scripts.docker_e2e import scheduler_drill as drill


class SchedulerDrillTests(unittest.TestCase):
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
                    "protocol_mode": "shadow",
                    "config_revision": 1,
                }
            ],
            "scenario_authorities": [],
            "shadow_comparisons": [
                {
                    "scenario_class": scenario,
                    "comparison_outcome": "matched",
                    "legacy_observation_json": "{}",
                }
                for scenario in drill.PRODUCTION_SCENARIOS
            ]
            + [
                {
                    "scenario_class": "exact_wait_resume",
                    "comparison_outcome": "matched",
                    "legacy_observation_json": json.dumps(
                        {
                            "input_kind": input_kind,
                            "wake_source": wake_source,
                        }
                    ),
                }
                for input_kind, wake_source in (
                    ("callback_event", "external_callback"),
                    ("webhook_event", "external_callback"),
                    ("channel_event", "channel_signal"),
                    ("timer_tick", "wait_deadline"),
                    ("system_tick", "system_tick"),
                    ("system_tick", "operator_wake_hint"),
                )
            ],
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
        summary = drill.evidence_summary(evidence)
        self.assertEqual(summary["status"], "go")
        self.assertTrue(all(summary["checks"].values()))

        evidence["shadow_comparisons"][0]["comparison_outcome"] = "diverged"
        summary = drill.evidence_summary(evidence)
        self.assertEqual(summary["status"], "no-go")
        self.assertFalse(summary["checks"]["no_divergence"])

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
            "divergences": [],
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
        self.assertEqual(sum(operation.duplicate for operation in first), 3)
        self.assertEqual(sum(operation.fault is not None for operation in first), 6)
        self.assertEqual(
            {operation.fault for operation in first if operation.fault},
            {"stale", "out_of_order", "wrong_fence"},
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


if __name__ == "__main__":
    unittest.main()
