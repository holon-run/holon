#!/usr/bin/env python3
import argparse
import json
from pathlib import Path


EXPECTED = {
    "runtime_db.json": {
        "samples": 5,
        "limits_ns": {
            "runtime_db.open_and_migrate.fresh": 5_000_000_000,
            "projection.agent_summary_storage.50_briefs_100_events": 2_000_000_000,
        },
    },
    "scheduler.json": {
        "samples": 5,
        "limits_ns": {
            "scheduler.work_queue_read_model.200": 5_000_000_000,
            "scheduler.due_rechecks.50": 2_000_000_000,
            "scheduler.active_waits.50": 5_000_000_000,
        },
    },
    "http_control.json": {
        "samples": 5,
        "limits_ns": {
            "http.runtime_performance.100": 2_000_000_000,
            "http.work_items.200x10": 10_000_000_000,
        },
    },
    "memory_lifecycle.json": {
        "samples": 3,
        "limits_ns": {
            "memory.long_session.events_1000_briefs_100": 30_000_000_000,
        },
    },
}

MAX_MEMORY_DELTA_KB = 64 * 1024
MAX_DATABASE_BYTES = 128 * 1024 * 1024


def load_artifact(path: Path) -> dict:
    artifact = json.loads(path.read_text())
    if artifact.get("schema_version") != "holon.performance.v0":
        raise AssertionError(f"{path}: unsupported schema")
    return artifact


def check_artifact(path: Path, expected: dict) -> list[str]:
    artifact = load_artifact(path)
    results = {result["benchmark_id"]: result for result in artifact.get("results", [])}
    if set(results) != set(expected["limits_ns"]):
        raise AssertionError(
            f"{path}: expected {sorted(expected['limits_ns'])}, got {sorted(results)}"
        )

    lines = []
    for benchmark_id, limit_ns in expected["limits_ns"].items():
        result = results[benchmark_id]
        samples = result.get("samples", [])
        if len(samples) != expected["samples"]:
            raise AssertionError(
                f"{benchmark_id}: expected {expected['samples']} samples, got {len(samples)}"
            )
        if any(sample.get("exit_status") != "ok" for sample in samples):
            raise AssertionError(f"{benchmark_id}: non-ok sample")
        median_ns = result["summary"]["median_wall_time_ns"]
        if median_ns > limit_ns:
            raise AssertionError(
                f"{benchmark_id}: median {median_ns} ns exceeds {limit_ns} ns"
            )
        lines.append(f"- `{benchmark_id}`: {median_ns / 1_000_000:.3f} ms")

        if benchmark_id.startswith("memory."):
            for sample in samples:
                rss = [
                    sample[key]
                    for key in (
                        "baseline_rss_kb",
                        "after_write_rss_kb",
                        "after_projection_rss_kb",
                        "after_drop_rss_kb",
                    )
                ]
                if sample["peak_rss_kb"] < max(rss):
                    raise AssertionError(f"{benchmark_id}: peak RSS is below current RSS")
                if sample["retained_rss_delta_kb"] > MAX_MEMORY_DELTA_KB:
                    raise AssertionError(f"{benchmark_id}: retained RSS delta exceeds 64 MiB")
                if sample["peak_rss_delta_kb"] > MAX_MEMORY_DELTA_KB:
                    raise AssertionError(f"{benchmark_id}: peak RSS delta exceeds 64 MiB")
                if sample["database_bytes"] > MAX_DATABASE_BYTES:
                    raise AssertionError(f"{benchmark_id}: database exceeds 128 MiB")
    return lines


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifact_dir", type=Path)
    args = parser.parse_args()

    summary = ["## Rust performance gate", ""]
    for filename, expected in EXPECTED.items():
        summary.extend(check_artifact(args.artifact_dir / filename, expected))
    print("\n".join(summary))


if __name__ == "__main__":
    main()
