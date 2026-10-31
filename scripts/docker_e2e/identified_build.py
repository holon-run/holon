"""Prepare clean, real Git metadata for revision-checked acceptance images."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import subprocess


FAST_BUILD_ANCHOR = "FROM rust:bookworm AS rust-builder\n"
FAST_BUILD_BLOCK = """FROM rust:bookworm AS rust-builder
# CI acceptance images only: parallelize codegen, strip release symbols, and
# link with lld so a per-PR acceptance build does not pay the default
# single-linker release profile. The published image keeps the stock profile.
ENV CARGO_PROFILE_RELEASE_CODEGEN_UNITS=64 \\
    CARGO_PROFILE_RELEASE_STRIP=symbols \\
    RUSTFLAGS="-C link-arg=-fuse-ld=lld"
RUN apt-get update \\
    && apt-get install -y --no-install-recommends lld \\
    && rm -rf /var/lib/apt/lists/*
"""


def apply_fast_acceptance_build(text: str) -> str:
    """Tune an acceptance Dockerfile for build time only.

    The published release image is built from the unmodified Dockerfile. CI
    acceptance images are rebuilt for every pull request, so they trade a
    little cross-crate optimization for a much shorter build.
    """
    if text.count(FAST_BUILD_ANCHOR) != 1:
        raise ValueError(
            "fast acceptance build requires exactly one Rust builder stage"
        )
    return text.replace(FAST_BUILD_ANCHOR, FAST_BUILD_BLOCK, 1)


def git(source, *args):
    return subprocess.run(
        ["git", "-C", str(source), *map(str, args)],
        text=True, capture_output=True, check=True,
    ).stdout.strip()


def prepare(
    source: Path,
    destination: Path,
    *,
    recovery_acceptance=False,
    fast_acceptance_build=False,
) -> dict:
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
    # Shallow clones otherwise default to seven digits, below require_build's minimum.
    git(context, "config", "--local", "core.abbrev", "12")
    if git(context, "status", "--porcelain") or git(context, "rev-parse", "HEAD") != revision:
        raise ValueError("identified build context does not match its clean source revision")
    dockerfile = destination / "Dockerfile"
    shutil.copyfile(context / "Dockerfile", dockerfile)
    if recovery_acceptance:
        text = dockerfile.read_text()
        needle = "cargo build --release --locked"
        if text.count(needle) != 1:
            raise ValueError("acceptance image requires exactly one standard Cargo build")
        dockerfile.write_text(text.replace(needle, needle + " --features recovery-acceptance"))
    if fast_acceptance_build:
        dockerfile.write_text(apply_fast_acceptance_build(dockerfile.read_text()))
    # Dockerfile-specific rules override the stock .dockerignore. All tracked
    # files and .git must be present: omitting tracked files makes Git dirty.
    # A zero-byte override makes Docker fall back to the context's ignore file.
    (destination / "Dockerfile.dockerignore").write_text("# Include the complete clean Git context.\n")
    result = {"revision": revision, "context": str(context), "dockerfile": str(dockerfile)}
    result["recovery_acceptance"] = recovery_acceptance
    result["fast_acceptance_build"] = fast_acceptance_build
    (destination / "source.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--recovery-acceptance", action="store_true",
                        help="build the distinctly marked, compile-time recovery test binary")
    parser.add_argument("--fast-acceptance-build", action="store_true",
                        help="tune the acceptance image build for compile time only")
    args = parser.parse_args(argv)
    print(json.dumps(prepare(args.source, args.destination,
                             recovery_acceptance=args.recovery_acceptance,
                             fast_acceptance_build=args.fast_acceptance_build)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
