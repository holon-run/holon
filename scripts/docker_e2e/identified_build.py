"""Prepare clean, real Git metadata for revision-checked acceptance images."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import subprocess


def git(source, *args):
    return subprocess.run(
        ["git", "-C", str(source), *map(str, args)],
        text=True, capture_output=True, check=True,
    ).stdout.strip()


def prepare(source: Path, destination: Path) -> dict[str, str]:
    source, destination = source.resolve(), destination.resolve()
    if git(source, "status", "--porcelain"):
        raise ValueError("identified image requires a clean committed source checkout")
    revision = git(source, "rev-parse", "HEAD")
    destination.mkdir(parents=True, exist_ok=False)
    context = destination / "context"
    # A real clone also works when the source is a linked worktree. Do not copy
    # its host-local .git pointer, credentials, hooks, or object alternates.
    git(source, "clone", "--no-local", "--depth", "1", "--no-checkout", source, context)
    git(context, "checkout", "--detach", revision)
    if git(context, "status", "--porcelain") or git(context, "rev-parse", "HEAD") != revision:
        raise ValueError("identified build context does not match its clean source revision")
    dockerfile = destination / "Dockerfile"
    shutil.copyfile(context / "Dockerfile", dockerfile)
    # Dockerfile-specific rules override the stock .dockerignore. All tracked
    # files and .git must be present: omitting tracked files makes Git dirty.
    (destination / "Dockerfile.dockerignore").write_text("")
    result = {"revision": revision, "context": str(context), "dockerfile": str(dockerfile)}
    (destination / "source.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args(argv)
    print(json.dumps(prepare(args.source, args.destination)))
    return 0
