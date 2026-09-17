import re
import unittest
from pathlib import Path
from unittest import mock

from scripts import ci_test_shards


MAKEFILE = Path(__file__).resolve().parent.parent / "Makefile"

FAKE_TARGETS = [
    "cli_exit_codes",
    "cli_json_contract",
    "http_callback",
    "http_control",
    "http_operator_transport",
    "http_workspace",
    "live_openai",
    "misc_new_binary",
    "models_dev_adapter",
    "run_once",
    "runtime_tasks",
    "wt204_parallel_worktree_workflow",
]


def makefile_variable(text: str, name: str) -> list[str]:
    """Return the tokens of a Makefile := definition, joining continuations."""
    tokens: list[str] = []
    collecting = False
    for line in text.splitlines():
        if collecting:
            stripped = line.rstrip()
            if stripped.endswith("\\"):
                tokens.extend(stripped[:-1].split())
                continue
            tokens.extend(stripped.split())
            return tokens
        match = re.match(rf"^{name} := (.*)$", line)
        if not match:
            continue
        rest = match.group(1).rstrip()
        if rest.endswith("\\"):
            tokens.extend(rest[:-1].split())
            collecting = True
        else:
            tokens.extend(rest.split())
            return tokens
    return tokens


class CiTestShardsTests(unittest.TestCase):
    def test_makefile_concurrent_tests_match_script(self):
        text = MAKEFILE.read_text()
        concurrent = makefile_variable(text, "CONCURRENT_TESTS")
        lifecycle = makefile_variable(text, "CONCURRENT_LIFECYCLE_TESTS")
        self.assertTrue(concurrent, "CONCURRENT_TESTS missing from Makefile")
        self.assertTrue(
            lifecycle, "CONCURRENT_LIFECYCLE_TESTS missing from Makefile"
        )
        makefile_names = [
            name
            for name in concurrent
            if name != "$(CONCURRENT_LIFECYCLE_TESTS)"
        ] + lifecycle
        self.assertEqual(
            set(makefile_names), set(ci_test_shards.CONCURRENT_TEST_TARGETS),
            "Makefile CONCURRENT_TESTS and scripts/ci_test_shards.py "
            "CONCURRENT_TEST_TARGETS must stay in sync",
        )

    def test_unassigned_targets_fall_through_to_misc(self):
        misc = ci_test_shards.suite_members("misc", FAKE_TARGETS)
        self.assertIn("live_openai", misc)
        self.assertIn("misc_new_binary", misc)
        for name in ci_test_shards.EXPLICIT_TARGETS:
            self.assertNotIn(name, misc)

    def test_serial_suite_excludes_only_concurrent_binaries(self):
        serial = ci_test_shards.suite_members("serial", FAKE_TARGETS)
        self.assertEqual(
            set(serial),
            set(FAKE_TARGETS) - ci_test_shards.CONCURRENT_TEST_TARGETS,
        )

    def test_validation_rejects_unknown_explicit_members(self):
        problems = ci_test_shards.validation_problems(["http_control"])
        self.assertTrue(problems)
        self.assertIn(
            "non-existent test targets", " ".join(problems)
        )

    def test_validation_accepts_disjoint_real_shards(self):
        targets = sorted(
            ci_test_shards.EXPLICIT_TARGETS
            | {"live_openai", "misc_new_binary"}
        )
        self.assertEqual(ci_test_shards.validation_problems(targets), [])

    def test_lib_suite_uses_two_threads_and_integration_shards_stay_serial(self):
        with mock.patch.object(
            ci_test_shards, "cargo_test_targets", return_value=FAKE_TARGETS
        ):
            lib = ci_test_shards.suite_command("lib", FAKE_TARGETS, None)
            control = ci_test_shards.suite_command("control", FAKE_TARGETS, None)
            serial = ci_test_shards.suite_command("serial", FAKE_TARGETS, None)
        self.assertEqual(
            lib, ["cargo", "test", "--lib", "--bins", "--", "--test-threads=2"]
        )
        self.assertIn("--test", control)
        self.assertIn("http_control", control)
        self.assertTrue(
            control[-2:] == ["--", "--test-threads=1"],
            "integration shards stay serial inside each binary",
        )
        self.assertTrue(serial[-2:] == ["--", "--test-threads=1"])
        self.assertNotIn("--test", lib)


if __name__ == "__main__":
    unittest.main()
