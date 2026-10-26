"""Real Git fixtures for the acceptance image's source identity contract."""
from pathlib import Path
import json
import subprocess
import sys
import tempfile
import unittest

from scripts.docker_e2e.identified_build import git, prepare
from scripts.docker_e2e.fallback_storm import require_build


class IdentifiedBuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        git(self.source, "init", "--quiet")
        git(self.source, "config", "user.name", "Acceptance Fixture")
        git(self.source, "config", "user.email", "fixture@example.invalid")
        (self.source / "Dockerfile").write_text("FROM scratch\nCOPY . /src\n")
        (self.source / ".dockerignore").write_text(".git\n.github\n")
        (self.source / ".github").mkdir()
        (self.source / ".github" / "tracked").write_text("must remain in the clean context\n")
        git(self.source, "add", ".")
        git(self.source, "commit", "--quiet", "-m", "fixture")

    def check_context(self, source):
        revision = git(source, "rev-parse", "HEAD")
        source_short_revision = git(source, "rev-parse", "--short", "HEAD")
        result = prepare(source, self.root / "identified")
        context, dockerfile = Path(result["context"]), Path(result["dockerfile"])
        self.assertEqual(result["revision"], revision)
        self.assertEqual(git(context, "rev-parse", "HEAD"), revision)
        short_revision = git(context, "rev-parse", "--short", "HEAD")
        self.assertEqual(short_revision, revision[:12])
        require_build(f"holon test ({short_revision})", revision)
        self.assertEqual(git(source, "rev-parse", "--short", "HEAD"), source_short_revision)
        self.assertEqual(git(context, "status", "--porcelain"), "")
        self.assertEqual(git(source, "status", "--porcelain"), "")
        self.assertTrue((context / ".git").is_dir())
        self.assertTrue((context / ".github" / "tracked").is_file())
        self.assertFalse((context / ".git/objects/info/alternates").exists())
        self.assertEqual(dockerfile.read_bytes(), (source / "Dockerfile").read_bytes())
        ignore = dockerfile.with_name("Dockerfile.dockerignore").read_text()
        # Docker falls back to the context's .dockerignore for a zero-byte file.
        self.assertTrue(ignore.strip())
        self.assertTrue(all(line.startswith("#") for line in ignore.splitlines()))
        self.assertNotIn("extraheader", git(context, "config", "--list").lower())

    def test_clean_checkout_uses_real_self_contained_git_metadata(self):
        git(self.source, "config", "core.abbrev", "7")
        git(self.source, "config", "http.https://example.invalid/.extraheader", "private fixture")
        self.check_context(self.source)

    def test_linked_worktree_does_not_copy_a_host_local_git_pointer(self):
        worktree = self.root / "worktree"
        git(self.source, "worktree", "add", "--detach", worktree, "HEAD")
        self.assertTrue((worktree / ".git").is_file())
        self.check_context(worktree)

    def test_dirty_source_and_existing_destination_fail_closed(self):
        for name in ("Dockerfile", "untracked"):
            with self.subTest(name=name):
                path = self.source / name
                original = path.read_text() if path.exists() else None
                path.write_text("dirty\n")
                with self.assertRaisesRegex(ValueError, "clean committed"):
                    prepare(self.source, self.root / "identified")
                if original is None:
                    path.unlink()
                else:
                    path.write_text(original)
        destination = self.root / "identified"
        destination.mkdir()
        with self.assertRaises(FileExistsError):
            prepare(self.source, destination)

    def test_acceptance_feature_only_changes_generated_dockerfile_not_git_context(self):
        original = "FROM scratch\nRUN cargo build --release --locked\n"
        (self.source / "Dockerfile").write_text(original)
        git(self.source, "commit", "-am", "standard cargo build")
        result = prepare(self.source, self.root / "identified", recovery_acceptance=True)
        self.assertTrue(result["recovery_acceptance"])
        self.assertIn("--features recovery-acceptance", Path(result["dockerfile"]).read_text())
        self.assertEqual((Path(result["context"]) / "Dockerfile").read_text(), original)
        self.assertEqual(git(Path(result["context"]), "status", "--porcelain"), "")
        short_revision = git(Path(result["context"]), "rev-parse", "--short", "HEAD")
        self.assertEqual(short_revision, result["revision"][:12])
        require_build(f"holon test ({short_revision}) [recovery-acceptance-test]", result["revision"])
        self.assertEqual((self.source / "Dockerfile").read_text(), original)

    def test_acceptance_build_rejects_missing_or_ambiguous_cargo_command(self):
        for index, count in enumerate((0, 2)):
            (self.source / "Dockerfile").write_text(
                "FROM scratch\n" + "RUN cargo build --release --locked\n" * count)
            git(self.source, "add", "Dockerfile")
            git(self.source, "commit", "--allow-empty", "-m", "build fixture")
            with self.subTest(count=count), self.assertRaisesRegex(ValueError, "exactly one"):
                prepare(self.source, self.root / f"invalid-{index}", recovery_acceptance=True)

    def test_documented_script_entrypoint_prepares_the_build_context(self):
        # The acceptance workflow runs this file as a script; a missing
        # __main__ guard would silently prepare nothing and build nothing.
        destination = self.root / "script-identified"
        run = subprocess.run(
            [sys.executable, "scripts/docker_e2e/identified_build.py",
             "--source", str(self.source), "--destination", str(destination)],
            cwd=Path(__file__).resolve().parents[1],
            capture_output=True, text=True,
        )
        self.assertEqual(run.returncode, 0, run.stderr)
        result = json.loads(run.stdout)
        self.assertEqual(result["revision"], git(self.source, "rev-parse", "HEAD"))
        self.assertTrue(Path(result["dockerfile"]).is_file())
        self.assertTrue((destination / "source.json").is_file())


if __name__ == "__main__":
    unittest.main()
