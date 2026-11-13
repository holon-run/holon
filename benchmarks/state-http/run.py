#!/usr/bin/env python3
"""Run saved State HTTP executables sequentially; preserve every failed cell."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys


def cases(matrix):
    if matrix == "pilot":
        return [("loaded-a1-c1-w50-idle", "loaded", 1, 1, 50, 0)]
    cells = []
    for mode in ("loaded", "cold"):
        for agents in (1, 8):
            for clients in (1, 10):
                for work in (0, 50, 200):
                    name = f"{mode}-a{agents}-c{clients}-w{work}-idle"
                    cells.append((name, mode, agents, clients, work, 0))
    for agents in (1, 8):
        for clients in (1, 10):
            name = f"loaded-a{agents}-c{clients}-w50-mutations"
            cells.append((name, "loaded", agents, clients, 50, 50))
    if matrix == "idle":
        return [cell for cell in cells if cell[-1] == 0]
    if matrix == "mutations":
        return [cell for cell in cells if cell[-1] != 0]
    return cells


def summary(artifact, expected_settings):
    if artifact["settings"] != expected_settings:
        raise ValueError("measurement settings do not match the planned cell")
    batches = artifact["batches"]
    if not batches or any(batch["aborted"] for batch in batches):
        raise ValueError("empty or unsuccessful measurement")
    if len(batches) != expected_settings["batches"]:
        raise ValueError("measured batch count does not match the planned cell")
    if any(
        batch["statuses"] != {"200": expected_settings["requests_per_batch"]}
        for batch in batches
    ):
        raise ValueError("successful sample count mismatch in a measured batch")
    count = expected_settings["requests_per_batch"] * len(batches)
    if artifact["settings"]["mutation_delay_ms_after_each_commit"] and any(
        batch["committed_work_mutations"] == 0 for batch in batches
    ):
        raise ValueError("mutation coverage requires a commit in every measured batch")
    result = {
        "requests": count,
        "p50_ns_batch_median": statistics.median(batch["http_p50_ns"] for batch in batches),
        "p95_ns_batch_median": statistics.median(batch["http_p95_ns"] for batch in batches),
        "cpu_us_per_request": sum(batch["process_cpu_us"] for batch in batches) / count,
        "rust_allocations_per_request": sum(batch["rust_allocations"] for batch in batches) / count,
        "rust_requested_bytes_per_request": sum(batch["rust_requested_bytes"] for batch in batches) / count,
        "response_bytes_p50_batch_median": statistics.median(
            batch["response_bytes_p50"] for batch in batches
        ),
        "committed_work_mutations": sum(batch["committed_work_mutations"] for batch in batches),
        "source_sha256": artifact["source_sha256"],
    }
    if len(batches) >= 3 and all(batch["http_p99_ns"] is not None for batch in batches):
        result["p99_ns_batch_median"] = statistics.median(
            batch["http_p99_ns"] for batch in batches
        )
    for metric in ("leaders", "joined_waiters", "cache_hits", "rejected"):
        result[metric] = sum(
            batch["gate_after"][metric] - batch["gate_before"][metric] for batch in batches
        )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-a", type=Path, required=True)
    parser.add_argument("--binary-b", type=Path)
    parser.add_argument("--order", choices=("AA", "ABBA"), default="AA")
    parser.add_argument("--matrix", choices=("pilot", "idle", "mutations", "full"), default="pilot")
    parser.add_argument(
        "--keep-going", action="store_true",
        help="diagnose other cells after a process failure; still exit unsuccessfully",
    )
    parser.add_argument("--requests", type=int, default=100)
    parser.add_argument("--batches", type=int, default=3)
    parser.add_argument("--pause-ms", type=int, default=10)
    parser.add_argument("--cell-timeout-seconds", type=int, default=240)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.order == "ABBA" and args.binary_b is None:
        parser.error("ABBA requires --binary-b")
    if not 1 <= args.requests <= 10000 or not 1 <= args.batches <= 10:
        parser.error("requests must be 1..10000 and batches 1..10")
    if not 0 <= args.pause_ms <= 1000 or args.cell_timeout_seconds < 1:
        parser.error("invalid pause or cell timeout")
    binaries = {"A": args.binary_a.resolve(strict=True)}
    if args.binary_b is not None:
        binaries["B"] = args.binary_b.resolve(strict=True)
    for binary in binaries.values():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"not an executable file: {binary}")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    fixture_home = output / "fixtures"
    fixture_home.mkdir()
    manifest = {
        "schema": "holon.state-http-comparison.v1",
        "platform": platform.platform(),
        "order": args.order,
        "matrix": args.matrix,
        "requests": args.requests,
        "batches": args.batches,
        "pause_ms": args.pause_ms,
        "keep_going": args.keep_going,
        "binaries": {
            arm: {"path": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
            for arm, binary in binaries.items()
        },
        "runs": [],
    }
    failed = False
    try:
        for name, mode, agents, clients, work, mutation_ms in cases(args.matrix):
            for position, arm in enumerate(args.order, 1):
                label = f"{name}-{position}-{arm}"
                env = dict(os.environ)
                env.update({
                    "TMPDIR": str(fixture_home),
                    "HOLON_STATE_LABEL": label,
                    "HOLON_STATE_MODE": mode,
                    "HOLON_STATE_AGENTS": str(agents),
                    "HOLON_STATE_CLIENTS": str(clients),
                    "HOLON_STATE_WORK_ITEMS": str(work),
                    "HOLON_STATE_MUTATION_MS": str(mutation_ms),
                    "HOLON_STATE_REQUESTS": str(args.requests),
                    "HOLON_STATE_BATCHES": str(args.batches),
                    "HOLON_STATE_PAUSE_MS": str(args.pause_ms),
                })
                record = {"case": name, "position": position, "arm": arm, "label": label}
                manifest["runs"].append(record)
                print(label, file=sys.stderr, flush=True)
                with (output / f"{label}.json").open("xb") as stdout, (
                    output / f"{label}.stderr"
                ).open("xb") as stderr:
                    try:
                        result = subprocess.run(
                            [str(binaries[arm])], env=env, stdout=stdout, stderr=stderr,
                            timeout=args.cell_timeout_seconds, check=False,
                        )
                        record["exit_status"] = result.returncode
                    except subprocess.TimeoutExpired:
                        record["failure"] = "outer harness timeout; fixture cleanup not guaranteed"
                        raise
                if result.returncode:
                    record["failure"] = f"{label} failed; inspect preserved stdout/stderr"
                    if not args.keep_going:
                        raise RuntimeError(record["failure"])
                    failed = True
                    continue
                artifact = json.loads((output / f"{label}.json").read_text())
                record["summary"] = summary(artifact, {
                    "mode": mode,
                    "agents": agents,
                    "clients": clients,
                    "work_items_per_agent": work,
                    "requests_per_batch": args.requests,
                    "batches": args.batches,
                    "pause_ms_per_client": args.pause_ms,
                    "mutation_delay_ms_after_each_commit": mutation_ms,
                })
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.TimeoutExpired) as error:
        manifest["failure"] = str(error)
        failed = True
    finally:
        if failed and "failure" not in manifest:
            manifest["failure"] = "unsuccessful cells; inspect per-run failures and preserved streams"
        # This artifact is independent of process exit status and survives partial runs.
        (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    if failed:
        print(manifest["failure"], file=sys.stderr)
        return 1
    print(output / "manifest.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
