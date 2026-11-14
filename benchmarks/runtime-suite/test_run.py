"""Tests for the opt-in baseline's scope, provenance and failure accounting."""

import argparse
import copy
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import run


def task_artifact(rows=2000, source_sha256="source"):
    return {
        "schema": "holon.task-history-scale.v1",
        "fixture": {
            "history_rows": rows, "fixed_message_rows": 2, "total_message_rows": rows + 2,
            "task_rows": 4, "active_tasks": 2, "selected_records_per_read": 1,
            "warmup_repetitions": 1, "measured_repetitions": 3,
        },
        "environment": {"debug_assertions": False, "build_source_sha256": source_sha256},
        "coverage_status": "partial",
        "coverage_gaps": [{"benchmark_id": name} for name in run.TASK_GAPS],
        "results": [{
            "id": name, "operations_per_sample": 1,
            "samples": [{"iteration": index, "wall_time_ns": index + 1,
                         "exit_status": "success", "stages": []} for index in range(3)],
            "summary": {
                "median_wall_time_ns": 2, "min_wall_time_ns": 1,
                "max_wall_time_ns": 3, "median_absolute_deviation_ns": 1,
            },
        } for name in run.TASK_OPERATIONS],
    }


class ContractTests(unittest.TestCase):
    def test_full_reuses_fixed_d0_and_scale_is_not_named_full(self):
        scale, full = run.plan("scale"), run.plan("full")
        self.assertEqual([cell["history_rows"] for cell in scale if cell["kind"] == "task"],
                         [0, 2000, 20000, 100000])
        self.assertFalse(any(cell["kind"] == "d0" for cell in scale))
        self.assertEqual(full[:len(scale)], scale)
        self.assertEqual([cell["case"] for cell in full if cell["kind"] == "d0"],
                         ["normal", "pressure"])

    def test_d0_sampling_and_ready_contract_are_explicit(self):
        binary = {"agent_state_workload": {"path": "/saved/d0"}}
        for case, warm, measured in (("normal", "13", "32"), ("pressure", "4", "8")):
            command = run.command_for(
                {"id": f"d0-{case}", "kind": "d0", "case": case},
                binary, Path("/new/output"), 300, 2, 1800,
            )
            self.assertIn("--require-ready-admission", command)
            self.assertEqual(command[command.index("--warmup-per-agent") + 1], warm)
            self.assertEqual(command[command.index("--measured-per-agent") + 1], measured)

    def test_task_population_profile_and_samples_are_required(self):
        self.assertEqual(
            run.validate_task(task_artifact(), 2000, 3, "source")[
                run.TASK_OPERATIONS[0]]["median_wall_time_ns"], 2,
        )
        for path, value in (
            (("fixture", "history_rows"), 20_000),
            (("environment", "debug_assertions"), True),
            (("environment", "build_source_sha256"), "other-source"),
            (("fixture", "active_tasks"), 3),
            (("fixture", "total_message_rows"), 2),
            (("coverage_status",), "complete"),
            (("coverage_gaps",), []),
            (("schema",), "unknown"),
            (("results",), []),
        ):
            artifact = task_artifact()
            target = artifact
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = value
            with self.assertRaises(ValueError):
                run.validate_task(artifact, 2000, 3, "source")
        with self.assertRaises(ValueError):
            run.validate_task(task_artifact(), 2000, 4, "source")

    def test_duplicate_task_results_are_invalid(self):
        artifact = task_artifact()
        artifact["results"] *= 2
        with self.assertRaises(ValueError):
            run.validate_task(artifact, 2000, 3, "source")

    def test_missing_and_unknown_operations_are_invalid(self):
        for results in (task_artifact()["results"][:1],
                        [{"id": "anything", "samples": [None] * 3, "summary": None}]):
            with self.subTest(results=results), self.assertRaises(ValueError):
                run.validate_task(task_artifact() | {"results": results}, 2000, 3, "source")

    def test_invalid_numeric_samples_or_summaries_are_rejected(self):
        for value in (None, True, -1, float("nan"), float("inf"), "1"):
            for target in ("sample", "summary"):
                artifact = task_artifact()
                result = artifact["results"][0]
                if target == "sample":
                    result["samples"][0]["wall_time_ns"] = value
                else:
                    result["summary"]["median_wall_time_ns"] = value
                with self.subTest(value=value, target=target), self.assertRaises(ValueError):
                    run.validate_task(artifact, 2000, 3, "source")

    def test_wrong_iteration_status_and_summary_are_rejected(self):
        for field, value in (("iteration", 3), ("exit_status", "failed")):
            artifact = task_artifact()
            artifact["results"][0]["samples"][0][field] = value
            with self.assertRaises(ValueError):
                run.validate_task(artifact, 2000, 3, "source")
        artifact = task_artifact()
        artifact["results"][0]["summary"]["median_wall_time_ns"] = 3
        with self.assertRaises(ValueError):
            run.validate_task(artifact, 2000, 3, "source")

    def test_process_failure_is_returned_without_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            record = Path(directory) / "process"
            code, _ = run.execute([sys.executable, "-c", "raise SystemExit(7)"],
                                  {}, record, 5)
            self.assertEqual(code, 7)
            self.assertTrue(record.with_suffix(".stderr").is_file())

    @unittest.skipUnless(sys.platform != "win32", "process groups require POSIX")
    def test_timeout_is_not_a_success_sample(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(TimeoutError):
                run.execute([sys.executable, "-c", "import time; time.sleep(60)"],
                            {}, Path(directory) / "timeout", 0.05)

    def test_adapter_timeout_is_not_an_ordinary_failure(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            run.os, "killpg", wraps=run.os.killpg
        ) as killpg:
            with self.assertRaisesRegex(TimeoutError, "adapter timeout"):
                run.execute([sys.executable, "-c", "raise SystemExit(124)"],
                            {}, Path(directory) / "adapter-timeout", 5)
            self.assertEqual([call.args[1] for call in killpg.call_args_list],
                             [run.signal.SIGTERM, run.signal.SIGKILL])

    def test_untracked_implementation_affects_source_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(
                ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                 "commit", "-qm", "fixture", "--allow-empty"], cwd=root, check=True,
            )
            before = run.source_identity(root)
            (root / "benches").mkdir()
            (root / "benches/new.rs").write_text("fn main() {}")
            after = run.source_identity(root)
            self.assertNotEqual(before, after)
            self.assertIn("benches/new.rs", after["untracked"])


class AccountingTests(unittest.TestCase):
    def invoke(self, fail=False, changed=False, timed_out=False, adapter_timeout=False,
               invalid_task=False):
        actual_execute = run.execute
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = Path(sys.executable).resolve()
            source = {"commit": "fixed", "diff_sha256": "diff", "untracked": {}}
            build = {
                "schema": "holon.runtime-baseline-build.v1", "status": "built",
                "source": source, "source_sha256": run.identity_digest(source),
                "profile": "release", "toolchain": "test", "rustflags": "-D warnings",
                "binaries": {
                    name: {"path": str(binary), "sha256": run.digest(binary)}
                    for name in run.BINARY_NAMES[:2]
                },
            }
            build_path = root / "build.json"
            build_path.write_text(json.dumps(build))
            args = argparse.Namespace(
                build_manifest=build_path, output=root / "results", profile="scale",
                samples=3, requests=300, repeats=2, cell_timeout=240, d0_timeout=1800,
                timeout=2700,
            )

            def execute(command, env, record, timeout):
                if timed_out:
                    raise TimeoutError("fixture process-group timeout")
                if adapter_timeout:
                    return actual_execute(
                        [sys.executable, "-c", "raise SystemExit(124)"],
                        env, record, timeout,
                    )
                rows = int(env["HOLON_BENCH_HISTORY_ROWS"])
                artifact = task_artifact(rows, build["source_sha256"])
                if invalid_task:
                    artifact["results"][0]["samples"] = [None] * 3
                record.with_suffix(".stdout").write_text(
                    json.dumps(artifact)
                )
                record.with_suffix(".stderr").write_text("preserved failure" if fail else "")
                return (1 if fail else 0), 0.1

            def collect(cell, output, record, build, args):
                if cell["kind"] == "task":
                    return run.validate_task(json.loads(record.with_suffix(".stdout").read_text()),
                                             cell["history_rows"], args.samples,
                                             build["source_sha256"])
                return {"state": "tested adapter separately"}

            with mock.patch.object(
                run, "source_identity", side_effect=[source, {} if changed else copy.deepcopy(source)]
            ), mock.patch.object(run, "execute", side_effect=execute) as process, mock.patch.object(
                run, "collect", side_effect=collect
            ), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                status = run.run(args)
            manifest = json.loads((args.output / "manifest.json").read_text())
            return status, process.call_count, manifest

    def test_scoped_completion_does_not_claim_unmeasured_families(self):
        status, calls, manifest = self.invoke()
        self.assertEqual((status, calls), (0, 9))
        self.assertEqual(manifest["measurement_status"], "complete")
        self.assertEqual(manifest["timing_verdict"], "reported_only")
        self.assertIn("d0-pressure", manifest["not_measured"])
        self.assertIn("historical-positive-controls-4105-4095-4059", manifest["not_measured"])
        self.assertTrue(set(run.TASK_GAPS).issubset(manifest["not_measured"]))

    def test_failed_processes_are_preserved_not_retried_or_summarized(self):
        status, calls, manifest = self.invoke(fail=True)
        self.assertEqual((status, calls), (1, 9))
        self.assertEqual(manifest["measurement_status"], "failed")
        self.assertTrue(all(item["status"] == "invalid" for item in manifest["runs"]))
        self.assertTrue(all("metrics" not in item for item in manifest["runs"]))

    def test_source_change_invalidates_otherwise_successful_runs(self):
        status, _, manifest = self.invoke(changed=True)
        self.assertEqual(status, 1)
        self.assertEqual(manifest["measurement_status"], "invalid")

    def test_timeout_stops_further_cells_and_preserves_incomplete_status(self):
        status, calls, manifest = self.invoke(timed_out=True)
        self.assertEqual((status, calls), (1, 1))
        self.assertEqual(manifest["measurement_status"], "failed")
        self.assertIn("timeout", manifest["failure"])
        self.assertEqual(manifest["runs"][0]["status"], "invalid")

    def test_inner_adapter_timeout_also_stops_the_suite(self):
        status, calls, manifest = self.invoke(adapter_timeout=True)
        self.assertEqual((status, calls), (1, 1))
        self.assertEqual(manifest["measurement_status"], "failed")
        self.assertIn("adapter timeout", manifest["failure"])
        self.assertNotIn("metrics", manifest["runs"][0])

    def test_invalid_task_samples_cannot_be_marked_measured(self):
        status, calls, manifest = self.invoke(invalid_task=True)
        self.assertEqual((status, calls), (1, 9))
        self.assertEqual(manifest["measurement_status"], "failed")
        self.assertTrue(all(entry["status"] == "invalid"
                            for entry in manifest["runs"] if entry["id"].startswith("task-")))


if __name__ == "__main__":
    unittest.main()
