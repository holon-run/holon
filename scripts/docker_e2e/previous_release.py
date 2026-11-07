"""Build a previous-release fixture from verified official binary assets."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile


ARCHIVE = "holon-linux-amd64.tar.gz"
DOCKERFILE = """FROM debian:trixie-slim
RUN apt-get update \\
    && apt-get install -y --no-install-recommends \\
       bash ca-certificates curl git gzip openssh-client tar \\
    && rm -rf /var/lib/apt/lists/* \\
    && useradd --create-home --home-dir /var/lib/holon --uid 10001 holon \\
    && mkdir -p /workspace \\
    && chown holon:holon /workspace
COPY holon /usr/local/bin/holon
RUN chmod +x /usr/local/bin/holon
ENV HOME=/var/lib/holon \\
    HOLON_HOME=/var/lib/holon \\
    HOLON_WORKSPACE_DIR=/workspace
WORKDIR /workspace
USER holon
EXPOSE 7878
ENTRYPOINT ["holon"]
CMD ["serve", "--listen", "0.0.0.0:7878"]
"""


def prepare(ref: str, *, repository: str = "holon-run/holon",
            image: str | None = None, expected_sha256: str | None = None) -> dict:
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", ref):
        raise ValueError("previous_ref must be a release tag such as v0.30.0")
    if not re.fullmatch(r"[\w.-]+/[\w.-]+", repository):
        raise ValueError("invalid release repository")
    if expected_sha256 is not None and not re.fullmatch(r"[0-9a-f]{64}", expected_sha256):
        raise ValueError("expected archive SHA-256 must contain 64 lowercase hex digits")
    image = image or f"holon-e2e-previous:{ref[1:]}"
    base_url = f"https://github.com/{repository}/releases/download/{ref}"
    with tempfile.TemporaryDirectory(prefix="holon-previous-release-") as temporary:
        work = Path(temporary)
        for name in ("checksums.txt", ARCHIVE):
            subprocess.run(
                ["curl", "-fsSL", "-o", str(work / name), f"{base_url}/{name}"],
                check=True,
            )
        matches = []
        for line in (work / "checksums.txt").read_text().splitlines():
            fields = line.split()
            if len(fields) == 2 and fields[1].removeprefix("*").removeprefix("./") == ARCHIVE:
                matches.append(fields[0])
        if len(matches) != 1 or not re.fullmatch(r"[0-9a-f]{64}", matches[0]):
            raise ValueError(f"checksums.txt must contain exactly one SHA-256 for {ARCHIVE}")
        checksum = matches[0]
        if expected_sha256 is not None and checksum != expected_sha256:
            raise ValueError("release checksum differs from the pinned archive SHA-256")
        digest = hashlib.sha256()
        with (work / ARCHIVE).open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        if digest.hexdigest() != checksum:
            raise ValueError("previous-release archive SHA-256 mismatch")
        # Extract only the regular executable; never unpack archive paths or links.
        with tarfile.open(work / ARCHIVE, "r:gz") as archive:
            members = [m for m in archive if m.name.removeprefix("./") == "holon"]
            if len(members) != 1 or not members[0].isfile():
                raise ValueError("previous-release archive must contain one regular holon binary")
            source = archive.extractfile(members[0])
            if source is None:
                raise ValueError("previous-release holon binary is unreadable")
            with source, (work / "holon").open("wb") as destination:
                shutil.copyfileobj(source, destination)
        (work / "holon").chmod(0o755)
        (work / "Dockerfile").write_text(DOCKERFILE)
        subprocess.run(["docker", "build", "--tag", image, str(work)], check=True)
        image_id = subprocess.run(
            ["docker", "image", "inspect", image, "--format", "{{.Id}}"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", image_id):
            raise ValueError("previous-release image has no valid local image identity")
    return {"image": image, "image_id": image_id, "source": "previous_ref",
            "ref": ref, "archive_sha256": checksum, "release_url": base_url}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY", "holon-run/holon"))
    parser.add_argument("--image")
    parser.add_argument("--expected-sha256")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    result = prepare(args.ref, repository=args.repository, image=args.image,
                     expected_sha256=args.expected_sha256)
    text = json.dumps(result, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text)
    print(text, end="")
    return 0
