import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock

from scripts import observe_http_performance as observation


def artifact(latency=100_000_000, revision="base"):
    samples = [{"wall_time_ns": latency, "exit_status": "ok"} for _ in range(100)]
    return {
        "workload_version": "v3-window",
        "environment": {"git_revision": revision, "git_dirty": False, "os": "linux"},
        "config": {"observation_seconds": 10, "transport": "tower.oneshot.in_process"},
        "results": [{
            "benchmark_id": observation.CASES[0], "operations_per_sample": 1,
            "samples": samples,
            "observation": {
                "minimum_batches": 100, "warmup_batches": 10,
                "minimum_request_time_ns": 10_000_000_000,
                "measured_request_time_ns": latency * len(samples),
                "timed_operations": len(samples),
                "stages": [{"name": "db.read", "count": 100,
                            "inclusive_total_ns": latency * 100, "rows": 200}],
            },
        }],
    }


class HttpObservationTests(unittest.TestCase):
    def test_schedule_alternates_and_balances_six_pairs(self):
        order = observation.schedule(6)
        self.assertEqual(order[:4], [(1, "baseline"), (1, "candidate"),
                                     (2, "candidate"), (2, "baseline")])
        self.assertEqual(len(order), 12)
        self.assertEqual(sum(side == "baseline" for _, side in order), 6)
        with self.assertRaises(ValueError):
            observation.schedule(4)

    def test_paired_intervals_classify_and_preserve_units(self):
        for before, after, expected in [
            ([100] * 6, [80] * 6, "改善"),
            ([100] * 6, [120] * 6, "退化"),
            ([100] * 6, [80, 120, 80, 120, 80, 120], "不确定"),
            ([100] * 6, [100] * 6, "不确定"),
        ]:
            with self.subTest(expected=expected):
                value = observation.paired_difference(before, after)
                self.assertEqual(value["classification"], expected)
        self.assertIsNone(observation.paired_difference([0] * 6, [1] * 6)["paired_median_percent"])

    def test_interval_resamples_pairs_not_individual_requests(self):
        value = observation.paired_difference([100, 200, 300, 400, 500],
                                               [110, 210, 310, 410, 510])
        self.assertEqual(value["paired_bootstrap_95_interval"], [10, 10])
        self.assertEqual(value["paired_median_delta"], 10)
        with self.assertRaises(ValueError):
            observation.paired_difference([1] * 6, [1] * 5)

    def test_artifact_qualification_rejects_invalid_samples(self):
        value = artifact()
        observation.validate(value, [observation.CASES[0]], 10)
        mutations = [
            lambda a: a.update(workload_version="v2"),
            lambda a: a["config"].update(observation_seconds=20),
            lambda a: a["config"].update(transport="socket"),
            lambda a: a["results"].append(copy.deepcopy(a["results"][0])),
            lambda a: a["results"][0]["samples"][0].update(exit_status="failed"),
            lambda a: a["results"][0]["samples"][0].update(wall_time_ns=float("nan")),
            lambda a: a["results"][0]["samples"].pop(),
            lambda a: a["results"][0]["observation"].update(timed_operations=99),
            lambda a: a["results"][0]["observation"].update(measured_request_time_ns=9_000_000_000),
            lambda a: a["results"][0]["observation"].update(warmup_batches=0),
            lambda a: a["results"][0]["observation"]["stages"][0].update(count=-1),
        ]
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                invalid = copy.deepcopy(value)
                mutate(invalid)
                with self.assertRaises(ValueError):
                    observation.validate(invalid, [observation.CASES[0]], 10)

    def make_runs(self, directory):
        manifest = {
            "schema_version": observation.SCHEMA, "cases": [observation.CASES[0]],
            "rounds": 6, "seconds": 10,
            "sources": {"baseline": {"head": "base"}, "candidate": {"head": "candidate"}},
            "runs": [],
        }
        for pair, side in observation.schedule(6):
            path = directory / f"{pair}-{side}.json"
            value = artifact(100_000_000 if side == "baseline" else 110_000_000,
                             "base" if side == "baseline" else "candidate")
            value["environment"]["build_source_sha256"] = observation.source_fingerprint(
                manifest["sources"][side])
            path.write_text(json.dumps(value))
            manifest["runs"].append({
                "pair": pair, "side": side, "artifact": path.name,
                "exit_status": 0, "sha256": observation.digest(path),
            })
        (directory / "manifest.json").write_text(json.dumps(manifest))
        return manifest

    def test_analyze_complete_runs_and_stage_normalization(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            self.make_runs(directory)
            report = observation.analyze(directory)
            case = report["cases"][0]
            self.assertEqual(case["latency"]["classification"], "退化")
            self.assertEqual(case["stages"][0]["calls_per_request"]["baseline_median"], 1)
            self.assertEqual(case["stages"][0]["rows_per_request"]["candidate_median"], 2)
            self.assertIn("进程内", observation.markdown(report))

    def test_analyze_rejects_mutation_incomplete_order_and_environment(self):
        for kind in ("hash", "missing", "order", "environment", "revision", "batch",
                     "build_source", "missing_build_source"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temp:
                directory = Path(temp)
                manifest = self.make_runs(directory)
                if kind == "missing":
                    manifest["runs"].pop()
                elif kind == "order":
                    manifest["runs"].reverse()
                else:
                    run = manifest["runs"][0]
                    path = directory / run["artifact"]
                    value = json.loads(path.read_text())
                    if kind == "environment":
                        value["environment"]["os"] = "other"
                    elif kind == "revision":
                        value["environment"]["git_revision"] = "unknown"
                    elif kind == "build_source":
                        value["environment"]["build_source_sha256"] = "0" * 64
                    elif kind == "missing_build_source":
                        value["environment"].pop("build_source_sha256")
                    elif kind == "batch":
                        row = value["results"][0]
                        row["operations_per_sample"] = 2
                        row["observation"]["timed_operations"] = 200
                    path.write_text(json.dumps(value) + "\n")
                    if kind != "hash":
                        run["sha256"] = observation.digest(path)
                (directory / "manifest.json").write_text(json.dumps(manifest))
                with self.assertRaises(ValueError):
                    observation.analyze(directory)

    def test_runtime_cwd_cannot_relabel_same_binary_as_different_source(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            manifest = self.make_runs(directory)
            for run in manifest["runs"]:
                if run["side"] == "candidate":
                    path = directory / run["artifact"]
                    value = json.loads(path.read_text())
                    # Runtime git says "candidate"; this is still the baseline build.
                    value["environment"]["build_source_sha256"] = observation.source_fingerprint(
                        manifest["sources"]["baseline"])
                    path.write_text(json.dumps(value))
                    run["sha256"] = observation.digest(path)
            (directory / "manifest.json").write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "build source fingerprint mismatch"):
                observation.analyze(directory)

    def test_source_fingerprint_binds_content_untracked_files_and_revision(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)

            def git(*args):
                subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)

            git("init", "-q")
            path = root / "source.rs"
            path.write_text("before")
            git("add", ".")
            git("-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                "commit", "-qm", "fixture")
            original = observation.source_snapshot(root)
            fingerprint = observation.source_fingerprint(original)
            self.assertEqual(fingerprint, observation.source_fingerprint(dict(reversed(
                list(original.items())))))
            path.write_text("after")
            self.assertNotEqual(fingerprint, observation.source_fingerprint(
                observation.source_snapshot(root)))
            path.write_text("before")
            extra = root / "untracked.rs"
            extra.write_text("untracked build input")
            self.assertNotEqual(fingerprint, observation.source_fingerprint(
                observation.source_snapshot(root)))
            extra.unlink()
            git("-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                "commit", "--allow-empty", "-qm", "same content, different revision")
            self.assertNotEqual(fingerprint, observation.source_fingerprint(
                observation.source_snapshot(root)))

    def test_missing_stage_is_not_zero_or_improvement(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            manifest = self.make_runs(directory)
            for run in manifest["runs"]:
                if run["side"] == "candidate":
                    path = directory / run["artifact"]
                    value = json.loads(path.read_text())
                    value["results"][0]["observation"]["stages"] = []
                    path.write_text(json.dumps(value))
                    run["sha256"] = observation.digest(path)
            (directory / "manifest.json").write_text(json.dumps(manifest))
            stages = observation.analyze(directory)["cases"][0]["stages"]
            self.assertEqual(stages, [{"name": "db.read", "classification": "未评估"}])

    def test_collector_rejects_wrong_build_and_preserves_raw_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary = root / "binary"
            binary.write_bytes(b"unit-test placeholder, not a performance binary")
            args = SimpleNamespace(
                seconds=10, rounds=6, output=root / "observations",
                baseline=binary, candidate=binary, baseline_cwd=root, candidate_cwd=root,
                cases=[observation.CASES[0]], timeout_seconds=1,
            )

            def run(*unused, **kwargs):
                value = artifact()
                value["environment"]["build_source_sha256"] = "0" * 64
                kwargs["stdout"].write(json.dumps(value))
                return SimpleNamespace(returncode=0)

            with mock.patch.object(observation, "source_snapshot", return_value={"head": "base"}), \
                    mock.patch.object(observation.subprocess, "run", side_effect=run):
                with self.assertRaisesRegex(ValueError, "build source fingerprint mismatch"):
                    observation.collect(args)
            manifest = json.loads((args.output / "manifest.json").read_text())
            self.assertEqual(manifest["runs"][0]["exit_status"], 0)
            self.assertTrue((args.output / "01-baseline.json").exists())

    def test_collector_records_timeout_and_preserves_raw_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary = root / "binary"
            binary.write_bytes(b"unit-test placeholder, not a performance binary")
            args = SimpleNamespace(
                seconds=10, rounds=6, output=root / "observations",
                baseline=binary, candidate=binary, baseline_cwd=root, candidate_cwd=root,
                cases=[observation.CASES[0]], timeout_seconds=1,
            )
            with mock.patch.object(observation, "source_snapshot", return_value={"head": "base"}), \
                    mock.patch.object(observation.subprocess, "run",
                                      side_effect=observation.subprocess.TimeoutExpired("test", 1)):
                with self.assertRaises(ValueError):
                    observation.collect(args)
            manifest = json.loads((args.output / "manifest.json").read_text())
            self.assertIsNone(manifest["active"])
            self.assertEqual(manifest["runs"][0]["exit_status"], 124)
            self.assertTrue(manifest["runs"][0]["timed_out"])
            self.assertTrue((args.output / "01-baseline.json").exists())
            self.assertTrue((args.output / "01-baseline.stderr").exists())


if __name__ == "__main__":
    unittest.main()
