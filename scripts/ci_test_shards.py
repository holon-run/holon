#!/usr/bin/env python3
"""Split the cargo test target set into explicit, coverage-complete shards.

Phase 3 of the CI optimization plan runs integration tests in parallel CI
matrix shards while keeping every ``cargo test --tests`` target assigned to
exactly one runner. Shards with explicit member lists are validated against
``cargo metadata``; unassigned targets fall through to the ``misc`` shard, so
a newly added test binary can never be silently skipped.

Library tests are split at compile time, before libtest enumeration. Each CI
runner builds and runs two disjoint groups, keeping private APIs and shared
fixtures in the library. No selector still builds the complete local suite.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from collections import Counter
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# Binaries already executed with Rust's default test threads by
# ``make test-concurrent`` (the Rust Concurrent job). They are excluded from
# the serial suites; tests/test_ci_test_shards.py keeps this set in sync with
# CONCURRENT_TESTS in the Makefile.
CONCURRENT_TEST_TARGETS = frozenset(
    {
        "runtime_tasks",
        "runtime_waiting_and_reactivation",
        "runtime_waiting_and_delivery_regressions",
        "http_events",
        "http_tasks",
        "wt204_parallel_worktree_workflow",
    }
)

# HTTP control-plane suites (slowest integration binary lives here).
CONTROL_SHARD = (
    "http_control",
    "http_client",
    "http_workspace",
    "http_ingress",
    "http_callback",
    "http_operator_transport",
)

# CLI contracts, snapshot gates, and the `holon run` end-to-end suite.
CLI_SHARD = (
    "cli_json_contract",
    "daemon_serve_profile",
    "run_once",
    "cli_snapshot",
    "openapi_snapshot",
    "http_route_snapshot",
    "runtime_status_inventory_snapshot",
    "tool_schema_inventory_snapshot",
    "cli_exit_codes",
    "release_version_contract",
    "models_dev_adapter",
)

EXPLICIT_TARGETS = frozenset(CONCURRENT_TEST_TARGETS)
EXPLICIT_TARGETS |= frozenset(CONTROL_SHARD)
EXPLICIT_TARGETS |= frozenset(CLI_SHARD)

SUITES = (
    "serial",
    "lib",
    "lib-a",
    "lib-b",
    "concurrent",
    "control",
    "cli",
    "misc",
)

# Two CI jobs each build two smaller libtest binaries, serially.
LIB_SHARDS = ("lib-a", "lib-b")
LIB_COMPILE_GROUPS = {"lib-a": ("a", "c"), "lib-b": ("b", "d")}
LIB_C_OWNERS = frozenset(
    {
        "agent_template", "auth", "calendar", "client", "cloudflare_tunnel",
        "contract_inventory", "daemon", "domain", "fd_limit", "github",
        "github_archive", "host_registry", "http", "http_dto", "memory",
        "model_catalog", "model_config_migration", "model_discovery",
        "object_query_cache", "object_resolver", "oidc", "otlp_exporter",
        "policy", "presentation", "provider", "runtime_event", "skills",
        "solve", "startup", "system", "tool", "tui_markdown", "utf8",
        "wake_contract", "x_search",
    }
)


def lib_compile_group(name: str) -> str:
    owner = name.split("::", 1)[0]
    if owner == "runtime":
        return "a"
    if owner == "runtime_db":
        return "b"
    return "c" if owner in LIB_C_OWNERS else "d"


def lib_group_environment(group: str) -> dict[str, str]:
    if group not in ("a", "b", "c", "d"):
        raise ValueError(f"unknown lib compile group: {group}")
    return {**os.environ, "HOLON_LIB_TEST_SHARD": group}


def validate_lib_group(names: list[str], group: str) -> None:
    if not names:
        raise ValueError(f"lib compile group {group} has no tests")
    if len(names) != len(set(names)):
        raise ValueError(f"lib compile group {group} contains duplicate tests")
    misplaced = [name for name in names if lib_compile_group(name) != group]
    if misplaced:
        raise ValueError(
            f"lib compile group {group} contains tests owned by another group "
            f"(missing or incorrect source cfg): {', '.join(misplaced[:10])}"
        )


def verify_lib_lists(baseline: list[str], groups: list[list[str]]) -> None:
    """Compare real libtest enumerations, not counts or inferred source names."""
    actual = [name for names in groups for name in names]
    duplicates = [name for name, count in Counter(actual).items() if count > 1]
    missing = sorted((Counter(baseline) - Counter(actual)).elements())
    extra = sorted((Counter(actual) - Counter(baseline)).elements())
    if not baseline or len(groups) != 4 or any(not group for group in groups):
        raise ValueError("coverage requires a nonempty baseline and all four nonempty lists")
    if duplicates or missing or extra:
        raise ValueError(
            f"lib coverage mismatch: duplicates={duplicates[:10]}, "
            f"missing={missing[:10]}, extra={extra[:10]}"
        )


def command_verify_lib_lists(args: argparse.Namespace) -> None:
    baseline = args.baseline.read_text().splitlines()
    groups = []
    for group in "abcd":
        names = json.loads((args.directory / f"lib-test-names-{group}.json").read_text())
        validate_lib_group(names, group)
        groups.append(names)
    verify_lib_lists(baseline, groups)
    print(f"lib coverage: {len(baseline)} original test names, no missing/extra/duplicates")

# Lib unit tests opt into 2 test threads after the Phase 3 acceptance runs;
# every other suite stays serial inside each binary and parallel across jobs.
THREAD_DEFAULTS = {"lib": "2"}
SERIAL_TEST_THREADS = "1"


def cargo_test_targets() -> list[str]:
    metadata = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    payload = json.loads(metadata)
    return sorted(
        target["name"]
        for package in payload["packages"]
        for target in package["targets"]
        if "test" in target.get("kind", [])
    )


def suite_members(suite: str, all_targets: list[str]) -> list[str] | None:
    """Return ``--test`` target names for a suite; ``None`` means lib+bins."""
    known = set(all_targets)
    if suite == "lib":
        return None
    if suite in LIB_SHARDS:
        return None
    if suite == "concurrent":
        return sorted(CONCURRENT_TEST_TARGETS)
    if suite == "control":
        return list(CONTROL_SHARD)
    if suite == "cli":
        return list(CLI_SHARD)
    if suite == "misc":
        return [name for name in all_targets if name not in EXPLICIT_TARGETS]
    if suite == "serial":
        return [
            name for name in all_targets if name not in CONCURRENT_TEST_TARGETS
        ]
    raise ValueError(f"unknown suite: {suite}")


def validation_problems(all_targets: list[str]) -> list[str]:
    known = set(all_targets)
    problems: list[str] = []
    for shard, members in (("concurrent", CONCURRENT_TEST_TARGETS), ("control", CONTROL_SHARD), ("cli", CLI_SHARD)):
        unknown = sorted(name for name in members if name not in known)
        if unknown:
            problems.append(
                f"{shard} shard lists non-existent test targets: {', '.join(unknown)}"
            )
    overlap = sorted(
        frozenset(CONTROL_SHARD)
        & frozenset(CLI_SHARD)
        | frozenset(CONTROL_SHARD) & CONCURRENT_TEST_TARGETS
        | frozenset(CLI_SHARD) & CONCURRENT_TEST_TARGETS
    )
    if overlap:
        problems.append(f"test targets assigned to multiple shards: {', '.join(overlap)}")
    return problems


def suite_command(suite: str, all_targets: list[str], test_threads: str | None) -> list[str]:
    if suite in LIB_SHARDS:
        raise ValueError("lib CI shards must use run_lib_shard, not a full libtest build")
    members = suite_members(suite, all_targets)
    command = ["cargo", "test"]
    if members is None:
        command += ["--lib", "--bins"]
    else:
        for name in members:
            command += ["--test", name]
    threads = test_threads or THREAD_DEFAULTS.get(suite, SERIAL_TEST_THREADS)
    return command + ["--", f"--test-threads={threads}"]


def lib_test_names(group: str | None = None) -> list[str]:
    """Return every unit-test name in the lib target, spelled as libtest does."""
    completed = subprocess.run(
        ["cargo", "test", "--lib", "--", "--list"],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
        env=lib_group_environment(group) if group else None,
    )
    names: list[str] = []
    for line in completed.stdout.splitlines():
        name, separator, kind = line.partition(": ")
        if separator and kind == "test" and name:
            names.append(name)
    return names


def run_lib_shard(shard: str, test_threads: str | None = None) -> None:
    threads = test_threads or THREAD_DEFAULTS["lib"]
    failures = []
    for group in LIB_COMPILE_GROUPS[shard]:
        names = lib_test_names(group)
        validate_lib_group(names, group)
        # Keep the real enumerations for cross-job union/overlap auditing.
        output = Path(os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "target"))
        if not output.is_absolute():
            output = REPO_ROOT / output
        output = output / "ci-timing"
        output.mkdir(parents=True, exist_ok=True)
        (output / f"lib-test-names-{group}.json").write_text(
            json.dumps(sorted(names), indent=2) + "\n"
        )
        print(f"lib compile group {group}: {len(names)} tests", file=sys.stderr)
        completed = subprocess.run(
            ["cargo", "test", "--lib", "--", f"--test-threads={threads}"],
            cwd=REPO_ROOT,
            env=lib_group_environment(group),
        )
        if completed.returncode:
            failures.append(completed)
    if shard == LIB_SHARDS[0]:
        # Binary unit tests are independent of the library's test-only cfg.
        completed = subprocess.run(
            ["cargo", "test", "--bins", "--", f"--test-threads={threads}"],
            cwd=REPO_ROOT,
            env=lib_group_environment(LIB_COMPILE_GROUPS[shard][-1]),
        )
        if completed.returncode:
            failures.append(completed)
    if failures:
        raise subprocess.CalledProcessError(failures[0].returncode, failures[0].args)


def suite_commands(
    suite: str, all_targets: list[str], test_threads: str | None
) -> list[list[str]]:
    """Return every cargo command a suite runs, in order."""
    return [suite_command(suite, all_targets, test_threads)]


def command_list(args: argparse.Namespace) -> None:
    all_targets = cargo_test_targets()
    problems = validation_problems(all_targets)
    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        sys.exit(1)
    members = suite_members(args.suite, all_targets)
    if members is None:
        print("(lib and binary unit tests; no --test targets)")
        return
    for name in members:
        print(name)


def command_args(args: argparse.Namespace) -> None:
    all_targets = cargo_test_targets()
    problems = validation_problems(all_targets)
    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        sys.exit(1)
    if args.suite in LIB_SHARDS:
        threads = args.test_threads or THREAD_DEFAULTS.get("lib", SERIAL_TEST_THREADS)
        for group in LIB_COMPILE_GROUPS[args.suite]:
            print(
                f"HOLON_LIB_TEST_SHARD={group} "
                f"cargo test --lib -- --test-threads={threads}"
            )
        return
    print(" ".join(suite_command(args.suite, all_targets, args.test_threads)))


def command_run(args: argparse.Namespace) -> None:
    all_targets = cargo_test_targets()
    problems = validation_problems(all_targets)
    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        sys.exit(1)
    if args.suite in LIB_SHARDS:
        run_lib_shard(args.suite, args.test_threads)
        return
    commands = suite_commands(args.suite, all_targets, args.test_threads)
    for command in commands:
        printable = " ".join(command)
        if len(printable) > 200:
            printable = f"{' '.join(command[:6])} <{len(command) - 7} test names>"
        print(f"$ {printable}", file=sys.stderr)
        completed = subprocess.run(command, cwd=REPO_ROOT)
        if completed.returncode != 0:
            sys.exit(completed.returncode)
    sys.exit(0)


def command_validate(_: argparse.Namespace) -> None:
    all_targets = cargo_test_targets()
    problems = validation_problems(all_targets)
    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        sys.exit(1)
    for suite in SUITES:
        members = suite_members(suite, all_targets)
        if members is None:
            if suite in LIB_SHARDS:
                print(f"{suite}: lib compile groups {LIB_COMPILE_GROUPS[suite]}")
            else:
                print(f"{suite}: lib + bins unit tests")
        else:
            print(f"{suite}: {len(members)} test targets")
    print(f"total cargo test targets: {len(all_targets)}")
    print("coverage: explicit shards + concurrent + misc = full target set")
    print(
        "lib ownership: source cfg and actual per-group lists are checked by "
        "the lib jobs; use verify-lib-lists for baseline union/overlap validation"
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    for name, func in (("list", command_list), ("args", command_args), ("run", command_run)):
        sub = subparsers.add_parser(name)
        sub.add_argument("suite", choices=SUITES)
        if name != "list":
            sub.add_argument("--test-threads", default=None)
        sub.set_defaults(func=func)

    validate = subparsers.add_parser("validate")
    validate.set_defaults(func=command_validate)
    verify = subparsers.add_parser("verify-lib-lists")
    verify.add_argument("--baseline", type=Path, required=True)
    verify.add_argument("--directory", type=Path, required=True)
    verify.set_defaults(func=command_verify_lib_lists)
    return parser


def main() -> None:
    args = build_parser().parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
