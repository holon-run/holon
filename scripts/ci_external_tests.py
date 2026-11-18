#!/usr/bin/env python3
"""Exact, reviewed external-dependency routing for the existing Cargo suites."""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
from collections import defaultdict
from collections.abc import Sequence
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "scripts/ci-external-tests.json"


def load_manifest(path: Path = MANIFEST) -> list[dict]:
    payload = json.loads(path.read_text())
    if payload["version"] != 1:
        raise ValueError("unsupported external test manifest version")
    entries = payload["tests"]
    if not entries:
        raise ValueError("external test manifest is empty")
    seen = set()
    sources = {}
    for entry in entries:
        key = (entry["manifest_path"], entry["target"], entry["name"])
        if key in seen:
            raise ValueError(f"duplicate external test: {key}")
        seen.add(key)
        if not entry["dependencies"] or not entry["platforms"] or not entry["reason"]:
            raise ValueError(f"missing dependency/platform/reason: {key}")
        if not set(entry["platforms"]) <= {"all", "unix", "linux", "darwin", "win32"}:
            raise ValueError(f"unknown platform: {key}")
        for field in ("manifest_path", "source"):
            path = Path(entry[field])
            if path.is_absolute() or ".." in path.parts or not (ROOT / path).is_file():
                raise ValueError(f"invalid {field}: {entry[field]}")
        function = entry["name"].rsplit("::", 1)[-1]
        if entry["source"] not in sources:
            sources[entry["source"]] = (ROOT / entry["source"]).read_text()
        if not re.search(rf"\bfn\s+{re.escape(function)}\s*\(", sources[entry["source"]]):
            raise ValueError(f"stale external test source: {key}")
    return entries


def applicable(entry: dict, system: str | None = None) -> bool:
    system = system or sys.platform
    platforms = entry["platforms"]
    return (
        "all" in platforms
        or system in platforms
        or ("unix" in platforms and system in ("linux", "darwin"))
    )


def entries_for(manifest_path: str, targets: list[str] | None = None) -> list[dict]:
    return [
        entry for entry in load_manifest()
        if entry["manifest_path"] == manifest_path
        and (targets is None or entry["target"] in targets)
    ]


def regular_command(command: list[str], manifest_path: str = "Cargo.toml",
                    targets: list[str] | None = None) -> list[str]:
    """Libtest applies --skip before both enumeration and execution.

    Keep target-specific skips out of unrelated mixed binaries. Full names are
    intentional; verify_partition rejects substring collisions and stale names.
    """
    names = sorted({entry["name"] for entry in entries_for(manifest_path, targets)})
    if not names:
        return command
    command = list(command)
    if "--" not in command:
        command.append("--")
    for name in names:
        command += ["--skip", name]
    return command


def parse_names(output: str) -> list[str]:
    return [
        line.removesuffix(": test")
        for line in output.splitlines()
        if line.endswith(": test")
    ]


def verify_partition(all_names: list[str], external: list[str]) -> tuple[list[str], list[str]]:
    """Every new test defaults to regular; audited entries alone are external."""
    if len(all_names) != len(set(all_names)) or len(external) != len(set(external)):
        raise ValueError("duplicate test names in routing input")
    missing = sorted(set(external) - set(all_names))
    if missing:
        raise ValueError(f"external manifest names absent from real libtest list: {missing}")
    regular = [name for name in all_names if not any(skip in name for skip in external)]
    if set(regular) & set(external) or set(regular) | set(external) != set(all_names):
        raise ValueError("libtest --skip substring collision: regular/external union is incomplete")
    return sorted(regular), sorted(external)


def verify_regular_target(manifest: str, target: str, group: str | None = None,
                          cargo_flags: Sequence[str] = ()) -> list[str]:
    """Enumerate only regular names; full partition audits belong to external runs."""
    if manifest == "Cargo.toml" and target == "lib" and group is None:
        configured = os.environ.get("HOLON_LIB_TEST_SHARD")
        if configured in ("a", "b", "c", "d"):
            group = configured
    entries = [
        entry for entry in entries_for(manifest, [target])
        if applicable(entry) and (not group or lib_group(entry["name"]) == group)
    ]
    command = ["cargo", "test", "--locked", "--manifest-path", manifest, *cargo_flags]
    command += ["--lib"] if target == "lib" else ["--test", target]
    env = dict(os.environ)
    if manifest == "Cargo.toml" and target == "lib":
        env["HOLON_LIB_TEST_SHARD"] = group or "all"
    filtered = subprocess.run(
        regular_command(command + ["--", "--list"], manifest, [target]),
        cwd=ROOT, env=env, stdout=subprocess.PIPE, text=True, check=True,
    )
    regular = parse_names(filtered.stdout)
    if len(regular) != len(set(regular)):
        raise ValueError(f"duplicate regular test names: {manifest}/{target}")
    if any(entry["name"] in name for entry in entries for name in regular):
        raise ValueError(f"external test leaked into regular enumeration: {manifest}/{target}")
    print(f"regular {manifest}/{target}/{group or 'all'}: "
          f"{len(regular)} tests; external names excluded before enumeration",
          file=sys.stderr)
    return sorted(regular)


def preflight(entries: list[dict]) -> None:
    dependencies = sorted({dep for entry in entries for dep in entry["dependencies"]})
    print(f"external fixtures: OS={platform.platform()} python={platform.python_version()}", flush=True)
    print(f"dependencies={','.join(dependencies)} cwd={ROOT}", flush=True)
    print(f"fixture TMPDIR={Path(tempfile.gettempdir()).resolve()}", flush=True)
    if "app-sdk" in dependencies:
        artifact = ROOT / "packages/app-sdk/dist/holon.js"
        if not artifact.is_file():
            raise ValueError(
                "missing external test fixture: packages/app-sdk/dist/holon.js; "
                "run make app-sdk before make test-external (tests were not skipped)"
            )
        print(f"app-sdk={artifact}", flush=True)
    # Tags describe fixture capabilities, not a demand to install stubbed CLIs.
    commands = set()
    for dependency in dependencies:
        if dependency in ("shell", "posix-shell"):
            commands.add("sh")
        elif dependency.startswith("cli:"):
            commands.add(dependency.removeprefix("cli:"))
        elif dependency in ("git", "sh", "bash", "sleep", "true", "mkfifo", "node", "python3"):
            commands.add(dependency)
    for command in sorted(commands):
        executable = shutil.which(command)
        if executable is None:
            raise ValueError(
                f"missing external test dependency: {command}; "
                "install it before running make test-external (tests were not skipped)"
            )
        print(f"{command}={executable}", flush=True)
        if command in ("git", "node", "bash"):
            subprocess.run([executable, "--version"], cwd=ROOT, check=True)


def lib_group(name: str) -> str:
    # One authoritative ownership mapping; import lazily to avoid an import cycle.
    try:
        from .ci_test_shards import lib_compile_group
    except ImportError:
        from ci_test_shards import lib_compile_group
    return lib_compile_group(name)


def run_external(args: argparse.Namespace) -> None:
    entries = [
        entry for entry in load_manifest()
        if applicable(entry)
        and (args.manifest_path is None or entry["manifest_path"] == args.manifest_path)
        and (args.target is None or entry["target"] == args.target)
        and (not args.lib_group or args.lib_group == "all"
             or entry["manifest_path"] != "Cargo.toml" or entry["target"] != "lib"
             or lib_group(entry["name"]) == args.lib_group)
        and (not args.name or entry["name"] in args.name)
    ]
    if not entries:
        raise ValueError("external selection is empty for this platform (not a successful run)")
    if args.name and set(args.name) != {entry["name"] for entry in entries}:
        raise ValueError("requested external test is unknown or unavailable on this platform")
    preflight(entries)
    groups = defaultdict(list)
    for entry in entries:
        group = lib_group(entry["name"]) if entry["manifest_path"] == "Cargo.toml" and entry["target"] == "lib" and args.lib_group != "all" else None
        groups[(entry["manifest_path"], entry["target"], group)].append(entry)
    failures = []
    report = []
    for (manifest, target, group), selected in sorted(groups.items()):
        command = ["cargo", "test", "--locked", "--manifest-path", manifest]
        command += ["--lib"] if target == "lib" else ["--test", target]
        env = dict(os.environ)
        # macOS's default TMPDIR may be a symlink; Git reports physical paths.
        env["TMPDIR"] = str(Path(tempfile.gettempdir()).resolve())
        if group:
            env["HOLON_LIB_TEST_SHARD"] = group
        elif manifest == "Cargo.toml" and target == "lib":
            env["HOLON_LIB_TEST_SHARD"] = "all"
        selected_names = sorted(entry["name"] for entry in selected)
        # Validate the complete target's ownership, not just this optional subset.
        listed = subprocess.run(command + ["--", "--list"], cwd=ROOT, env=env,
                                stdout=subprocess.PIPE, text=True, check=True)
        all_names = parse_names(listed.stdout)
        audited = [
            entry["name"] for entry in entries_for(manifest, [target])
            if applicable(entry) and (not group or lib_group(entry["name"]) == group)
        ]
        regular, external = verify_partition(all_names, audited)
        filtered = subprocess.run(
            regular_command(command + ["--", "--list"], manifest, [target]),
            cwd=ROOT, env=env, stdout=subprocess.PIPE, text=True, check=True,
        )
        if sorted(parse_names(filtered.stdout)) != regular:
            raise ValueError(f"actual regular enumeration differs from partition: {manifest}/{target}")
        command += ["--", "--exact", "--test-threads=1", *selected_names]
        print(f"$ HOLON_LIB_TEST_SHARD={group or 'all'} {' '.join(command)}", flush=True)
        completed = subprocess.run(command, cwd=ROOT, env=env, stdout=subprocess.PIPE, text=True)
        print(completed.stdout, end="", flush=True)
        summary = re.search(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;",
                            completed.stdout)
        if completed.returncode == 0 and (
            not summary or int(summary[1]) + int(summary[2]) != len(selected_names)
            or int(summary[3]) != 0
        ):
            raise ValueError(f"external execution did not run every selected test: {manifest}/{target}")
        report.append({
            "manifest_path": manifest, "target": target, "lib_group": group,
            "platform": sys.platform, "baseline_count": len(all_names),
            "regular_count": len(regular), "external_count": len(external),
            "selected_names": selected_names, "exit_status": completed.returncode,
            "os": platform.platform(), "fixture_tmpdir": env["TMPDIR"],
            "sources": sorted({entry["source"] for entry in selected}),
        })
        if completed.returncode:
            failures.append(completed.returncode)
        if args.report:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(f"external: {len(entries)} selected tests, {len(groups)} targets/groups, "
          f"{len(failures)} failed groups", flush=True)
    if failures:
        raise SystemExit(failures[0])


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run")
    run.add_argument("--manifest-path")
    run.add_argument("--target")
    run.add_argument("--name", action="append")
    run.add_argument("--lib-group", choices=("all", "a", "b", "c", "d"))
    run.add_argument("--report", type=Path)
    validate = sub.add_parser("validate")
    validate.set_defaults(command="validate")
    crate = sub.add_parser("crate")
    crate.add_argument("manifest_path")
    crate.add_argument("--features")
    regular = sub.add_parser("regular")
    regular.add_argument("--target", required=True)
    regular.add_argument("--filter")
    regular.add_argument("--test-threads")
    args = parser.parse_args()
    try:
        if args.command == "validate":
            entries = load_manifest()
            print(f"external manifest: {len(entries)} exact test names; new unlisted tests are regular")
        elif args.command in ("crate", "regular"):
            manifest = args.manifest_path if args.command == "crate" else "Cargo.toml"
            targets = None if args.command == "crate" else [args.target]
            cargo_flags = ["--features", args.features] if args.command == "crate" and args.features else []
            command = ["cargo", "test", "--locked", "--manifest-path", manifest, *cargo_flags]
            if targets:
                command += ["--lib"] if args.target == "lib" else ["--test", args.target]
                if args.filter:
                    command.append(args.filter)
                if args.test_threads:
                    command += ["--", f"--test-threads={args.test_threads}"]
            affected = {entry["target"] for entry in entries_for(manifest, targets)}
            for target in sorted(affected):
                verify_regular_target(manifest, target, cargo_flags=cargo_flags)
            command = regular_command(
                command, manifest, targets,
            )
            raise SystemExit(subprocess.run(command, cwd=ROOT).returncode)
        else:
            run_external(args)
    except (ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"external test routing: {error}\n")


if __name__ == "__main__":
    main()
