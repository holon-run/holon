#!/usr/bin/env python3
"""Opt-in runtime baseline: history growth, State populations and complete D0.

No build, network provider, production home, PR gate or automatic retry occurs
here. Build once, save its provenance, then execute the frozen binaries.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
HISTORY_ROWS = (0, 2_000, 20_000, 100_000)
BINARY_NAMES = ("task_history_scale", "state_http", "agent_state_workload")
TASK_OPERATIONS = (
    "task.history.selected_result_by_id",
    "task.history.reverse_source_lookup",
    "task.history.record_by_id",
)
TASK_GAPS = ("task.output.head", "task.output.selected")


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def source_identity(root=ROOT):
    def git(*args):
        return subprocess.check_output(["git", *args], cwd=root)

    # HEAD plus tracked changes and untracked implementation files identify a
    # dirty development build, without hashing generated benchmark artifacts.
    untracked = git(
        "ls-files", "--others", "--exclude-standard", "-z",
        "--", "src", "crates", "benches", "benchmarks", "Cargo.toml", "Cargo.lock",
    )
    return {
        "commit": git("rev-parse", "HEAD").decode().strip(),
        "diff_sha256": hashlib.sha256(git("diff", "--binary", "HEAD")).hexdigest(),
        "untracked": {
            name: digest(root / name)
            for name in sorted(untracked.decode().split("\0")) if name
        },
    }


def identity_digest(identity):
    return hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def adapter(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def plan(profile):
    # Independent processes are the repeat unit. The normal/pressure D0 fixture
    # and its sampling policy are not reduced for a quick baseline.
    cells = [
        {"id": f"task-history-{rows}", "kind": "task", "history_rows": rows}
        for rows in HISTORY_ROWS
    ]
    cells.append({"id": "state-scale", "kind": "state", "matrix": "scale"})
    if profile == "full":
        cells.extend([
            {"id": "state-full", "kind": "state", "matrix": "full"},
            {"id": "d0-normal", "kind": "d0", "case": "normal"},
            {"id": "d0-pressure", "kind": "d0", "case": "pressure"},
        ])
    return cells


def terminate_group(process):
    # These are only this invocation's children, never another build.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
    # A driver can exit before its own child. Always terminate the group.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def execute(command, env, output, timeout):
    """Bound the entire driver process group, including nested fixture children."""
    started = time.monotonic()
    with output.with_suffix(".stdout").open("xb") as out, (
        output.with_suffix(".stderr")
    ).open("xb") as err:
        process = subprocess.Popen(
            command, cwd=ROOT, env=env, stdout=out, stderr=err, start_new_session=True,
        )
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            terminate_group(process)
            raise TimeoutError("process-group timeout; preserve fixture homes for inspection")
        if code == 124:
            terminate_group(process)
            raise TimeoutError("adapter timeout; process group terminated; preserve fixture homes")
    return code, time.monotonic() - started


def command_for(cell, binaries, output, requests, repeats, cell_timeout):
    if cell["kind"] == "task":
        return [binaries["task_history_scale"]["path"]]
    if cell["kind"] == "state":
        return [
            sys.executable, str(ROOT / "benchmarks/state-http/run.py"),
            "--binary-a", binaries["state_http"]["path"],
            "--matrix", cell["matrix"], "--order", "AA", "--keep-going",
            "--requests", str(requests), "--batches", "3",
            "--cell-timeout-seconds", str(cell_timeout), "--output", str(output),
        ]
    warmup, measured = (13, 32) if cell["case"] == "normal" else (4, 8)
    return [
        sys.executable, str(ROOT / "benchmarks/agent-state/run.py"), "run",
        "--binary", binaries["agent_state_workload"]["path"],
        "--output", str(output), "--label", cell["id"],
        "--cases", cell["case"], "--repeat", str(repeats),
        "--timeout", str(cell_timeout), "--warmup-per-agent", str(warmup),
        "--measured-per-agent", str(measured), "--require-ready-admission",
    ]


def validate_task(artifact, rows, samples, source_sha256):
    if artifact["schema"] != "holon.task-history-scale.v1":
        raise ValueError("unrecognized Task workload schema")
    fixture = artifact["fixture"]
    expected = {
        "history_rows": rows, "fixed_message_rows": 2, "total_message_rows": rows + 2,
        "task_rows": 4, "active_tasks": 2, "selected_records_per_read": 1,
        "warmup_repetitions": 1, "measured_repetitions": samples,
    }
    if any(type(fixture[key]) is not int or fixture[key] != value
           for key, value in expected.items()):
        raise ValueError("Task fixture population or repetitions differ from plan")
    if artifact["environment"]["debug_assertions"] is not False:
        raise ValueError("Task executable is not optimized")
    if artifact["environment"]["build_source_sha256"] != source_sha256:
        raise ValueError("Task compiled source fingerprint differs")
    if (artifact["coverage_status"] != "partial"
            or {gap["benchmark_id"] for gap in artifact["coverage_gaps"]} != set(TASK_GAPS)):
        raise ValueError("Task output coverage gaps must remain explicit")
    results = artifact["results"]
    if len(results) != len(TASK_OPERATIONS) or {item["id"] for item in results} != set(TASK_OPERATIONS):
        raise ValueError("missing, duplicate or unexpected Task operations")
    for item in results:
        if item["operations_per_sample"] != 1 or len(item["samples"]) != samples:
            raise ValueError("Task operation/sample count differs from plan")
        times = []
        for iteration, sample in enumerate(item["samples"]):
            value = sample["wall_time_ns"]
            if (sample["iteration"] != iteration or sample["exit_status"] != "success"
                    or type(value) is not int or value < 0):
                raise ValueError("invalid Task sample")
            times.append(value)
        times.sort()
        median = times[len(times) // 2]
        expected_summary = {
            "median_wall_time_ns": median, "min_wall_time_ns": times[0],
            "max_wall_time_ns": times[-1],
            "median_absolute_deviation_ns": sorted(abs(t - median) for t in times)[len(times) // 2],
        }
        if any(type(item["summary"][key]) is not int or item["summary"][key] != value
               for key, value in expected_summary.items()):
            raise ValueError("Task summary differs from validated samples")
    return {item["id"]: item["summary"] for item in results}


def collect(cell, output, record, build, args):
    if cell["kind"] == "task":
        artifact = json.loads(record.with_suffix(".stdout").read_text())
        return validate_task(artifact, cell["history_rows"], args.samples, build["source_sha256"])
    if cell["kind"] == "state":
        driver = adapter("state_suite_adapter", "benchmarks/state-http/run.py")
        manifest = json.loads((output / "manifest.json").read_text())
        expected = [(name, position) for name, *_ in driver.cases(cell["matrix"])
                    for position in (1, 2)]
        actual = [(item["case"], item["position"]) for item in manifest["runs"]]
        if actual != expected or any("summary" not in item for item in manifest["runs"]):
            raise ValueError("State matrix incomplete or contains unsuccessful cells")
        for item in manifest["runs"]:
            artifact = json.loads((output / f'{item["label"]}.json').read_text())
            if artifact["source_sha256"] != build["source_sha256"]:
                raise ValueError("State compiled source fingerprint differs")
        return manifest["runs"]
    manifest = json.loads((output / f'{cell["id"]}-manifest.json').read_text())
    if len(manifest["runs"]) != args.repeats or any(
        item["status"] != "measured" for item in manifest["runs"]
    ):
        raise ValueError("D0 incomplete or unsuccessful")
    if any(item["metrics"]["context_request_samples"] < 100 for item in manifest["runs"]):
        raise ValueError("D0 has fewer than 100 actual context timing samples")
    return manifest["runs"]


def run(args):
    build = json.loads(args.build_manifest.read_text())
    if build.get("schema") != "holon.runtime-baseline-build.v1" or build.get("status") != "built":
        raise ValueError("build provenance is incomplete or unsuccessful")
    current = source_identity()
    if build["source"] != current or build["source_sha256"] != identity_digest(current):
        raise ValueError("source changed since build; freeze and rebuild before measuring")
    if build["profile"] != "release":
        raise ValueError("baseline requires an optimized release build")
    required = BINARY_NAMES if args.profile == "full" else BINARY_NAMES[:2]
    for name in required:
        binary = build["binaries"][name]
        path = Path(binary["path"])
        if not path.is_absolute() or not os.access(path, os.X_OK) or digest(path) != binary["sha256"]:
            raise ValueError(f"missing or changed executable: {name}")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    fixtures = output / "fixtures"
    fixtures.mkdir()
    manifest = {
        "schema": "holon.runtime-baseline.v1", "profile": args.profile,
        "build": build, "platform": platform.platform(), "runs": [],
        "measurement_status": "incomplete", "timing_verdict": "reported_only",
        "limitations": [
            "One source baseline, not historical red/green or an A/B performance claim.",
            "AA repeats do not establish a cross-version regression threshold.",
            "In-process State transport; real socket/client soak and idle maintenance not measured.",
            "Synthetic fixtures; host/page cache and external load are not isolated.",
        ],
        "not_measured": [
            "historical-positive-controls-4105-4095-4059",
            "idle-maintenance-soak", "real-transport-client-amplification",
            *TASK_GAPS,
        ],
    }
    if args.profile != "full":
        manifest["not_measured"].extend(["state-full", "d0-normal", "d0-pressure"])
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("HOLON_BENCH_", "HOLON_STATE_"))}
    env.update({"TMPDIR": str(fixtures), "RUSTUP_TOOLCHAIN": build["toolchain"],
                "RUSTFLAGS": build["rustflags"], "HOLON_BENCH_SAMPLES": str(args.samples)})
    failed = False
    deadline = time.monotonic() + args.timeout
    try:
        for cell in plan(args.profile):
            # State's saved-binary driver has a fixed two-process AA order.
            repetitions = args.repeats if cell["kind"] == "task" else 1
            for repetition in range(1, repetitions + 1):
                label = f'{cell["id"]}-{repetition}'
                record = output / label
                destination = output / f"{label}-artifacts"
                item = {"id": cell["id"], "repetition": repetition,
                        "status": "not_measured", "stdout": f"{label}.stdout",
                        "stderr": f"{label}.stderr"}
                manifest["runs"].append(item)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    item["error"] = "suite deadline exhausted; no process started"
                    raise TimeoutError(item["error"])
                command = command_for(cell, build["binaries"], destination,
                                      args.requests, args.repeats,
                                      args.d0_timeout if cell["kind"] == "d0" else args.cell_timeout)
                env["HOLON_BENCH_HISTORY_ROWS"] = str(cell.get("history_rows", 0))
                print(label, file=sys.stderr, flush=True)
                item["command"] = command
                try:
                    timeout = min(remaining, args.cell_timeout) if cell["kind"] == "task" else remaining
                    code, wall = execute(command, env, record, timeout)
                    item.update(exit_status=code, wall_seconds_including_setup=wall)
                    if code:
                        raise ValueError("unsuccessful workload; inspect preserved streams/artifacts")
                    item["metrics"] = collect(cell, destination, record, build, args)
                    item["status"] = "measured"
                except TimeoutError as error:
                    item.update(status="invalid", error=str(error))
                    raise  # Do not run more cells while fixture cleanup is uncertain.
                except (OSError, ValueError, KeyError, TypeError) as error:
                    item.update(status="invalid", error=str(error))
                    failed = True
                finally:
                    write_json(output / "manifest.json", manifest)
        manifest["measurement_status"] = "failed" if failed else "complete"
    except (OSError, TimeoutError) as error:
        manifest["failure"] = str(error)
        manifest["measurement_status"] = "failed"
        failed = True
    finally:
        if source_identity() != current:
            manifest.update(measurement_status="invalid", failure="source changed while measuring")
            failed = True
        write_json(output / "manifest.json", manifest)
    print(output / "manifest.json")
    return 1 if failed else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--profile", choices=("scale", "full"), default="full")
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--requests", type=int, default=300)
    parser.add_argument("--cell-timeout", type=int, default=240)
    parser.add_argument("--d0-timeout", type=int, default=1800)
    parser.add_argument("--timeout", type=int, default=2700)
    args = parser.parse_args()
    if not (1 <= args.samples <= 20 and 2 <= args.repeats <= 10
            and 1 <= args.requests <= 10_000 and args.cell_timeout > 0
            and args.d0_timeout > 0 and args.timeout > 0):
        parser.error("invalid sampling or time budget")
    try:
        return run(args)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
