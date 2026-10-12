#!/usr/bin/env python3
"""Publish an explicit Android candidate without modifying other Play tracks."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent
PACKAGE = "run.holon.android"
TRACK = "alpha"
GROUP = "holon-pioneers@googlegroups.com"
API = f"https://androidpublisher.googleapis.com/androidpublisher/v3/applications/{PACKAGE}/edits"
UPLOAD = f"https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications/{PACKAGE}/edits"


class AlphaError(ValueError):
    """A sanitized publishing rejection suitable for CI logs."""


def version_data(version, root=ROOT):
    try:
        result = subprocess.run(
            ["bash", str(root / "scripts/android-release-version.sh"), version],
            check=True, capture_output=True, text=True,
        )
    except subprocess.CalledProcessError as error:
        # This local validator emits fixed rejection text, not API responses.
        raise AlphaError(error.stderr.strip() or "Android version validation failed.") from error
    data = dict(line.split("=", 1) for line in result.stdout.splitlines())
    return data["version_name"], int(data["version_code"])


def prepare(version, root=ROOT):
    name, code = version_data(version, root)
    cargo = root / "Cargo.toml"
    text = cargo.read_text()
    current = tomllib.loads(text)["package"]["version"]
    if code <= version_data(current, root)[1]:
        raise AlphaError("Candidate must be newer than the source Cargo.toml version.")
    # Match only the actual package version, not dependency or workspace versions.
    package = re.search(r"(?ms)^\[package\]\n.*?(?=^\[|\Z)", text)
    if not package:
        raise AlphaError("Cargo.toml package section not found.")
    section, count = re.subn(
        r'^version = "' + re.escape(current) + r'"$',
        f'version = "{name}"', package.group(), count=1, flags=re.MULTILINE,
    )
    if count != 1:
        raise AlphaError("Cargo.toml package version did not match.")
    cargo.write_text(text[:package.start()] + section + text[package.end():])
    return name, code


def request(session, method, url, **kwargs):
    response = session.request(method, url, timeout=300, **kwargs)
    if not response.ok:
        # Do not dump credentials, auth headers, or raw API responses into CI logs.
        raise AlphaError(f"Publisher API {method} failed (HTTP {response.status_code}).")
    return response.json() if response.content else {}


def snapshot(session, edit):
    tracks = request(session, "GET", f"{API}/{edit}/tracks")["tracks"]
    testers = request(session, "GET", f"{API}/{edit}/testers/{TRACK}")
    return {track["track"]: track for track in tracks}, testers


def track_mismatches(actual, expected):
    """Compare release semantics; report fixed field names, never API values."""
    differences = []
    if {k: v for k, v in actual.items() if k != "releases"} != {
        k: v for k, v in expected.items() if k != "releases"
    }:
        differences.append("track metadata")
    observed, wanted = actual.get("releases", []), expected.get("releases", [])
    if len(observed) != len(wanted):
        return differences + ["release count"]
    fields = ("name", "versionCodes", "status", "releaseNotes", "inAppUpdatePriority")
    for release, candidate in zip(observed, wanted):
        for field in fields:
            # Publisher defines an omitted inAppUpdatePriority as zero.
            default = 0 if field == "inAppUpdatePriority" else None
            if release.get(field, default) != candidate.get(field, default):
                differences.append(field)
        if {k: v for k, v in release.items() if k not in fields} != {
            k: v for k, v in candidate.items() if k not in fields
        }:
            differences.append("other release fields")
    return sorted(set(differences))


def discard_edit(session, edit):
    try:
        request(session, "DELETE", f"{API}/{edit}")
    except Exception as error:
        # Cleanup cannot override a committed result or the original failure.
        print(
            f"Warning: temporary Play edit cleanup failed ({type(error).__name__}); "
            "inspect Console for abandoned edits.",
            file=sys.stderr,
        )


def publish(session, bundle, name, code, status, notes):
    if status not in ("draft", "completed"):
        raise AlphaError("Only draft or completed Alpha releases are allowed.")
    if not notes.strip() or len(notes) > 500:
        raise AlphaError("Provide nonempty release notes of at most 500 characters.")
    edit = request(session, "POST", API, json={})["id"]
    committed = False
    try:
        before, testers = snapshot(session, edit)
        if TRACK not in before or GROUP not in testers.get("googleGroups", []):
            raise AlphaError("Existing Alpha with Holon Pioneers is required.")
        releases = before[TRACK].get("releases", [])
        if any(release.get("status") != "completed" for release in releases):
            raise AlphaError("Resolve the existing Alpha draft or rollout before uploading.")
        used = []
        for kind in ("bundles", "apks"):
            artifacts = request(session, "GET", f"{API}/{edit}/{kind}")
            used.extend(int(item["versionCode"]) for item in artifacts.get(kind, []))
        if used and code <= max(used):
            raise AlphaError("Candidate versionCode must be newer than all existing Play artifacts.")
        digest = hashlib.sha256(bundle.read_bytes()).hexdigest()
        with bundle.open("rb") as stream:
            uploaded = request(
                session, "POST", f"{UPLOAD}/{edit}/bundles?uploadType=media",
                data=stream, headers={"Content-Type": "application/octet-stream"},
            )
        if int(uploaded["versionCode"]) != code or uploaded["sha256"] != digest:
            raise AlphaError("Uploaded bundle versionCode or SHA-256 mismatch.")
        candidate = {
            "name": f"v{name}", "versionCodes": [str(code)], "status": status,
            "inAppUpdatePriority": 0, "releaseNotes": [{"language": "en-US", "text": notes}],
        }
        # A draft must not replace the currently serving completed release.
        expected = {"track": TRACK, "releases": (releases if status == "draft" else []) + [candidate]}
        request(session, "PUT", f"{API}/{edit}/tracks/{TRACK}", json=expected)
        after, after_testers = snapshot(session, edit)
        differences = track_mismatches(after.get(TRACK, {}), expected)
        if differences:
            raise AlphaError("Alpha track readback mismatch: " + ", ".join(differences) + ".")
        if after_testers != testers:
            raise AlphaError("Alpha tester configuration changed; refusing to commit.")
        if {k: v for k, v in before.items() if k != TRACK} != {
            k: v for k, v in after.items() if k != TRACK
        }:
            raise AlphaError("Another Play track changed; refusing to commit.")
        request(session, "POST", f"{API}/{edit}:validate")
        request(session, "POST", f"{API}/{edit}:commit", params={
            "changesNotSentForReview": "false",
            "changesInReviewBehavior": "ERROR_IF_IN_REVIEW",
        })
        committed = True
    finally:
        if not committed:
            discard_edit(session, edit)

    # A committed edit alone is not proof of tester availability.
    verification = request(session, "POST", API, json={})["id"]
    try:
        actual, actual_testers = snapshot(session, verification)
        differences = track_mismatches(actual.get(TRACK, {}), after[TRACK])
        if differences:
            raise AlphaError(
                "Committed Alpha readback mismatch: " + ", ".join(differences)
                + "; inspect Console before retrying."
            )
        if actual_testers != testers or {k: v for k, v in actual.items() if k != TRACK} != {
            k: v for k, v in after.items() if k != TRACK
        }:
            raise AlphaError("Committed Alpha readback did not match; inspect Console before retrying.")
    finally:
        discard_edit(session, verification)
    return digest


def verified_bundle(metadata):
    data = json.loads(metadata.read_text())
    name, code = version_data(data["version"])
    cert = (ROOT / "apps/android/signing/release-certificate.sha256").read_text().strip()
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    asset = f"holon-android-v{name}"
    if (data["package"], data["version_code"], data["certificate_sha256"], data["commit"], data["aab"]) != (
        PACKAGE, code, cert, commit, f"{asset}.aab"
    ):
        raise AlphaError("Candidate provenance does not match the verified build.")
    bundle = metadata.parent / data["aab"]
    checksums = dict(line.split()[::-1] for line in (metadata.parent / f"{asset}.sha256").read_text().splitlines())
    for path in (metadata, bundle):
        if hashlib.sha256(path.read_bytes()).hexdigest() != checksums.get(path.name):
            raise AlphaError("Candidate checksum mismatch.")
    return bundle, name, code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("prepare").add_argument("version")
    upload = commands.add_parser("publish")
    upload.add_argument("metadata", type=Path)
    upload.add_argument("status", choices=("draft", "completed"))
    upload.add_argument("notes")
    args = parser.parse_args()
    if args.command == "prepare":
        name, code = prepare(args.version)
        print(f"version_name={name}\nversion_code={code}")
        return

    from google.oauth2.service_account import Credentials
    from google.auth.transport.requests import AuthorizedSession

    bundle, name, code = verified_bundle(args.metadata)
    credentials = Credentials.from_service_account_info(
        json.loads(os.environ["GOOGLE_PLAY_SERVICE_ACCOUNT_JSON"]),
        scopes=["https://www.googleapis.com/auth/androidpublisher"],
    )
    with AuthorizedSession(credentials) as session:
        digest = publish(session, bundle, name, code, args.status, args.notes)
    report = (
        f"## Google Play Alpha\n- 包名：{PACKAGE}\n- 版本：{name} / {code}\n"
        f"- 状态：{args.status}\n- 来源：{subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()}\n"
        f"- AAB SHA-256: {digest}\n"
        "仅更新 Alpha；Holon Pioneers 与其他轨道保持不变。\n"
        "API 提交和读回成功；Google 审核及测试者实际可用状态仍需确认。\n"
    )
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
            summary.write(report)
    print(report)


if __name__ == "__main__":
    try:
        main()
    except AlphaError as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
    except Exception as error:
        print(f"Alpha publishing failed ({type(error).__name__}); inspect Console before retrying.", file=sys.stderr)
        raise SystemExit(1)
