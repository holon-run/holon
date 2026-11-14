"""Regression coverage for State comparison artifact acceptance."""

import copy
import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

import run


def artifact():
    batch = {
        "aborted": False,
        "statuses": {"200": 10},
        "http_p50_ns": 100,
        "http_p95_ns": 200,
        "http_p99_ns": None,
        "process_cpu_us": 20,
        "rust_allocations": 30,
        "rust_requested_bytes": 40,
        "response_bytes_p50": 50,
        "committed_work_mutations": 0,
        "gate_before": dict.fromkeys(
            ("leaders", "joined_waiters", "cache_hits", "rejected"), 0
        ),
        "gate_after": {
            "leaders": 1, "joined_waiters": 0, "cache_hits": 9, "rejected": 0
        },
    }
    return {
        "settings": {
            "mode": "loaded",
            "agents": 1,
            "clients": 1,
            "work_items_per_agent": 50,
            "requests_per_batch": 10,
            "batches": 1,
            "pause_ms_per_client": 10,
            "mutation_delay_ms_after_each_commit": 0,
        },
        "source_sha256": "test-source",
        "batches": [batch],
    }


class SummaryTests(unittest.TestCase):
    def test_idle_allows_zero_mutations(self):
        value = artifact()
        result = run.summary(value, value["settings"])
        self.assertEqual(result["requests"], 10)
        self.assertEqual(result["cache_hits"], 9)
        self.assertNotIn("p99_ns_batch_median", result)

    def test_mutation_requires_commit_in_each_batch(self):
        value = artifact()
        value["settings"]["mutation_delay_ms_after_each_commit"] = 50
        value["batches"][0]["committed_work_mutations"] = 1
        value["batches"].append(copy.deepcopy(value["batches"][0]))
        value["settings"]["batches"] = 2
        value["batches"][1]["committed_work_mutations"] = 0
        with self.assertRaisesRegex(ValueError, "commit in every"):
            run.summary(value, value["settings"])

    def test_committed_mutation_is_accepted(self):
        value = artifact()
        value["settings"]["mutation_delay_ms_after_each_commit"] = 50
        value["batches"][0]["committed_work_mutations"] = 2
        self.assertEqual(
            run.summary(value, value["settings"])["committed_work_mutations"], 2
        )

    def test_aborted_batch_is_rejected(self):
        value = artifact()
        value["batches"][0]["aborted"] = True
        with self.assertRaisesRegex(ValueError, "unsuccessful"):
            run.summary(value, value["settings"])

    def test_incomplete_samples_are_rejected(self):
        value = artifact()
        value["batches"][0]["statuses"] = {"200": 9, "503": 1}
        with self.assertRaisesRegex(ValueError, "sample count"):
            run.summary(value, value["settings"])

    def test_settings_must_match_every_planned_field(self):
        for field, planned in artifact()["settings"].items():
            with self.subTest(field=field):
                value = artifact()
                expected = value["settings"].copy()
                value["settings"][field] = (
                    planned + 1 if isinstance(planned, int) else "cold"
                )
                with self.assertRaisesRegex(ValueError, "settings"):
                    run.summary(value, expected)

    def test_missing_planned_batches_are_rejected(self):
        value = artifact()
        value["settings"]["batches"] = 3
        with self.assertRaisesRegex(ValueError, "batch count"):
            run.summary(value, value["settings"])

    def test_compensating_sample_counts_are_rejected(self):
        value = artifact()
        value["settings"]["batches"] = 2
        value["batches"].append(copy.deepcopy(value["batches"][0]))
        value["batches"][0]["statuses"] = {"200": 9}
        value["batches"][1]["statuses"] = {"200": 11}
        with self.assertRaisesRegex(ValueError, "sample count"):
            run.summary(value, value["settings"])

    def test_extra_unsuccessful_status_is_rejected(self):
        value = artifact()
        value["batches"][0]["statuses"]["503"] = 1
        with self.assertRaisesRegex(ValueError, "sample count"):
            run.summary(value, value["settings"])


class DriverTests(unittest.TestCase):
    def invoke(self, extra, side_effect):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "fixture"
            binary.write_text("unused mocked executable")
            binary.chmod(0o700)
            output = root / "results"
            argv = [
                "run.py", "--binary-a", str(binary), "--matrix", "mutations",
                "--output", str(output), *extra,
            ]
            with mock.patch("sys.argv", argv), mock.patch(
                "run.platform.platform", return_value="test-platform"
            ), mock.patch(
                "run.subprocess.run", side_effect=side_effect
            ) as process, contextlib.redirect_stderr(io.StringIO()):
                status = run.main()
            return status, process.call_count, json.loads(
                (output / "manifest.json").read_text()
            )

    def test_submatrices_partition_full_matrix(self):
        self.assertEqual(len(run.cases("idle")), 24)
        self.assertEqual(len(run.cases("mutations")), 4)
        self.assertEqual(
            run.cases("full"), run.cases("idle") + run.cases("mutations")
        )

    def test_scale_holds_agents_fixed_and_crosses_the_wire_window(self):
        cells = run.cases("scale")
        self.assertEqual(len(cells), 12)
        self.assertEqual(len({cell[0] for cell in cells}), 12)
        self.assertEqual({cell[2] for cell in cells}, {1})
        self.assertEqual({cell[4] for cell in cells}, {0, 200, 2000})
        self.assertEqual({cell[1] for cell in cells}, {"loaded", "cold"})
        self.assertEqual({cell[3] for cell in cells}, {1, 10})
        self.assertTrue(all(cell[-1] == 0 for cell in cells))

    def test_failure_stops_by_default(self):
        status, calls, manifest = self.invoke(
            [], lambda *args, **kwargs: subprocess.CompletedProcess(args[0], 1)
        )
        self.assertEqual((status, calls), (1, 1))
        self.assertIn("failure", manifest["runs"][0])

    def test_keep_going_preserves_failures_without_success_summaries(self):
        status, calls, manifest = self.invoke(
            ["--keep-going"],
            lambda *args, **kwargs: subprocess.CompletedProcess(args[0], 1),
        )
        self.assertEqual((status, calls), (1, 8))
        self.assertEqual(len(manifest["runs"]), 8)
        for record in manifest["runs"]:
            self.assertIn("failure", record)
            self.assertNotIn("summary", record)

    def test_timeout_still_stops_keep_going(self):
        status, calls, manifest = self.invoke(
            ["--keep-going"], subprocess.TimeoutExpired("fixture", 1)
        )
        self.assertEqual((status, calls), (124, 1))
        self.assertEqual(manifest["failure_kind"], "timeout")
        self.assertIn("cleanup not guaranteed", manifest["runs"][0]["failure"])


if __name__ == "__main__":
    unittest.main()
