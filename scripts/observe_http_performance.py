#!/usr/bin/env python3
"""Collect balanced, sustained HTTP observations; never a performance pass gate."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import random
import statistics
import subprocess
import sys
import time


SCHEMA = "holon.http-observation.v1"
CASES = (
    "http.agents_list.gate_miss",
    "http.agents_list.gate_hit",
    "http.agent_state.gate_miss",
    "http.agent_state.gate_hit",
    "http.runtime_performance.100",
    "http.agents_list.101",
    "http.work_items.200",
    "http.work_items.200x10",
)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def source_snapshot(directory):
    def git(*args):
        return subprocess.check_output(["git", *args], cwd=directory)

    paths = git("ls-files", "--cached", "--others", "--exclude-standard", "-z").split(b"\0")
    files = {os.fsdecode(path): digest(directory / os.fsdecode(path))
             for path in paths if path and (directory / os.fsdecode(path)).is_file()}
    return {
        "head": git("rev-parse", "HEAD").decode().strip(),
        "tree": git("rev-parse", "HEAD^{tree}").decode().strip(),
        "dirty": bool(git("status", "--porcelain")),
        "files": files,
    }


def source_fingerprint(snapshot):
    payload = json.dumps(snapshot, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(payload).hexdigest()


def validate_source(artifact, source):
    environment = artifact["environment"]
    if environment.get("build_source_sha256") != source_fingerprint(source):
        raise ValueError("artifact build source fingerprint mismatch")
    if environment["git_revision"] != source["head"]:
        raise ValueError("artifact source revision mismatch")


def schedule(rounds):
    if rounds < 5:
        raise ValueError("at least five paired rounds are required")
    return [
        (pair + 1, side)
        for pair in range(rounds)
        for side in (("baseline", "candidate") if pair % 2 == 0 else ("candidate", "baseline"))
    ]


def finite_positive(value):
    return isinstance(value, (int, float)) and math.isfinite(value) and value > 0


def validate(artifact, cases, seconds):
    if artifact.get("workload_version") != "v3-window":
        raise ValueError("not a sustained observation artifact")
    if artifact["config"].get("observation_seconds") != seconds:
        raise ValueError("observation duration mismatch")
    if artifact["config"].get("transport") != "tower.oneshot.in_process":
        raise ValueError("transport mismatch")
    results = artifact["results"]
    if {row["benchmark_id"] for row in results} != set(cases) or len(results) != len(cases):
        raise ValueError("missing, duplicate or extra workloads")
    for row in results:
        observation = row["observation"]
        samples = row["samples"]
        operations = row["operations_per_sample"]
        if not isinstance(operations, int) or operations < 1:
            raise ValueError("invalid batch size")
        if len(samples) < max(100, observation["minimum_batches"]):
            raise ValueError("insufficient samples")
        if observation["timed_operations"] != len(samples) * operations:
            raise ValueError("operation count mismatch")
        if observation["warmup_batches"] < 1:
            raise ValueError("no warmup")
        if any(s["exit_status"] != "ok" or not finite_positive(s["wall_time_ns"]) for s in samples):
            raise ValueError("failed or invalid sample")
        measured = sum(s["wall_time_ns"] for s in samples)
        if measured != observation["measured_request_time_ns"] or measured < seconds * 1e9:
            raise ValueError("insufficient or inconsistent timed-request duration")
        if observation["minimum_request_time_ns"] != seconds * 1_000_000_000:
            raise ValueError("minimum duration mismatch")
        stages = observation["stages"]
        if len({s["name"] for s in stages}) != len(stages):
            raise ValueError("duplicate stages")
        for stage in stages:
            if any(not isinstance(stage[k], int) or stage[k] < 0
                   for k in ("count", "inclusive_total_ns", "rows")):
                raise ValueError("invalid stage delta")
    return {row["benchmark_id"]: row for row in results}


def paired_difference(before, after):
    if len(before) != len(after) or len(before) < 5:
        raise ValueError("at least five complete pairs are required")
    differences = [c - b for b, c in zip(before, after)]
    rng = random.Random(3793)
    medians = sorted(
        statistics.median(rng.choices(differences, k=len(differences)))
        for _ in range(10_000)
    )
    low, high = medians[249], medians[9749]
    return {
        "baseline_rounds": before, "candidate_rounds": after,
        "baseline_median": statistics.median(before),
        "candidate_median": statistics.median(after),
        "paired_median_delta": statistics.median(differences),
        "paired_median_percent": (
            statistics.median([(c / b - 1) * 100 for b, c in zip(before, after)])
            if all(b > 0 for b in before) else None
        ),
        "paired_bootstrap_95_interval": [low, high],
        "classification": "改善" if high < 0 else "退化" if low > 0 else "不确定",
    }


def analyze(directory):
    directory = Path(directory)
    manifest = json.loads((directory / "manifest.json").read_text())
    if manifest.get("schema_version") != SCHEMA:
        raise ValueError("unsupported observation manifest")
    cases, seconds = manifest["cases"], manifest["seconds"]
    expected = schedule(manifest["rounds"])
    runs = manifest["runs"]
    if [(r["pair"], r["side"]) for r in runs] != expected:
        raise ValueError("incomplete or unbalanced run schedule")
    grouped = {"baseline": [], "candidate": []}
    configuration = None
    environment = None
    for run in runs:
        path = directory / run["artifact"]
        if run["exit_status"] != 0 or digest(path) != run["sha256"]:
            raise ValueError("failed run or artifact hash mismatch")
        artifact = json.loads(path.read_text())
        validate_source(artifact, manifest["sources"][run["side"]])
        # Source bindings differ by design; machine/toolchain must not.
        comparable = {k: v for k, v in artifact["environment"].items()
                      if k not in ("git_revision", "git_dirty", "build_source_sha256")}
        if configuration is None:
            configuration, environment = artifact["config"], comparable
        if artifact["config"] != configuration or comparable != environment:
            raise ValueError("configuration or environment mismatch")
        grouped[run["side"]].append(validate(artifact, cases, seconds))
    report = {
        "schema_version": SCHEMA,
        "interval_method": "seeded paired percentile bootstrap of run medians; 10000 resamples",
        "limitations": [
            "Synthetic warm in-process routes; not network latency or production capacity.",
            "Independent paired runs, not individual requests, are the resampling unit.",
            "Small round counts and shared-host drift limit interval reliability.",
            "Batch means are not per-request p95/p99. No performance acceptance threshold.",
            "Stage durations are inclusive, overlapping, process-global and non-exhaustive.",
            "Missing stages are not zero. Zero attempts do not estimate per-call latency.",
        ],
        "cases": [],
    }
    for case in cases:
        rows = {side: [run[case] for run in arm] for side, arm in grouped.items()}
        times = {
            side: [statistics.median(s["wall_time_ns"] for s in r["samples"])
                   / r["operations_per_sample"] for r in arm]
            for side, arm in rows.items()
        }
        sizes = {r["operations_per_sample"] for arm in rows.values() for r in arm}
        if len(sizes) != 1:
            raise ValueError(f"{case}: batch size mismatch")
        result = {
            "case": case, "unit": "ns/request" if sizes == {1} else "ns/request (batch mean)",
            "latency": paired_difference(times["baseline"], times["candidate"]),
            "stages": [],
        }
        stage_maps = {
            side: [{s["name"]: s for s in r["observation"]["stages"]} for r in arm]
            for side, arm in rows.items()
        }
        names = sorted(set().union(*(m.keys() for arm in stage_maps.values() for m in arm)))
        for name in names:
            if any(name not in m for arm in stage_maps.values() for m in arm):
                result["stages"].append({"name": name, "classification": "未评估"})
                continue
            if not any(m[name]["count"] for arm in stage_maps.values() for m in arm):
                continue
            stage = {"name": name}
            for key, output in (
                ("inclusive_total_ns", "inclusive_ns_per_request"),
                ("count", "calls_per_request"), ("rows", "rows_per_request"),
            ):
                values = {
                    side: [m[name][key] / row["observation"]["timed_operations"]
                           for m, row in zip(stage_maps[side], rows[side])]
                    for side in rows
                }
                stage[output] = paired_difference(values["baseline"], values["candidate"])
            result["stages"].append(stage)
        report["cases"].append(result)
    return report


def markdown(report):
    lines = [
        "# HTTP 长窗口性能比较", "",
        "负数为耗时减少；区间跨零记不确定，不设置性能通过阈值。阶段互有包含，不可相加。",
        "进程内暖fixture；批均值不是单请求尾延迟，不能直接推断线上体验。", "",
        "| case | 基线 μs/请求 | 候选 μs/请求 | 配对差 μs | 配对变化 | 95%配对区间 μs | 判断 |",
        "| --- | ---: | ---: | ---: | ---: | --- | --- |",
    ]
    for case in report["cases"]:
        value = case["latency"]
        low, high = value["paired_bootstrap_95_interval"]
        percent = value["paired_median_percent"]
        lines.append(
            f"| `{case['case']}` | {value['baseline_median']/1000:.3f} "
            f"| {value['candidate_median']/1000:.3f} | {value['paired_median_delta']/1000:+.3f} "
            f"| {percent:+.2f}% | [{low/1000:+.3f}, {high/1000:+.3f}] | {value['classification']} |"
        )
    batches = [f"`{case['case']}`" for case in report["cases"] if "batch mean" in case["unit"]]
    if batches:
        lines.extend(["", f"{'、'.join(batches)} 使用批均值，不报告单请求 p95/p99。"])
    lines.extend(["", "## 阶段差分", "", "按各阶段每请求 inclusive 耗时差排序；计数、行数与全部原始轮次见 JSON。"])
    for case in report["cases"]:
        lines.extend(["", f"### `{case['case']}`", ""])
        stages = sorted(case["stages"], key=lambda s: abs(
            s.get("inclusive_ns_per_request", {}).get("paired_median_delta", 0)), reverse=True)
        for stage in stages:
            if "inclusive_ns_per_request" not in stage:
                lines.append(f"- `{stage['name']}`：未评估（缺少可比阶段）。")
                continue
            value = stage["inclusive_ns_per_request"]
            lines.append(f"- `{stage['name']}`：{value['baseline_median']/1000:.3f} → "
                         f"{value['candidate_median']/1000:.3f} μs/请求；{value['classification']}。")
    return "\n".join(lines) + "\n"


def collect(args):
    if not 10 <= args.seconds <= 30:
        raise ValueError("--seconds must be 10..30")
    order = schedule(args.rounds)
    directory = args.output.resolve()
    directory.mkdir(parents=True, exist_ok=False)
    binaries = {side: Path(getattr(args, side)).resolve() for side in ("baseline", "candidate")}
    directories = {side: Path(getattr(args, side + "_cwd")).resolve()
                   for side in binaries}
    hashes = {side: digest(path) for side, path in binaries.items()}
    manifest = {
        "schema_version": SCHEMA, "seconds": args.seconds, "rounds": args.rounds,
        "cases": args.cases, "binaries": {s: str(p) for s, p in binaries.items()},
        "binary_sha256": hashes, "sources": {s: source_snapshot(p) for s, p in directories.items()},
        "source_directories": {s: str(p) for s, p in directories.items()}, "runs": [],
        "runner_sha256": digest(__file__),
        "cpu_affinity": sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
    }
    manifest_path = directory / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    env = {**os.environ, "HOLON_BENCH_WINDOW_SECS": str(args.seconds),
           "HOLON_BENCH_SAMPLES": "100", "HOLON_BENCH_FILTER": ",".join(args.cases)}
    for pair, side in order:
        if (digest(binaries[side]) != hashes[side]
                or source_snapshot(directories[side]) != manifest["sources"][side]):
            raise ValueError("binary or source changed during observation")
        name = f"{pair:02}-{side}.json"
        started = time.time()
        manifest["active"] = {"pair": pair, "side": side, "started": started}
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        # Fresh processes/fixtures, serialized execution. External CPU/memory
        # scopes apply to this runner and are inherited by both binaries.
        with (directory / name).open("w") as out, (directory / f"{pair:02}-{side}.stderr").open("w") as err:
            try:
                result = subprocess.run([str(binaries[side])], cwd=directories[side], env=env, stdout=out, stderr=err,
                                        timeout=args.timeout_seconds, check=False)
                exit_status, timed_out = result.returncode, False
            except subprocess.TimeoutExpired:
                exit_status, timed_out = 124, True
        run = {"pair": pair, "side": side, "artifact": name,
               "exit_status": exit_status, "timed_out": timed_out,
               "elapsed_seconds": time.time() - started, "sha256": digest(directory / name)}
        manifest["runs"].append(run)
        manifest["active"] = None
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        print(json.dumps(run), flush=True)
        if exit_status:
            raise ValueError(f"{side} pair {pair} failed; original artifacts retained")
        if (digest(binaries[side]) != hashes[side]
                or source_snapshot(directories[side]) != manifest["sources"][side]):
            raise ValueError("binary or source changed during observation")
        artifact = json.loads((directory / name).read_text())
        validate_source(artifact, manifest["sources"][side])
        validate(artifact, args.cases, args.seconds)
    return directory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-fingerprint", type=Path,
                        help="print the source fingerprint to embed when compiling http_control")
    parser.add_argument("--analyze", type=Path, help="analyze an existing complete observation")
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--baseline-cwd", type=Path, help="source worktree matching the baseline binary")
    parser.add_argument("--candidate-cwd", type=Path, help="source worktree matching the candidate binary")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--seconds", type=int, default=10)
    parser.add_argument("--rounds", type=int, default=6)
    parser.add_argument("--timeout-seconds", type=int, default=1800)
    parser.add_argument("--cases", nargs="+", choices=CASES, default=list(CASES))
    args = parser.parse_args()
    if args.source_fingerprint:
        print(source_fingerprint(source_snapshot(args.source_fingerprint.resolve())))
        return
    if not args.analyze and not all((args.baseline, args.candidate, args.output,
                                     args.baseline_cwd, args.candidate_cwd)):
        parser.error("provide --analyze or both binaries, source working directories and --output")
    directory = args.analyze or collect(args)
    report = analyze(directory)
    (directory / "comparison.json").write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    (directory / "comparison.md").write_text(markdown(report))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.TimeoutExpired) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
