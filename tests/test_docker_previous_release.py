"""Fail-closed previous-release fixture preparation without network or Docker."""
import hashlib
import io
from pathlib import Path
import subprocess
import tarfile
import unittest
from unittest.mock import patch

from scripts.docker_e2e import previous_release as fixture


ROOT = Path(__file__).resolve().parents[1]


def archive_bytes(names=("holon",), *, symlink=False):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        for name in names:
            member = tarfile.TarInfo(name)
            if symlink:
                member.type = tarfile.SYMTYPE
                member.linkname = "/outside"
                archive.addfile(member)
            else:
                data = b"previous release binary"
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
    return output.getvalue()


class PreviousReleaseTests(unittest.TestCase):
    def setUp(self):
        self.archive = archive_bytes(("holon", "../not-extracted"))
        self.checksum = hashlib.sha256(self.archive).hexdigest()
        self.checksums = f"{self.checksum}  ./{fixture.ARCHIVE}\n"
        self.calls = []

    def command(self, args, **kwargs):
        self.calls.append(args)
        if args[0] == "curl":
            name = args[-1].rsplit("/", 1)[-1]
            Path(args[3]).write_bytes(
                self.checksums.encode() if name == "checksums.txt" else self.archive
            )
        elif args[:2] == ["docker", "build"]:
            work = Path(args[-1])
            self.assertEqual((work / "holon").read_bytes(), b"previous release binary")
            self.assertEqual((work / "holon").stat().st_mode & 0o777, 0o755)
            self.assertEqual((work / "Dockerfile").read_text(), fixture.DOCKERFILE)
            self.assertEqual(
                {p.name for p in work.iterdir()},
                {"holon", "Dockerfile", fixture.ARCHIVE, "checksums.txt"},
            )
        return subprocess.CompletedProcess(args, 0, stdout="sha256:" + "b" * 64)

    def prepare(self, **kwargs):
        with patch.object(fixture.subprocess, "run", side_effect=self.command):
            return fixture.prepare("v0.30.0", **kwargs)

    def assert_no_build(self):
        self.assertFalse(any(call[0] == "docker" for call in self.calls))

    def test_verified_archive_builds_exact_previous_binary(self):
        result = self.prepare(expected_sha256=self.checksum)
        self.assertEqual(result["ref"], "v0.30.0")
        self.assertEqual(result["image"], "holon-e2e-previous:0.30.0")
        self.assertEqual(result["archive_sha256"], self.checksum)
        self.assertEqual(result["image_id"], "sha256:" + "b" * 64)
        self.assertTrue(all("/v0.30.0/" in call[-1] for call in self.calls[:2]))
        self.assertEqual(len(self.calls), 4)

    def test_invalid_ref_repository_and_pin_do_not_download(self):
        with patch.object(fixture.subprocess, "run") as run:
            for ref in ("main", "0.30.0", "v0.30.0/other"):
                with self.subTest(ref=ref), self.assertRaises(ValueError):
                    fixture.prepare(ref)
            with self.assertRaises(ValueError):
                fixture.prepare("v0.30.0", repository="../other/repo")
            with self.assertRaises(ValueError):
                fixture.prepare("v0.30.0", expected_sha256="bad")
            run.assert_not_called()

    def test_missing_duplicate_and_invalid_checksum_fail_closed(self):
        for checksums in ("", self.checksums * 2, f"invalid  {fixture.ARCHIVE}\n"):
            with self.subTest(checksums=checksums):
                self.checksums = checksums
                with self.assertRaisesRegex(ValueError, "exactly one SHA-256"):
                    self.prepare()
                self.assert_no_build()

    def test_mismatched_archive_is_rejected_before_extraction(self):
        self.archive += b"tampered"
        with patch.object(fixture.tarfile, "open") as extract:
            with self.assertRaisesRegex(ValueError, "archive SHA-256 mismatch"):
                self.prepare()
            extract.assert_not_called()
        self.assert_no_build()

    def test_pinned_checksum_cannot_follow_changed_release_assets(self):
        with self.assertRaisesRegex(ValueError, "pinned archive"):
            self.prepare(expected_sha256="0" * 64)
        self.assert_no_build()

    def test_missing_duplicate_and_link_binary_are_rejected(self):
        for names, link in ((("other",), False), (("holon", "holon"), False),
                            (("holon",), True)):
            with self.subTest(names=names, link=link):
                self.archive = archive_bytes(names, symlink=link)
                self.checksums = f"{hashlib.sha256(self.archive).hexdigest()}  {fixture.ARCHIVE}\n"
                with self.assertRaisesRegex(ValueError, "one regular holon binary"):
                    self.prepare()
                self.assert_no_build()

    def test_download_failure_does_not_build(self):
        with patch.object(fixture.subprocess, "run",
                          side_effect=subprocess.CalledProcessError(22, ["curl"])):
            with self.assertRaises(subprocess.CalledProcessError):
                fixture.prepare("v0.30.0")

    def test_ci_and_release_share_verified_asset_builder(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        release = (ROOT / ".github/workflows/release-e2e.yml").read_text()
        for workflow in (ci, release):
            self.assertIn("scripts/prepare-previous-release-image.py", workflow)
            self.assertNotIn("ghcr.io/holon-run/holon:v0.30.0", workflow)
        self.assertIn("--ref v0.30.0", ci)
        self.assertIn("--expected-sha256 20d5b7c544c9d55977188e69e03fc90825748c5d26dd47e9d5b32c9146c4e32c", ci)
        self.assertIn("--previous-image holon-e2e-previous:0.30.0", ci)
