#!/usr/bin/env python3
"""Run isolated optimized D0 cases, or compare artifacts from identical workloads."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys
import time

VERSION = "holon-agent-state-d0-v1"
CASES = {"normal": (8, 4), "pressure": (32, 16)}


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def validate(artifact: dict) -> list[str]:
    errors = []
    case = artifact.get("case", {})
    expected = CASES.get(case.get("name"))
    if artifact.get("workload_version") != VERSION or expected is None:
        return ["unrecognized workload version/case"]
    loaded, concurrent = expected
    sampling = artifact.get("sampling", {})
    warmup, measured = sampling.get("warmup_requests_per_agent"), sampling.get("measured_responses_per_agent")
    if not isinstance(warmup, int) or not isinstance(measured, int) or min(warmup, measured) < 1:
        return ["invalid sampling policy"]
    if (case.get("loaded_agents"), case.get("concurrent_requests")) != expected:
        errors.append("workload dimensions differ from the fixed contract")
    if artifact.get("environment", {}).get("debug_assertions") is not False:
        errors.append("optimized profile was not verified")
    fixture = artifact.get("fixture", {})
    for key, value in {
        "history_messages_per_agent": 100_000,
        "linked_turns_per_agent": 64,
        "history_records_per_agent": 100_193,
        "pending_inputs_per_agent": 100,
        "pending_body_bytes": 16_384,
        "large_stdout_bytes_per_agent": 10_485_760,
        "prompt_budget_estimated_tokens": 8192,
    }.items():
        if fixture.get(key) != value:
            errors.append(f"fixture {key} differs from {value}")
    if artifact.get("loaded_preview", {}).get("operations") != loaded * 5:
        errors.append("loaded preview sample count differs")
    if artifact.get("enqueue_commit_publication", {}).get("operations") != loaded:
        errors.append("enqueue sample count differs")
    groups = artifact.get("scripted_dispatch", {}).get("groups", [])
    if len(groups) != loaded // concurrent:
        errors.append("scripted group count differs")
    for index, group in enumerate(groups):
        if len(group.get("agent_ids", [])) != concurrent:
            errors.append(f"group {index} concurrency differs")
        if group.get("measured_constructed_requests") != concurrent * (measured + 1):
            errors.append(f"group {index} request count differs")
        if len(group.get("provider_arrivals", [])) != concurrent * (warmup + measured + 2):
            errors.append(f"group {index} warm/measured topology differs")
    for name, report in reports(artifact):
        if report.get("counters", {}).get("timing_samples_dropped") != 0:
            errors.append(f"{name}: timing observer dropped samples")
    for kind in ("context_preparation_to_request", "commit_publication"):
        if not timing_values(artifact, kind):
            errors.append(f"actual {kind} timing samples are absent")
    return errors


def reports(artifact: dict):
    for name in ("loaded_preview", "enqueue_commit_publication"):
        yield name, artifact.get(name, {}).get("actual_io", {})
    for index, group in enumerate(artifact.get("scripted_dispatch", {}).get("groups", [])):
        yield f"scripted_group_{index}", group.get("actual_io", {})


def timing_values(artifact: dict, kind: str) -> list[int]:
    # Enqueue and dispatch intervals are disjoint; setup/preview timings are excluded.
    return [sample["elapsed_micros"] * 1000 for name, report in reports(artifact)
            if name != "loaded_preview" for sample in report.get("timing_samples", [])
            if sample.get("kind") == kind]


def percentile(values: list[int], p: float) -> int | None:
    if not values:
        return None
    values = sorted(values)
    return values[math.ceil(len(values) * p) - 1]


def semantic_signature(artifact: dict) -> dict:
    return {
        "workload_version": artifact["workload_version"],
        "case": artifact["case"],
        "sampling": artifact["sampling"],
        "fixture": artifact["fixture"],
        "request_topology": [[{
            key: arrival[key] for key in (
                "agent_id", "ordinal", "phase", "request_kind", "tool_count", "conversation_messages",
                "current_operator_root_present", "prompt_shape_sha256")
        } for arrival in sorted(group["provider_arrivals"],
                                key=lambda item: (item["agent_id"], item["ordinal"]))]
            for group in artifact["scripted_dispatch"]["groups"]],
    }


def metrics(artifact: dict) -> dict:
    groups = artifact["scripted_dispatch"]["groups"]
    return {
        "context_request_p95_ns": percentile(timing_values(artifact, "context_preparation_to_request"), .95),
        "commit_publication_p95_ns": percentile(timing_values(artifact, "commit_publication"), .95),
        "constructed_requests_per_second": sum(g["measured_constructed_requests"] for g in groups)
            / (sum(g["wall_ns"] for g in groups) / 1e9),
        "context_request_samples": len(timing_values(artifact, "context_preparation_to_request")),
        "commit_publication_samples": len(timing_values(artifact, "commit_publication")),
        "setup_wall_ns": artifact["setup"]["wall_ns"],
        "loaded_preview_p95_ns": artifact["loaded_preview"]["latency"]["p95_ns"],
        "setup_process_lifetime_peak_rss_bytes": artifact["setup"]["memory"]["process_lifetime_peak_rss_bytes"],
    }


def binary_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(args) -> int:
    binary = args.binary.resolve(strict=True)
    root = Path(__file__).resolve().parents[2]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    manifest_path = output / f"{args.label}-manifest.json"
    if manifest_path.exists():
        raise ValueError(f"refusing to replace existing manifest {manifest_path}; use a new label or output directory")
    manifest = {"schema_version": "holon.agent_state.runs.v1", "label": args.label,
                "binary": str(binary), "binary_sha256": binary_digest(binary),
                "workload_version": VERSION, "runs": [],
                "limitations": ["Separate processes isolate cases; OS page cache is not controlled.",
                                "This driver does not build, change limits, or reduce failed workloads."]}
    failed = False
    for case in args.cases:
        for repetition in range(1, args.repeat + 1):
            stem = f"{args.label}-{case}-{repetition:02}"
            stdout, stderr = output / f"{stem}.stdout", output / f"{stem}.stderr"
            environment = os.environ | {"HOLON_STATE_BENCH_CASE": case}
            if args.warmup_per_agent is not None:
                environment["HOLON_STATE_BENCH_WARMUP_REQUESTS"] = str(args.warmup_per_agent)
            if args.measured_per_agent is not None:
                environment["HOLON_STATE_BENCH_MEASURED_REQUESTS"] = str(args.measured_per_agent)
            if stdout.exists() or stderr.exists():
                raise ValueError(f"refusing to replace existing run {stem}; use a fresh output directory or label")
            started = time.monotonic()
            entry = {"case": case, "repetition": repetition, "stdout": stdout.name, "stderr": stderr.name}
            print(f"D0 {case} repetition {repetition}: fixed complete workload", file=sys.stderr, flush=True)
            try:
                with stdout.open("w") as out, stderr.open("w") as err:
                    result = subprocess.run([str(binary)], cwd=root, env=environment, stdout=out, stderr=err,
                                            timeout=args.timeout, check=False)
                entry["exit_code"] = result.returncode
                if result.returncode:
                    raise RuntimeError(f"workload exited {result.returncode}; inspect stderr")
                artifact = json.loads(stdout.read_text())
                errors = validate(artifact)
                if errors:
                    raise ValueError("; ".join(errors))
                path = output / f"{stem}.json"
                write_json(path, artifact)
                entry.update({"artifact": path.name, "status": "measured", "metrics": metrics(artifact)})
            except (subprocess.TimeoutExpired, RuntimeError, ValueError, KeyError) as error:
                entry.update({"status": "failed", "error": str(error)})
                failed = True
            entry["wall_seconds_including_setup"] = time.monotonic() - started
            manifest["runs"].append(entry)
            write_json(manifest_path, manifest)
    return 1 if failed else 0


def load_directory(path: Path):
    cases = {case: [] for case in CASES}
    failures = {case: [] for case in CASES}
    for file in sorted(path.glob("*.json")):
        artifact = json.loads(file.read_text())
        if artifact.get("schema_version") == "holon.agent_state.runs.v1":
            for entry in artifact.get("runs", []):
                if entry.get("status") != "measured" and entry.get("case") in failures:
                    failures[entry["case"]].append({"manifest": str(file), "run": entry})
            continue
        if artifact.get("schema_version") != "holon.agent_state.workload.v1":
            continue
        errors = validate(artifact)
        if errors:
            raise ValueError(f"{file}: {'; '.join(errors)}")
        cases[artifact["case"]["name"]].append(artifact)
    return cases, failures


def compare(args) -> int:
    (baseline, baseline_failures), (candidate, candidate_failures) = load_directory(args.baseline), load_directory(args.candidate)
    result = {"schema_version": "holon.agent_state.comparison.v1", "cases": {},
              "normal_thresholds": {"latency_p95_max_ratio": 1.10, "throughput_min_ratio": .95,
                                    "minimum_independent_runs": 3, "minimum_timing_samples_per_run": 100},
              "limitations": ["Prompt shape/topology and mandatory-root assertions are compatibility checks, not a full prompt-content golden.",
                              "Pressure, startup, RSS, and missing resident/pin metrics are reported separately and do not imply a pass.",
                              "A failed/OOM/timed-out case must remain in its run manifest; successful artifacts cannot erase it."]}
    normal_pass = False
    for case in CASES:
        left, right = baseline[case], candidate[case]
        if not left or not right:
            result["cases"][case] = {"status": "not_measured"}
            continue
        signatures = [semantic_signature(artifact) for artifact in left + right]
        compatible = all(signature == signatures[0] for signature in signatures)
        environment_keys = ("os", "arch", "profile", "rustc", "logical_cpus", "allocator", "network_mode", "host_kernel", "rustflags")
        environments = [{key: a["environment"].get(key) for key in environment_keys} for a in left + right]
        same_environment = all(value == environments[0] for value in environments)
        left_metrics, right_metrics = [metrics(a) for a in left], [metrics(a) for a in right]
        ratios = {}
        for key in ("context_request_p95_ns", "commit_publication_p95_ns", "constructed_requests_per_second",
                    "setup_wall_ns", "loaded_preview_p95_ns", "setup_process_lifetime_peak_rss_bytes"):
            a_values = [m[key] for m in left_metrics if m[key] is not None]
            b_values = [m[key] for m in right_metrics if m[key] is not None]
            a = statistics.median(a_values) if a_values else None
            b = statistics.median(b_values) if b_values else None
            ratios[key] = {"baseline_median": a, "candidate_median": b, "candidate_over_baseline": b / a if a and b is not None else None}
        sufficient = min(len(left), len(right)) >= 3 and all(
            m["context_request_samples"] >= 100 and m["commit_publication_samples"] >= 100
            for m in left_metrics + right_metrics)
        failed_runs = baseline_failures[case] + candidate_failures[case]
        passed = compatible and same_environment and sufficient and not failed_runs and all(
            ratios[k]["candidate_over_baseline"] is not None and ratios[k]["candidate_over_baseline"] <= 1.10
            for k in ("context_request_p95_ns", "commit_publication_p95_ns")) and (
            ratios["constructed_requests_per_second"]["candidate_over_baseline"] >= .95)
        result["cases"][case] = {"status": ("pass" if passed else "not_passed") if case == "normal" else "reported_only",
                                 "same_workload_and_request_shape": compatible, "same_environment": same_environment,
                                 "failed_runs": failed_runs, "sufficient_samples": sufficient,
                                 "baseline_runs": len(left), "candidate_runs": len(right), "metrics": ratios}
        if case == "normal":
            normal_pass = passed
    write_json(args.output, result)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if normal_pass else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    runner = subparsers.add_parser("run", help="execute an already-built optimized harness")
    runner.add_argument("--binary", type=Path, required=True)
    runner.add_argument("--output", type=Path, required=True)
    runner.add_argument("--label", default="baseline")
    runner.add_argument("--cases", nargs="+", choices=CASES, default=list(CASES))
    runner.add_argument("--repeat", type=int, default=1)
    runner.add_argument("--timeout", type=float, default=7200)
    runner.add_argument("--warmup-per-agent", type=int, help="pilot sample override; fixture dimensions stay fixed")
    runner.add_argument("--measured-per-agent", type=int, help="pilot sample override; fixture dimensions stay fixed")
    runner.set_defaults(function=run)
    comparison = subparsers.add_parser("compare", help="compare same-instrumentation artifact directories")
    comparison.add_argument("--baseline", type=Path, required=True)
    comparison.add_argument("--candidate", type=Path, required=True)
    comparison.add_argument("--output", type=Path, required=True)
    comparison.set_defaults(function=compare)
    args = parser.parse_args()
    if any(value is not None and not 1 <= value <= 1024 for value in (getattr(args, "warmup_per_agent", None), getattr(args, "measured_per_agent", None))):
        parser.error("sampling overrides must be between 1 and 1024")
    if getattr(args, "repeat", 1) < 1 or getattr(args, "timeout", 1) <= 0:
        parser.error("repeat and timeout must be positive")
    try:
        return args.function(args)
    except (ValueError, OSError, KeyError, statistics.StatisticsError) as error:
        parser.error(str(error))
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
