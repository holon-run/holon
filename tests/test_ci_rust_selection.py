"""Regression cases for path ownership, Cargo dependency direction, and fallback."""

import io
import json
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from subprocess import run
from unittest.mock import patch

from scripts import ci_rust_selection as selection


class RustSelectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.manifests = selection.crate_manifests()
        cls.graph = selection.reverse_dependencies(
            {"holon": selection.ROOT / "Cargo.toml", **cls.manifests}
        )

    def select(self, *paths):
        return selection.select(paths, self.manifests, lambda: self.graph)

    def test_isolated_unit_and_integration_tests(self):
        for path in (
            "crates/holon-tui/src/tui/composer/tests.rs",
            "crates/holon-provider/src/provider/transports/openai/oauth_timeout_tests.rs",
            "crates/holon-model-catalog/tests/fixtures/models_dev/sample.json",
        ):
            with self.subTest(path=path):
                self.assertEqual(self.select(path), (False, [path.split("/")[1]], True))

    def test_leaf_and_reverse_dependencies(self):
        self.assertEqual(self.select("crates/holon-tui/src/lib.rs"),
                         (True, ["holon-tui"], False))
        self.assertEqual(self.select("crates/holon-github/src/lib.rs"),
                         (True, ["holon-agent-templates", "holon-github"], False))
        self.assertEqual(self.select("src/runtime.rs"), (True, [], False))
        self.assertEqual(self.select("crates/decision-local-onnx/src/lib.rs"),
                         (True, [], False))

    def test_dev_only_consumer_is_a_terminal_edge(self):
        graph = (
            {"holon-client": {"holon-tui"}, "holon-tui": {"holon"}},
            {"holon-provider": {"holon-client"}},
        )
        plan = selection.select(
            ["crates/holon-provider/src/lib.rs"], self.manifests, lambda: graph
        )
        self.assertEqual(plan, (False, ["holon-client", "holon-provider"], False))

    def test_mixed_changes_and_exported_test_support(self):
        self.assertEqual(
            self.select(
                "crates/holon-tui/src/tui/composer/tests.rs",
                "crates/holon-github/src/lib.rs",
            ),
            (True, ["holon-agent-templates", "holon-github", "holon-tui"], False),
        )
        provider = "crates/holon-provider/src/provider/"
        self.assertEqual(self.select(provider + "test_support.rs"),
                         self.select(provider + "mod.rs"))

    def test_unclassified_paths_and_package_metadata_run_the_full_suite(self):
        for paths in (
            (),
            ("Cargo.lock",),
            ("crates/holon-tui/Cargo.toml",),
            ("tests/fixtures/client-wire/status.json",),
            ("crates/deleted-crate/src/lib.rs",),
            (".github/workflows/ci.yml", "crates/holon-tui/src/tui/composer/tests.rs"),
        ):
            with self.subTest(paths=paths):
                self.assertEqual(self.select(*paths), (True, None, False))

    def test_git_move_includes_both_owners(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(selection, "ROOT", Path(directory)):
            def git(*args):
                return run(["git", *args], cwd=directory, check=True, capture_output=True, text=True)

            git("init", "-q")
            git("config", "commit.gpgsign", "false")
            file = Path(directory, "crates/holon-provider/src/lib.rs")
            file.parent.mkdir(parents=True)
            file.write_text("pub const VALUE: u64 = 1;")
            git("add", ".")
            git("-c", "user.name=CI", "-c", "user.email=ci@example.test", "commit", "-qm", "base")
            dest = Path(directory, "crates/holon-tui/tests/moved.rs")
            dest.parent.mkdir(parents=True)
            file.rename(dest)
            git("add", "--all")
            git("-c", "user.name=CI", "-c", "user.email=ci@example.test", "commit", "-qm", "move")
            self.assertEqual(
                set(selection.changed_paths("HEAD^1")),
                {"crates/holon-provider/src/lib.rs", "crates/holon-tui/tests/moved.rs"},
            )

    def test_missing_diff_and_graph_failure_fall_back(self):
        for patched, opts in (
            ("changed_paths", {"side_effect": OSError}),
            ("reverse_dependencies", {"side_effect": KeyError}),
        ):
            with self.subTest(patched=patched), tempfile.TemporaryDirectory() as directory:
                output = Path(directory, "outputs")
                with patch.dict("os.environ", GITHUB_OUTPUT=str(output)):
                    with patch.object(
                        selection, "changed_paths", return_value=["crates/holon-tui/src/lib.rs"]
                    ), patch.object(selection, patched, **opts), redirect_stdout(io.StringIO()):
                        selection.plan("HEAD")
                plan = {key: json.loads(value) for key, value in (
                    line.split("=", 1) for line in output.read_text().splitlines()
                )}
                self.assertEqual(plan, {"root": True, "crates": None, "crate_tests_only": False})


if __name__ == "__main__":
    unittest.main()
