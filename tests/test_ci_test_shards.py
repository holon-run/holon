import re
import unittest
from pathlib import Path
from unittest import mock

from scripts import ci_test_shards


MAKEFILE = Path(__file__).resolve().parent.parent / "Makefile"

FAKE_TARGETS = [
    "cli_exit_codes",
    "cli_json_contract",
    "daemon_serve_profile",
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


    def test_lib_test_names_are_read_from_the_libtest_list(self):
        completed = mock.Mock()
        completed.stdout = (
            "running 0 tests\n"
            "alpha::tests::one: test\n"
            "alpha::tests::two: test\n"
            "beta::solo: test\n"
            "gamma::bench: benchmark\n"
            "4 tests, 1 benchmark\n"
        )
        with mock.patch.object(
            ci_test_shards.subprocess, "run", return_value=completed
        ) as run:
            names = ci_test_shards.lib_test_names()
        self.assertEqual(
            names,
            ["alpha::tests::one", "alpha::tests::two", "beta::solo"],
        )
        self.assertEqual(
            run.call_args.args[0],
            ["cargo", "test", "--lib", "--", "--list"],
        )

    def test_compile_groups_cover_each_group_once(self):
        groups = [g for job in ci_test_shards.LIB_SHARDS
                  for g in ci_test_shards.LIB_COMPILE_GROUPS[job]]
        self.assertEqual(sorted(groups), ["a", "b", "c", "d"])

    def test_ownership_keeps_subsystems_and_new_tests_together(self):
        for owner, expected in (("runtime", "a"), ("runtime_db", "b"),
                                ("model_catalog", "c"), ("host", "d"),
                                ("new_subsystem", "d")):
            for name in (f"{owner}::tests::one", f"{owner}::nested::new_test"):
                self.assertEqual(ci_test_shards.lib_compile_group(name), expected)

    def test_missing_cfg_duplicate_and_empty_groups_are_rejected(self):
        for names, group in (([], "a"), (["runtime::test"] * 2, "a"),
                             (["host::ungated_test"], "a")):
            with self.subTest(names=names), self.assertRaises(ValueError):
                ci_test_shards.validate_lib_group(names, group)
        ci_test_shards.validate_lib_group(["runtime::tests::new_test"], "a")

    def test_group_env_overrides_stale_selector_without_changing_rustflags(self):
        with mock.patch.dict(ci_test_shards.os.environ,
                             {"HOLON_LIB_TEST_SHARD": "b", "RUSTFLAGS": "-D warnings"}):
            env = ci_test_shards.lib_group_environment("a")
            self.assertEqual(env["HOLON_LIB_TEST_SHARD"], "a")
            self.assertEqual(env["RUSTFLAGS"], "-D warnings")
            self.assertEqual(ci_test_shards.os.environ["HOLON_LIB_TEST_SHARD"], "b")
        with self.assertRaises(ValueError):
            ci_test_shards.lib_group_environment("typo")

    def test_group_enumeration_selects_cfg_before_compilation(self):
        with mock.patch.object(ci_test_shards.subprocess, "run") as run:
            run.return_value.stdout = "runtime::tests::one: test\n"
            self.assertEqual(ci_test_shards.lib_test_names("a"), ["runtime::tests::one"])
        self.assertEqual(run.call_args.kwargs["env"]["HOLON_LIB_TEST_SHARD"], "a")
        self.assertNotIn("capture_output", run.call_args.kwargs)

    def test_ci_jobs_never_request_full_libtest(self):
        for job in ci_test_shards.LIB_SHARDS:
            with self.assertRaises(ValueError):
                ci_test_shards.suite_command(job, FAKE_TARGETS, None)

    def test_runner_executes_compile_groups_serially_and_bins_once(self):
        import json
        import tempfile
        for job, names, expected_bins in (
            ("lib-a", [["runtime::one"], ["model_catalog::one"]], 1),
            ("lib-b", [["runtime_db::one"], ["host::one"]], 0),
        ):
            with self.subTest(job=job), tempfile.TemporaryDirectory() as target:
                with mock.patch.dict(ci_test_shards.os.environ, {"CARGO_TARGET_DIR": target}), \
                     mock.patch.object(ci_test_shards, "lib_test_names", side_effect=names) as listing, \
                     mock.patch.object(ci_test_shards.subprocess, "run") as run:
                    run.return_value.returncode = 0
                    ci_test_shards.run_lib_shard(job)
                groups = ci_test_shards.LIB_COMPILE_GROUPS[job]
                self.assertEqual(listing.call_args_list, [mock.call(g) for g in groups])
                self.assertEqual(run.call_count, 2 + expected_bins)
                for call, group in zip(run.call_args_list, groups):
                    self.assertEqual(call.kwargs["env"]["HOLON_LIB_TEST_SHARD"], group)
                    self.assertEqual(call.args[0],
                                     ["cargo", "test", "--lib", "--", "--test-threads=2"])
                    self.assertNotIn("check", call.kwargs)
                for group, expected in zip(groups, names):
                    path = Path(target) / "ci-timing" / f"lib-test-names-{group}.json"
                    self.assertEqual(json.loads(path.read_text()), expected)
                if expected_bins:
                    self.assertIn("--bins", run.call_args_list[-1].args[0])

    def test_missing_source_cfg_fails_before_running_tests(self):
        with mock.patch.object(ci_test_shards, "lib_test_names", return_value=["host::one"]), \
             mock.patch.object(ci_test_shards.subprocess, "run") as run:
            with self.assertRaises(ValueError):
                ci_test_shards.run_lib_shard("lib-a")
            run.assert_not_called()

    def test_coverage_compares_the_name_multiset_not_only_counts(self):
        names = ["runtime::one", "runtime_db::one", "model_catalog::one", "host::one"]
        groups = [[name] for name in names]
        ci_test_shards.verify_lib_lists(names, groups)
        for broken in (groups[:-1], groups[:-1] + [["host::other"]],
                       groups[:-1] + [["runtime::one"]], groups[:-1] + [[]]):
            with self.subTest(groups=broken), self.assertRaises(ValueError):
                ci_test_shards.verify_lib_lists(names, broken)

    def test_test_failures_do_not_skip_the_remaining_groups(self):
        import tempfile
        with tempfile.TemporaryDirectory() as target:
            with mock.patch.dict(ci_test_shards.os.environ, {"CARGO_TARGET_DIR": target}), \
                 mock.patch.object(ci_test_shards, "lib_test_names",
                                   side_effect=[["runtime::one"], ["model_catalog::one"]]), \
                 mock.patch.object(ci_test_shards.subprocess, "run") as run:
                run.return_value.returncode = 1
                with self.assertRaises(ci_test_shards.subprocess.CalledProcessError):
                    ci_test_shards.run_lib_shard("lib-a")
                self.assertEqual(run.call_count, 3)


if __name__ == "__main__":
    unittest.main()
