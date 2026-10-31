#!/usr/bin/env python3
"""Split the cargo test target set into explicit, coverage-complete shards.

Phase 3 of the CI optimization plan runs integration tests in parallel CI
matrix shards while keeping every ``cargo test --tests`` target assigned to
exactly one runner. Shards with explicit member lists are validated against
``cargo metadata``; unassigned targets fall through to the ``misc`` shard, so
a newly added test binary can never be silently skipped.

The ``lib`` unit-test binary is split the same way, but by test name instead
of by cargo target: the runner enumerates the real test list with
``cargo test --lib -- --list`` and hands each shard an exact, disjoint set of
test names. A module never spans two shards, so every test still runs exactly
once and the per-process test thread count stays unchanged.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
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

# Shards of the ``lib`` unit-test binary. Each shard runs a disjoint set of
# test names from the same binary.
LIB_SHARDS = ("lib-a", "lib-b")

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
    members = suite_members(suite, all_targets)
    command = ["cargo", "test"]
    if members is None:
        command += ["--lib", "--bins"]
    else:
        for name in members:
            command += ["--test", name]
    threads = test_threads or THREAD_DEFAULTS.get(suite, SERIAL_TEST_THREADS)
    return command + ["--", f"--test-threads={threads}"]


def lib_test_names() -> list[str]:
    """Return every unit-test name in the lib target, spelled as libtest does."""
    completed = subprocess.run(
        ["cargo", "test", "--lib", "--", "--list"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    names: list[str] = []
    for line in completed.stdout.splitlines():
        name, separator, kind = line.partition(": ")
        if separator and kind == "test" and name:
            names.append(name)
    return names


def partition_lib_tests(names: list[str], shard_count: int) -> list[list[str]]:
    """Split lib test names into shards by owning module, balanced by count.

    A module is never split across shards, so module-level fixtures stay in
    one process, and every name is handed to exactly one shard. Modules are
    placed largest first into the currently lightest shard, which keeps the
    shards close to equal in test count.
    """
    if shard_count < 1:
        raise ValueError("shard_count must be positive")
    modules: dict[str, list[str]] = {}
    for name in names:
        module = name.rsplit("::", 1)[0] if "::" in name else name
        modules.setdefault(module, []).append(name)
    shards: list[list[str]] = [[] for _ in range(shard_count)]
    loads = [0] * shard_count
    ordered = sorted(modules.items(), key=lambda item: (-len(item[1]), item[0]))
    for _module, members in ordered:
        target = min(range(shard_count), key=lambda index: (loads[index], index))
        shards[target].extend(members)
        loads[target] += len(members)
    return shards


def lib_shard_commands(
    shard: str, names: list[str], test_threads: str | None = None
) -> list[list[str]]:
    """Return the cargo commands that run one shard of the lib test binary."""
    if shard not in LIB_SHARDS:
        raise ValueError(f"unknown lib shard: {shard}")
    threads = test_threads or THREAD_DEFAULTS.get("lib", SERIAL_TEST_THREADS)
    shards = partition_lib_tests(names, len(LIB_SHARDS))
    selected = shards[LIB_SHARDS.index(shard)]
    if not selected:
        raise ValueError(f"lib shard {shard} has no tests to run")
    commands = [
        [
            "cargo",
            "test",
            "--lib",
            "--",
            "--exact",
            *selected,
            f"--test-threads={threads}",
        ]
    ]
    if shard == LIB_SHARDS[0]:
        # Binary unit tests are cheap; run them once, in the first shard.
        commands.append(["cargo", "test", "--bins", "--", f"--test-threads={threads}"])
    return commands


def suite_commands(
    suite: str, all_targets: list[str], test_threads: str | None
) -> list[list[str]]:
    """Return every cargo command a suite runs, in order."""
    if suite in LIB_SHARDS:
        return lib_shard_commands(suite, lib_test_names(), test_threads)
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
        names = lib_test_names()
        shards = partition_lib_tests(names, len(LIB_SHARDS))
        selected = shards[LIB_SHARDS.index(args.suite)]
        threads = args.test_threads or THREAD_DEFAULTS.get("lib", SERIAL_TEST_THREADS)
        print(
            " ".join(
                [
                    "cargo",
                    "test",
                    "--lib",
                    "--",
                    "--exact",
                    f"<{len(selected)} lib tests>",
                    f"--test-threads={threads}",
                ]
            )
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
                print(f"{suite}: one shard of the lib unit-test binary")
            else:
                print(f"{suite}: lib + bins unit tests")
        else:
            print(f"{suite}: {len(members)} test targets")
    print(f"total cargo test targets: {len(all_targets)}")
    print("coverage: explicit shards + concurrent + misc = full target set")
    print(
        "lib coverage: every lib test name is assigned to exactly one of "
        + ", ".join(LIB_SHARDS)
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
    return parser


def main() -> None:
    args = build_parser().parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
