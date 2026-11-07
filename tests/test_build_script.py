"""Exercise the real build script through Cargo, including linked worktrees."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


BUILD_SCRIPT = Path(__file__).resolve().parent.parent / "build.rs"


class BuildScriptTests(unittest.TestCase):
    def test_full_build_stages_large_targets_and_keeps_all_targets(self):
        makefile = (BUILD_SCRIPT.parent / "Makefile").read_text()
        recipe = makefile.split("\nbuild:", 1)[1].split("\n\n", 1)[0]
        self.assertEqual(
            [line.strip() for line in recipe.splitlines() if line.startswith("\t")],
            ["cargo build --lib", "cargo test --lib --no-run", "cargo build --all-targets"],
        )

    def command(self, directory, *args, env=None):
        return subprocess.run(
            args, cwd=directory, env=env, check=True, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        ).stdout

    def fixture(self, directory):
        (directory / "src").mkdir()
        (directory / "Cargo.toml").write_text(
            '[package]\nname = "holon-build-script-fixture"\n'
            'version = "0.1.0"\nedition = "2021"\n'
        )
        (directory / "src/lib.rs").write_text("pub fn fixture() {}\n")
        shutil.copyfile(BUILD_SCRIPT, directory / "build.rs")

    def git(self, directory, *args):
        return self.command(
            directory, "git", "-c", "user.name=Fixture",
            "-c", "user.email=fixture@example.invalid", *args,
        )

    def assert_cached(self, directory, has_git):
        env = dict(os.environ)
        env["CARGO_TARGET_DIR"] = str(directory / "target")
        env.pop("HOLON_LIB_TEST_SHARD", None)
        self.command(directory, "cargo", "build", "--offline", "--lib", env=env)
        output = next((directory / "target/debug/build").glob(
            "holon-build-script-fixture-*/output"
        ))
        lines = output.read_text().splitlines()
        watched = [
            line.split("=", 1)[1] for line in lines
            if line.startswith("cargo:rerun-if-changed=")
        ]
        self.assertIn("build.rs", watched)
        self.assertTrue(all((directory / path).exists() for path in watched))
        if has_git:
            head_path = self.git(directory, "rev-parse", "--git-path", "HEAD").strip()
            expected_watched = ["build.rs", head_path]
            reference = self.git(directory, "rev-parse", "--symbolic-full-name", "HEAD").strip()
            if reference != "HEAD":
                path = self.git(directory, "rev-parse", "--git-path", reference).strip()
                if not (directory / path).exists():
                    packed = self.git(directory, "rev-parse", "--git-path", "packed-refs").strip()
                    expected_watched.append(packed)
                    parent = Path(path).parent
                    while not (directory / parent).is_dir():
                        parent = parent.parent
                    expected_watched.append(str(parent))
                else:
                    expected_watched.append(path)
            self.assertEqual(watched, expected_watched)
            self.assertIn(
                f"cargo:rustc-env=HOLON_VERSION=0.1.0 "
                f"({self.git(directory, 'rev-parse', '--short', 'HEAD').strip()})",
                lines,
            )
        else:
            self.assertEqual(watched, ["build.rs"])
        timestamp = output.stat().st_mtime_ns
        repeated = self.command(
            directory, "cargo", "build", "--offline", "--lib", "-vv", env=env,
        )
        self.assertEqual(output.stat().st_mtime_ns, timestamp, repeated)
        self.command(
            directory, "cargo", "test", "--offline", "--lib", "--no-run", env=env,
        )
        repeated = self.command(
            directory, "cargo", "test", "--offline", "--lib", "--", "--list", env=env,
        )
        self.assertNotIn("Compiling holon-build-script-fixture", repeated)
        if has_git:
            self.git(
                directory, "update-ref", "refs/remotes/origin/unrelated",
                self.git(directory, "rev-parse", "HEAD").strip(),
            )
            repeated = self.command(
                directory, "cargo", "test", "--offline", "--lib",
                "--", "--list", env=env,
            )
            self.assertEqual(output.stat().st_mtime_ns, timestamp, repeated)
            self.assertNotIn("Compiling holon-build-script-fixture", repeated)
            self.git(directory, "commit", "--allow-empty", "-m", "advance HEAD")
            self.command(directory, "cargo", "build", "--offline", "--lib", env=env)
            self.assertIn(
                f"cargo:rustc-env=HOLON_VERSION=0.1.0 "
                f"({self.git(directory, 'rev-parse', '--short', 'HEAD').strip()})",
                output.read_text().splitlines(),
            )

    def test_canonical_and_linked_worktree_cache_and_head_changes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repository = root / "repository"
            repository.mkdir()
            self.fixture(repository)
            (repository / ".gitignore").write_text("/target/\n/Cargo.lock\n")
            self.git(repository, "init")
            self.git(repository, "add", ".")
            self.git(repository, "commit", "-m", "fixture")
            self.git(repository, "pack-refs", "--all")
            self.assert_cached(repository, has_git=True)
            worktree = root / "linked"
            self.git(repository, "worktree", "add", "-b", "linked-fixture", str(worktree))
            self.assert_cached(worktree, has_git=True)
            self.git(worktree, "checkout", "--detach")
            self.assert_cached(worktree, has_git=True)

    def test_source_archive_without_git_is_cached(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            self.fixture(directory)
            self.assert_cached(directory, has_git=False)
