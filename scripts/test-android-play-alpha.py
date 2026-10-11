#!/usr/bin/env python3
"""Credential-free contract tests run by the Alpha pull-request workflow."""

from copy import deepcopy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("alpha", ROOT / "scripts/android-play-alpha.py")
alpha = importlib.util.module_from_spec(spec)
spec.loader.exec_module(alpha)


class Response:
    def __init__(self, data, status=200):
        self.data = data
        self.status_code = status
        self.ok = status < 400
        self.content = bool(data)

    def json(self):
        return deepcopy(self.data)


class Publisher:
    def __init__(self):
        self.tracks = [
            {"track": "alpha", "releases": [{"versionCodes": ["49002"], "status": "completed"}]},
            {"track": "production", "releases": []},
            {"track": "internal", "releases": []},
        ]
        self.testers = {"googleGroups": [alpha.GROUP]}
        self.calls = []
        self.edits = 0
        self.used = 49002
        self.upload_code = 50001
        self.upload_hash = None
        self.commit_status = 200
        self.mutate = None
        self.bad_readback = False

    def request(self, method, url, **kwargs):
        self.calls.append((method, url, kwargs))
        if method == "POST" and url == alpha.API:
            self.edits += 1
            return Response({"id": str(self.edits)})
        if method == "GET" and url.endswith("/tracks"):
            tracks = deepcopy(self.tracks)
            if self.edits == 2 and self.bad_readback:
                tracks[0]["releases"] = []
            return Response({"tracks": tracks})
        if method == "GET" and "/testers/" in url:
            return Response(self.testers)
        if method == "GET" and url.endswith("/bundles"):
            return Response({"bundles": [{"versionCode": self.used}]})
        if method == "GET" and url.endswith("/apks"):
            return Response({})
        if method == "POST" and "?uploadType=media" in url:
            digest = hashlib.sha256(kwargs["data"].read()).hexdigest()
            return Response({"versionCode": self.upload_code, "sha256": self.upload_hash or digest})
        if method == "PUT":
            if not url.endswith("/tracks/alpha"):
                raise AssertionError("Unexpected write outside Alpha")
            self.tracks[0] = deepcopy(kwargs["json"])
            if self.mutate == "track":
                self.tracks[1]["releases"] = [{"status": "draft"}]
            if self.mutate == "testers":
                self.testers = {"googleGroups": []}
            return Response(self.tracks[0])
        if url.endswith(":commit"):
            return Response({}, self.commit_status)
        if url.endswith(":validate") or method == "DELETE":
            return Response({})
        raise AssertionError(f"Unexpected API request: {method} {url}")


class CandidateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/android-release-version.sh", self.root / "scripts")
        self.cargo = self.root / "Cargo.toml"
        self.source = '[package]\nname = "holon"\nversion = "0.50.0"\n\n[dependencies]\nversion = "1.0.0"\n'
        self.cargo.write_text(self.source)

    def test_only_package_version_is_overlaid(self):
        self.assertEqual(alpha.prepare("v0.50.1", self.root), ("0.50.1", 50001))
        self.assertEqual(self.cargo.read_text(), self.source.replace('"0.50.0"', '"0.50.1"'))

    def test_equal_and_older_versions_do_not_mutate(self):
        for version in ("0.50.0", "0.49.3"):
            with self.assertRaises(alpha.AlphaError):
                alpha.prepare(version, self.root)
            self.assertEqual(self.cargo.read_text(), self.source)

    def test_invalid_versions_do_not_mutate(self):
        for version in ("0.50.1-rc1", "$(echo unsafe)", "0.5000.1"):
            with self.assertRaises(subprocess.CalledProcessError):
                alpha.prepare(version, self.root)
            self.assertEqual(self.cargo.read_text(), self.source)


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.bundle = Path(self.tmp.name) / "candidate.aab"
        self.bundle.write_bytes(b"verified candidate fixture")
        self.publisher = Publisher()

    def upload(self, status="completed", notes="Conversation recovery and report access fixes."):
        return alpha.publish(self.publisher, self.bundle, "0.50.1", 50001, status, notes)

    def commits(self):
        return [call for call in self.publisher.calls if call[1].endswith(":commit")]

    def test_completed_commit_is_alpha_only_and_review_safe(self):
        self.assertEqual(self.upload(), hashlib.sha256(self.bundle.read_bytes()).hexdigest())
        commit = self.commits()
        self.assertEqual(len(commit), 1)
        self.assertEqual(commit[0][2]["params"], {
            "changesNotSentForReview": "false", "changesInReviewBehavior": "ERROR_IF_IN_REVIEW",
        })
        release = self.publisher.tracks[0]["releases"]
        self.assertEqual(len(release), 1)
        self.assertEqual(release[0]["versionCodes"], ["50001"])
        self.assertEqual(self.publisher.testers, {"googleGroups": [alpha.GROUP]})
        self.assertEqual(self.publisher.tracks[1]["releases"], [])
        self.assertEqual(self.publisher.tracks[2]["releases"], [])
        self.assertEqual(self.publisher.edits, 2)
        self.assertEqual(self.publisher.calls[-1][:2], ("DELETE", f"{alpha.API}/2"))

    def test_draft_preserves_serving_release(self):
        self.upload("draft")
        releases = self.publisher.tracks[0]["releases"]
        self.assertEqual(releases[0]["versionCodes"], ["49002"])
        self.assertEqual(releases[1]["status"], "draft")

    def test_invalid_status_or_notes_make_no_api_calls(self):
        for status, notes in (("production", "fix"), ("inProgress", "fix"), ("completed", ""), ("draft", "x" * 501)):
            with self.assertRaises(alpha.AlphaError):
                self.upload(status, notes)
        self.assertEqual(self.publisher.calls, [])

    def test_missing_alpha_or_group_is_rejected_before_upload(self):
        for missing in ("alpha", "group"):
            self.publisher = Publisher()
            if missing == "alpha":
                self.publisher.tracks.pop(0)
            else:
                self.publisher.testers = {"googleGroups": []}
            with self.assertRaises(alpha.AlphaError):
                self.upload()
            self.assertFalse(any("uploadType" in call[1] for call in self.publisher.calls))
            self.assertEqual(self.publisher.calls[-1][0], "DELETE")

    def test_existing_draft_or_rollout_is_not_overwritten(self):
        for status in ("draft", "inProgress", "halted"):
            self.publisher = Publisher()
            self.publisher.tracks[0]["releases"][0]["status"] = status
            with self.assertRaises(alpha.AlphaError):
                self.upload()
            self.assertEqual(self.commits(), [])

    def test_reused_or_older_version_code_is_rejected(self):
        for code in (50001, 51000):
            self.publisher = Publisher()
            self.publisher.used = code
            with self.assertRaises(alpha.AlphaError):
                self.upload()
            self.assertFalse(any("uploadType" in call[1] for call in self.publisher.calls))

    def test_uploaded_version_or_hash_mismatch_is_not_committed(self):
        for mismatch in ("version", "hash"):
            self.publisher = Publisher()
            if mismatch == "version":
                self.publisher.upload_code = 99999
            else:
                self.publisher.upload_hash = "bad"
            with self.assertRaises(alpha.AlphaError):
                self.upload()
            self.assertEqual(self.commits(), [])

    def test_other_tracks_and_testers_cannot_change(self):
        for mutation in ("track", "testers"):
            self.publisher = Publisher()
            self.publisher.mutate = mutation
            with self.assertRaises(alpha.AlphaError):
                self.upload()
            self.assertEqual(self.commits(), [])

    def test_in_review_error_is_not_retried_or_downgraded(self):
        self.publisher.commit_status = 400
        with self.assertRaisesRegex(alpha.AlphaError, "HTTP 400"):
            self.upload()
        self.assertEqual(len(self.commits()), 1)
        self.assertEqual(self.publisher.calls[-1][0], "DELETE")

    def test_readback_mismatch_is_not_success(self):
        self.publisher.bad_readback = True
        with self.assertRaisesRegex(alpha.AlphaError, "readback"):
            self.upload()
        self.assertEqual(len(self.commits()), 1)

    def test_authentication_error_is_sanitized(self):
        class Denied:
            def request(self, *args, **kwargs):
                return Response({"private_key": "never-log-this"}, 403)
        with self.assertRaisesRegex(alpha.AlphaError, "HTTP 403") as caught:
            alpha.publish(Denied(), self.bundle, "0.50.1", 50001, "draft", "Fixes.")
        self.assertNotIn("never-log-this", str(caught.exception))


class WorkflowTests(unittest.TestCase):
    def test_credentials_are_only_in_main_dispatch_job(self):
        text = (ROOT / ".github/workflows/android-play-alpha.yml").read_text()
        contract, upload = text.split("\n  upload:", 1)
        self.assertNotIn("secrets.", contract)
        self.assertIn("github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main'", upload)
        self.assertIn('test "$CONFIRMED" = true', upload)
        self.assertIn("default: draft", contract)
        self.assertIn("group: android-play-upload", upload)
        internal = (ROOT / ".github/workflows/android-play-internal.yml").read_text()
        self.assertIn("group: android-play-upload", internal)
        self.assertNotIn("cancel-in-progress: true", upload)


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)
        self.asset = "holon-android-v0.50.1"
        self.bundle = self.directory / f"{self.asset}.aab"
        self.bundle.write_bytes(b"candidate fixture")
        self.metadata = self.directory / f"{self.asset}.json"
        self.data = {
            "package": alpha.PACKAGE, "version": "0.50.1", "version_code": 50001,
            "certificate_sha256": (ROOT / "apps/android/signing/release-certificate.sha256").read_text().strip(),
            "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
            "aab": self.bundle.name,
        }
        self.write_metadata()

    def write_metadata(self):
        self.metadata.write_text(json.dumps(self.data))
        checksums = "\n".join(
            f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}"
            for path in (self.bundle, self.metadata)
        )
        (self.directory / f"{self.asset}.sha256").write_text(checksums + "\n")

    def test_verified_metadata_matches_candidate(self):
        self.assertEqual(alpha.verified_bundle(self.metadata), (self.bundle, "0.50.1", 50001))

    def test_provenance_fields_cannot_be_replaced(self):
        for key, value in (
            ("package", "other.app"), ("version_code", 50002),
            ("certificate_sha256", "bad"), ("commit", "bad"), ("aab", "../outside.aab"),
        ):
            original = self.data[key]
            self.data[key] = value
            self.write_metadata()
            with self.assertRaisesRegex(alpha.AlphaError, "provenance"):
                alpha.verified_bundle(self.metadata)
            self.data[key] = original

    def test_tampered_metadata_and_bundle_fail_checksums(self):
        self.metadata.write_text(json.dumps(self.data, indent=2))
        with self.assertRaisesRegex(alpha.AlphaError, "checksum"):
            alpha.verified_bundle(self.metadata)
        self.write_metadata()
        self.bundle.write_bytes(b"tampered")
        with self.assertRaisesRegex(alpha.AlphaError, "checksum"):
            alpha.verified_bundle(self.metadata)


if __name__ == "__main__":
    unittest.main()
