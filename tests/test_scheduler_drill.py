#!/usr/bin/env python3

import json
import stat
import tempfile
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


if __name__ == "__main__":
    unittest.main()
