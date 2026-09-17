import json
import tempfile
import unittest
from pathlib import Path

from scripts.ci_timing import (
    budget_report,
    historical_summary,
    merge_test_target_reports,
    parse_test_log,
    render_summary,
    workflow_metrics,
)


def job(name, started, completed, conclusion="success", steps=None):
    return {
        "name": name,
        "status": "completed",
        "conclusion": conclusion,
        "started_at": started,
        "completed_at": completed,
        "steps": steps or [],
    }


class CiTimingTests(unittest.TestCase):
    def test_parse_test_log_collects_targets_and_slow_targets(self):
        report = parse_test_log(
            """
     Running unittests src/lib.rs (target/debug/deps/holon-123)
test result: ok. 3205 passed; 0 failed; finished in 100.25s
     Running tests/http.rs (target/debug/deps/http-123)
test result: ok. 10 passed; 0 failed; finished in 2.50s
   Doc-tests holon
test result: ok. 4 passed; 0 failed; finished in 0.25s
"""
        )

        self.assertEqual(report["target_count"], 3)
        self.assertEqual(report["targets"][0]["name"], "unittests src/lib.rs")
        self.assertEqual(report["total_reported_seconds"], 103.0)
        self.assertEqual(
            [target["name"] for target in report["slow_targets"]],
            ["unittests src/lib.rs"],
        )
        self.assertEqual(report["individual_test_timing"]["status"], "unavailable")

    def test_parse_test_log_accepts_downloaded_github_job_logs(self):
        report = parse_test_log(
            "\n".join(
                (
                    "Rust\tCheck generated snapshots\t"
                    "2026-09-13T00:00:00Z test result: ok. finished in 9.00s",
                    "Rust\tTest\t2026-09-13T00:00:01Z "
                    "\x1b[1m\x1b[92m     Running\x1b[0m "
                    "tests/http.rs (target/debug/deps/http-123)",
                    "Rust\tTest\t2026-09-13T00:00:04Z "
                    "test result: ok. finished in 3.00s",
                )
            )
        )

        self.assertEqual(
            report["targets"],
            [{"name": "tests/http.rs", "seconds": 3.0}],
        )

    def test_parse_test_log_accepts_printed_ansi_escapes(self):
        report = parse_test_log(
            "\n".join(
                (
                    "Rust\tTest\t2026-09-13T00:00:01Z "
                    "^[[1m^[[92m     Running^[[0m "
                    "tests/http.rs (target/debug/deps/http-123)",
                    "Rust\tTest\t2026-09-13T00:00:04Z "
                    "test result: ok. finished in 3.00s",
                )
            )
        )

        self.assertEqual(report["target_count"], 1)

    def test_workflow_metrics_excludes_timing_job(self):
        jobs = [
            job(
                "Rust",
                "2026-09-13T00:00:00Z",
                "2026-09-13T00:45:00Z",
                steps=[
                    {
                        "name": "Test",
                        "status": "completed",
                        "conclusion": "success",
                        "started_at": "2026-09-13T00:10:00Z",
                        "completed_at": "2026-09-13T00:40:00Z",
                    }
                ],
            ),
            job(
                "Coverage",
                "2026-09-13T00:02:00Z",
                "2026-09-13T00:32:00Z",
            ),
            {
                "name": "CI Timing",
                "status": "in_progress",
                "conclusion": None,
                "started_at": "2026-09-13T00:45:00Z",
                "completed_at": None,
                "steps": [],
            },
        ]

        report = workflow_metrics(jobs)

        self.assertEqual(report["workflow_wall_seconds"], 2700)
        self.assertEqual(report["total_runner_seconds"], 4500)
        self.assertEqual(report["rust_job_seconds"], 2700)
        self.assertEqual(report["coverage_job_seconds"], 1800)
        self.assertEqual(report["jobs"][1]["steps"][0]["seconds"], 1800)

    def test_history_and_budgets_are_warning_only(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            samples = (
                [
                    job(
                        "Rust",
                        "2026-09-13T00:00:00Z",
                        "2026-09-13T00:40:00Z",
                    )
                ],
                [
                    job(
                        "Rust",
                        "2026-09-13T00:00:00Z",
                        "2026-09-13T00:50:00Z",
                    )
                ],
                [
                    job(
                        "Web",
                        "2026-09-13T00:00:00Z",
                        "2026-09-13T00:01:00Z",
                    )
                ],
            )
            for index, jobs in enumerate(samples):
                (root / f"{index}.json").write_text(json.dumps({"jobs": jobs}))

            history = historical_summary(sorted(root.glob("*.json")))
            current = workflow_metrics(samples[1])
            budgets = budget_report(
                current,
                {
                    "slow_targets": [
                        {"name": "unittests src/lib.rs", "seconds": 100.0}
                    ]
                },
            )

        rust = history["metrics"]["rust_job_seconds"]
        self.assertEqual(rust["samples"], 2)
        self.assertEqual(history["run_count"], 2)
        self.assertEqual(rust["p50_seconds"], 2700)
        self.assertEqual(rust["p90_seconds"], 2940)
        self.assertEqual(budgets["mode"], "warning_only")
        self.assertEqual(budgets["status"], "warning")
        self.assertTrue(
            next(
                check
                for check in budgets["checks"]
                if check["metric"] == "rust_job_seconds"
            )["exceeded"]
        )

    def test_render_summary_uses_budget_and_target_count(self):
        current = {
            "workflow_wall_seconds": 60,
            "total_runner_seconds": 120,
            "rust_job_seconds": 60,
            "coverage_job_seconds": 60,
            "jobs": [],
        }
        history = {
            field: {"samples": 0, "p50_seconds": None, "p90_seconds": None}
            for field in (
                "workflow_wall_seconds",
                "total_runner_seconds",
                "rust_job_seconds",
                "coverage_job_seconds",
            )
        }
        targets = {
            "targets": [{"name": "src/lib.rs", "seconds": 100.25}],
            "slow_targets": [{"name": "src/lib.rs", "seconds": 100.25}],
        }
        report = {
            "current": current,
            "history": {"metrics": history},
            "rust_test_targets": targets,
            "budgets": budget_report(current, targets),
            "cache_status": "not-configured",
        }

        summary = render_summary(report)

        self.assertIn("| Target | Seconds | Over 90s budget |", summary)
        self.assertIn(
            "`test_target_seconds` exceeded `90s` (1 target)", summary
        )

    def test_merge_test_target_reports_combines_shards(self):
        def shard_report(entries):
            return parse_test_log(
                "\n".join(
                    f"     Running {name} (target/debug/deps/x)\n"
                    f"test result: ok. 1 passed; 0 failed; finished in {seconds:.2f}s"
                    for name, seconds in entries
                )
            )

        merged = merge_test_target_reports(
            [
                shard_report([("unittests src/lib.rs", 977.42)]),
                shard_report(
                    [("tests/http_control.rs", 89.02), ("tests/http_client.rs", 26.63)]
                ),
                shard_report([("tests/http_control.rs", 91.0)]),
            ]
        )

        self.assertEqual(merged["target_count"], 3)
        self.assertEqual(merged["total_reported_seconds"], 1184.07)
        self.assertEqual(merged["targets"][0]["name"], "unittests src/lib.rs")
        control = next(
            target
            for target in merged["targets"]
            if target["name"] == "tests/http_control.rs"
        )
        self.assertAlmostEqual(control["seconds"], 180.02, places=3)
        self.assertEqual(len(merged["slow_targets"]), 2)


if __name__ == "__main__":
    unittest.main()
