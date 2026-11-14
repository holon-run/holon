#!/usr/bin/env python3
"""Build frozen offline benchmark executables and record their provenance."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

from run import BINARY_NAMES, ROOT, digest, execute, identity_digest, source_identity, write_json


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--toolchain", default="1.100.0")
    parser.add_argument("--timeout", type=int, default=3600)
    args = parser.parse_args()
    if args.timeout < 1:
        parser.error("timeout must be positive")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    source = source_identity()
    fingerprint = identity_digest(source)
    manifest = {
        "schema": "holon.runtime-baseline-build.v1", "status": "failed",
        "source": source, "source_sha256": fingerprint, "toolchain": args.toolchain,
        "profile": "release", "rustflags": "-D warnings", "jobs": 2, "binaries": {},
    }
    try:
        cargo = Path(subprocess.check_output(
            ["rustup", "which", "--toolchain", args.toolchain, "cargo"], text=True
        ).strip())
        rustc = cargo.with_name("rustc")
        env = os.environ | {
            "PATH": f'{cargo.parent}{os.pathsep}{os.environ.get("PATH", "")}',
            "RUSTC": str(rustc), "RUSTUP_TOOLCHAIN": args.toolchain,
            "CARGO_TARGET_DIR": str(args.target_dir.resolve()),
            "CARGO_PROFILE_RELEASE_DEBUG": "0",
            "CARGO_BUILD_JOBS": "2", "RUSTFLAGS": "-D warnings",
            "HOLON_BENCH_SOURCE_SHA256": fingerprint,
        }
        command = [str(cargo), "build", "--locked", "--release", "--message-format=json"]
        for name in BINARY_NAMES:
            command.extend(["--bench", name])
        manifest.update(
            command=command, target_dir=env["CARGO_TARGET_DIR"],
            rustc=subprocess.check_output([str(rustc), "-Vv"], text=True),
            profile_overrides={key: value for key, value in env.items()
                               if key.startswith("CARGO_PROFILE_")},
        )
        code, wall = execute(command, env, output / "cargo", args.timeout)
        manifest.update(exit_status=code, wall_seconds=wall)
        if code:
            raise ValueError("Cargo failed; inspect cargo.stderr and cargo.stdout")
        finished = False
        for line in (output / "cargo.stdout").read_text().splitlines():
            event = json.loads(line)
            if event["reason"] == "build-finished":
                finished = event["success"]
            if event["reason"] != "compiler-artifact" or not event.get("executable"):
                continue
            name = event["target"]["name"]
            if name not in BINARY_NAMES:
                continue
            if event["profile"]["debug_assertions"] or str(event["profile"]["opt_level"]) == "0":
                raise ValueError("Cargo produced an unoptimized executable")
            path = Path(event["executable"]).resolve(strict=True)
            manifest["binaries"][name] = {
                "path": str(path), "sha256": digest(path), "profile": event["profile"],
            }
        if not finished or set(manifest["binaries"]) != set(BINARY_NAMES):
            raise ValueError("missing build-finished or benchmark executable")
        if source_identity() != source:
            raise ValueError("source changed during build; not a valid frozen build")
        manifest["status"] = "built"
    except (OSError, ValueError, KeyError, TimeoutError, subprocess.CalledProcessError) as error:
        manifest["failure"] = str(error)
        print(str(error), file=sys.stderr)
    finally:
        write_json(output / "manifest.json", manifest)
    print(output / "manifest.json")
    return 0 if manifest["status"] == "built" else 1


if __name__ == "__main__":
    sys.exit(main())
