#!/usr/bin/env python3
"""Collect stable, machine-readable timing evidence for the CI workflow."""

from __future__ import annotations

import argparse
import json
import math
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = 1
TIMING_JOB_NAME = "CI Timing"
DEFAULT_BUDGETS_SECONDS = {
    "rust_job": 45 * 60,
    "coverage_job": 29 * 60,
    "total_runner": 130 * 60,
    "test_target": 90,
    "individual_test": 20,
}
RUNNING_TARGET_RE = re.compile(
    r"^\s*Running (?P<target>.+?)(?: \((?:[^()]*/)?target/[^()]+\))?\s*$"
)
DOC_TEST_RE = re.compile(r"^\s*Doc-tests (?P<crate>\S+)\s*$")
TEST_RESULT_RE = re.compile(
    r"^\s*test result: .+ finished in (?P<seconds>[0-9]+(?:\.[0-9]+)?)s\s*$"
)
GITHUB_LOG_TIMESTAMP_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\S+Z$")
ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
PRINTED_ANSI_ESCAPE_RE = re.compile(r"\^\[\[[0-9;]*[A-Za-z]")


def parse_timestamp(value: str | None) -> datetime | None:
    if not value:
        return None
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def duration_seconds(started_at: str | None, completed_at: str | None) -> float | None:
    started = parse_timestamp(started_at)
    completed = parse_timestamp(completed_at)
    if started is None or completed is None:
        return None
    return max(0.0, (completed - started).total_seconds())


def flatten_jobs(payload: Any) -> list[dict[str, Any]]:
    if isinstance(payload, dict):
        jobs = payload.get("jobs")
        if isinstance(jobs, list):
            return [job for job in jobs if isinstance(job, dict)]
        return []
    if isinstance(payload, list):
        flattened: list[dict[str, Any]] = []
        for page in payload:
            flattened.extend(flatten_jobs(page))
        return flattened
    return []


def load_jobs(path: Path) -> list[dict[str, Any]]:
    return flatten_jobs(json.loads(path.read_text()))


def parse_test_log(text: str) -> dict[str, Any]:
    targets: list[dict[str, Any]] = []
    current_target: str | None = None

    for line in text.splitlines():
        fields = line.split("\t", 2)
        if len(fields) == 3:
            timestamp, separator, cargo_line = fields[2].partition(" ")
            if GITHUB_LOG_TIMESTAMP_RE.match(timestamp):
                if fields[1] != "Test" or not separator:
                    continue
                line = cargo_line
        line = ANSI_ESCAPE_RE.sub("", line)
        line = PRINTED_ANSI_ESCAPE_RE.sub("", line)
        running = RUNNING_TARGET_RE.match(line)
        if running:
            current_target = running.group("target")
            continue
        doc_test = DOC_TEST_RE.match(line)
        if doc_test:
            current_target = f"Doc-tests {doc_test.group('crate')}"
            continue
        result = TEST_RESULT_RE.match(line)
        if result and current_target:
            targets.append(
                {
                    "name": current_target,
                    "seconds": float(result.group("seconds")),
                }
            )
            current_target = None

    targets.sort(key=lambda target: (-target["seconds"], target["name"]))
    slow_targets = [
        target
        for target in targets
        if target["seconds"] > DEFAULT_BUDGETS_SECONDS["test_target"]
    ]
    return {
        "schema_version": SCHEMA_VERSION,
        "target_count": len(targets),
        "total_reported_seconds": round(
            sum(target["seconds"] for target in targets), 3
        ),
        "targets": targets,
        "slow_targets": slow_targets,
        "individual_test_timing": {
            "status": "unavailable",
            "budget_seconds": DEFAULT_BUDGETS_SECONDS["individual_test"],
            "reason": "The stable Rust test harness does not report per-test durations.",
        },
    }


def job_summary(jobs: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    summaries = []
    for job in jobs:
        name = str(job.get("name", ""))
        if name == TIMING_JOB_NAME:
            continue
        seconds = duration_seconds(job.get("started_at"), job.get("completed_at"))
        steps = []
        for step in job.get("steps") or []:
            step_seconds = duration_seconds(
                step.get("started_at"), step.get("completed_at")
            )
            steps.append(
                {
                    "name": step.get("name"),
                    "status": step.get("status"),
                    "conclusion": step.get("conclusion"),
                    "seconds": step_seconds,
                }
            )
        summaries.append(
            {
                "name": name,
                "status": job.get("status"),
                "conclusion": job.get("conclusion"),
                "started_at": job.get("started_at"),
                "completed_at": job.get("completed_at"),
                "seconds": seconds,
                "steps": steps,
            }
        )
    return sorted(summaries, key=lambda job: job["name"])


def workflow_metrics(jobs: Iterable[dict[str, Any]]) -> dict[str, Any]:
    summaries = job_summary(jobs)
    timed_jobs = [job for job in summaries if job["seconds"] is not None]
    starts = [
        timestamp
        for job in timed_jobs
        if (timestamp := parse_timestamp(job["started_at"])) is not None
    ]
    completions = [
        timestamp
        for job in timed_jobs
        if (timestamp := parse_timestamp(job["completed_at"])) is not None
    ]
    by_name = {job["name"]: job for job in summaries}
    wall_seconds = None
    if starts and completions:
        wall_seconds = max(0.0, (max(completions) - min(starts)).total_seconds())
    return {
        "workflow_wall_seconds": wall_seconds,
        "total_runner_seconds": round(
            sum(job["seconds"] for job in timed_jobs), 3
        ),
        "rust_job_seconds": (by_name.get("Rust") or {}).get("seconds"),
        "coverage_job_seconds": (by_name.get("Coverage") or {}).get("seconds"),
        "jobs": summaries,
    }


def percentile(values: list[float], percentile_value: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    index = (len(ordered) - 1) * percentile_value
    lower = math.floor(index)
    upper = math.ceil(index)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (index - lower)


def historical_summary(history_paths: Iterable[Path]) -> dict[str, Any]:
    samples = [
        sample
        for path in history_paths
        if (sample := workflow_metrics(load_jobs(path)))["rust_job_seconds"] is not None
    ]
    fields = (
        "workflow_wall_seconds",
        "total_runner_seconds",
        "rust_job_seconds",
        "coverage_job_seconds",
    )
    metrics = {}
    for field in fields:
        values = [
            float(sample[field]) for sample in samples if sample[field] is not None
        ]
        metrics[field] = {
            "samples": len(values),
            "p50_seconds": percentile(values, 0.50),
            "p90_seconds": percentile(values, 0.90),
        }
    return {"run_count": len(samples), "metrics": metrics}


def budget_report(
    current: dict[str, Any], test_targets: dict[str, Any] | None
) -> dict[str, Any]:
    checks = []
    for metric, budget_key in (
        ("rust_job_seconds", "rust_job"),
        ("coverage_job_seconds", "coverage_job"),
        ("total_runner_seconds", "total_runner"),
    ):
        actual = current.get(metric)
        limit = DEFAULT_BUDGETS_SECONDS[budget_key]
        checks.append(
            {
                "metric": metric,
                "actual_seconds": actual,
                "budget_seconds": limit,
                "exceeded": actual is not None and actual > limit,
            }
        )
    slow_targets = (test_targets or {}).get("slow_targets", [])
    checks.append(
        {
            "metric": "test_target_seconds",
            "actual_seconds": max(
                (target["seconds"] for target in slow_targets), default=None
            ),
            "budget_seconds": DEFAULT_BUDGETS_SECONDS["test_target"],
            "exceeded": bool(slow_targets),
            "targets": slow_targets,
        }
    )
    return {
        "mode": "warning_only",
        "status": "warning" if any(check["exceeded"] for check in checks) else "ok",
        "checks": checks,
    }


def format_minutes(seconds: float | None) -> str:
    if seconds is None:
        return "n/a"
    return f"{seconds / 60:.2f}"


def render_summary(report: dict[str, Any]) -> str:
    current = report["current"]
    history = report["history"]["metrics"]
    lines = [
        "# CI Timing",
        "",
        f"- Cache status: `{report['cache_status']}`",
        f"- Budget mode: **{report['budgets']['mode']}**",
        f"- Budget result: **{report['budgets']['status']}**",
        "",
        "| Metric | Current (min) | History samples | p50 (min) | p90 (min) |",
        "|---|---:|---:|---:|---:|",
    ]
    for label, field in (
        ("Workflow wall", "workflow_wall_seconds"),
        ("Total runner", "total_runner_seconds"),
        ("Rust job", "rust_job_seconds"),
        ("Coverage job", "coverage_job_seconds"),
    ):
        historical = history[field]
        lines.append(
            f"| {label} | {format_minutes(current[field])} | "
            f"{historical['samples']} | "
            f"{format_minutes(historical['p50_seconds'])} | "
            f"{format_minutes(historical['p90_seconds'])} |"
        )

    lines.extend(["", "## Jobs", "", "| Job | Conclusion | Minutes |", "|---|---|---:|"])
    for job in current["jobs"]:
        lines.append(
            f"| {job['name']} | {job['conclusion'] or job['status']} | "
            f"{format_minutes(job['seconds'])} |"
        )

    targets = (report.get("rust_test_targets") or {}).get("targets", [])
    if targets:
        lines.extend(
            [
                "",
                "## Slow Rust test targets",
                "",
                "| Target | Seconds | Over 90s budget |",
                "|---|---:|---|",
            ]
        )
        for target in targets[:20]:
            lines.append(
                f"| `{target['name']}` | {target['seconds']:.2f} | "
                f"{'yes' if target['seconds'] > 90 else 'no'} |"
            )

    warnings = [
        check for check in report["budgets"]["checks"] if check["exceeded"]
    ]
    if warnings:
        lines.extend(["", "## Budget warnings", ""])
        for warning in warnings:
            lines.append(
                f"- `{warning['metric']}` exceeded "
                f"`{warning['budget_seconds']}s`; this phase does not block CI."
            )
    return "\n".join(lines) + "\n"


def command_parse_test_log(args: argparse.Namespace) -> None:
    report = parse_test_log(args.input.read_text(errors="replace"))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")


def command_summarize(args: argparse.Namespace) -> None:
    current = workflow_metrics(load_jobs(args.current_jobs))
    history_paths = sorted(args.history_dir.glob("*.json"))
    test_targets = None
    if args.rust_test_targets and args.rust_test_targets.exists():
        test_targets = json.loads(args.rust_test_targets.read_text())
    report = {
        "schema_version": SCHEMA_VERSION,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "cache_status": args.cache_status,
        "current": current,
        "history": historical_summary(history_paths),
        "rust_test_targets": test_targets,
    }
    report["budgets"] = budget_report(current, test_targets)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    summary = render_summary(report)
    if args.summary:
        args.summary.write_text(summary)
    print(summary, end="")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    test_log = subparsers.add_parser("parse-test-log")
    test_log.add_argument("--input", type=Path, required=True)
    test_log.add_argument("--output", type=Path, required=True)
    test_log.set_defaults(func=command_parse_test_log)

    summarize = subparsers.add_parser("summarize")
    summarize.add_argument("--current-jobs", type=Path, required=True)
    summarize.add_argument("--history-dir", type=Path, required=True)
    summarize.add_argument("--rust-test-targets", type=Path)
    summarize.add_argument("--output", type=Path, required=True)
    summarize.add_argument("--summary", type=Path)
    summarize.add_argument("--cache-status", default="not-configured")
    summarize.set_defaults(func=command_summarize)
    return parser


def main() -> None:
    args = build_parser().parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
