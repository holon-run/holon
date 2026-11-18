import io
import argparse
import json
import re
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock

from scripts import ci_test_shards
from scripts import ci_external_tests


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


class ExternalTestRoutingTests(unittest.TestCase):
    def setUp(self):
        patcher = mock.patch.object(ci_external_tests.platform, "platform", return_value="test-os")
        patcher.start()
        self.addCleanup(patcher.stop)

    def entry(self, name="external", target="mixed"):
        return {
            "manifest_path": "Cargo.toml", "target": target, "name": name,
            "dependencies": ["git"], "platforms": ["all"],
            "source": "tests/cli_json_contract.rs",
        }

    def test_reviewed_manifest_has_exact_source_names_and_platforms(self):
        entries = ci_external_tests.load_manifest()
        self.assertTrue(entries)
        self.assertTrue(any("git" in entry["dependencies"] for entry in entries))

    def test_reviewed_mixed_fixtures_keep_pure_cases_and_existing_opt_in_gates(self):
        payload = json.loads(ci_external_tests.MANIFEST.read_text())
        keys = {
            (entry["manifest_path"], entry["target"], entry["name"])
            for entry in ci_external_tests.load_manifest()
        }
        for entry in payload["existing_opt_in_tests"]:
            self.assertNotIn((entry["manifest_path"], entry["target"], entry["name"]), keys)
        for target, name in (
            ("prompt_context_snapshots", "multi_turn_context_eval_preserves_long_task_continuity_and_efficiency"),
            ("prompt_context_snapshots", "multi_turn_context_eval_preserves_initial_issue_list_during_item_by_item_discussion"),
            ("runtime_tasks", "exec_command_rejects_ambiguous_input_envelopes_without_execution"),
        ):
            self.assertNotIn(("Cargo.toml", target, name), keys)
        self.assertIn((
            "Cargo.toml", "cli_json_contract",
            "tailscale_serve_reenable_uses_bound_loopback_for_each_primary",
        ), keys)

    def test_preflight_checks_indirect_shell_dependencies_not_stubbed_cli(self):
        entry = {**self.entry(), "dependencies": ["posix-shell", "cli:sleep", "fixture:gh"]}
        with mock.patch.object(ci_external_tests.shutil, "which",
                               side_effect=lambda name: None if name == "sleep" else f"/bin/{name}") as which, \
             self.assertRaisesRegex(ValueError, "missing external test dependency: sleep"):
            ci_external_tests.preflight([entry])
        self.assertEqual([call.args[0] for call in which.call_args_list], ["sh", "sleep"])

    def test_manifest_rejects_duplicates_unknown_platforms_and_stale_sources(self):
        entry = ci_external_tests.load_manifest()[0]
        invalid = [
            {"version": 1, "tests": [entry, entry]},
            {"version": 1, "tests": [{**entry, "platforms": ["unverified-os"]}]},
            {"version": 1, "tests": [{**entry, "name": "missing_test_function"}]},
            {"version": 1, "tests": [{**entry, "source": "../outside.rs"}]},
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            for payload in invalid:
                path.write_text(json.dumps(payload))
                with self.subTest(payload=payload), self.assertRaises(ValueError):
                    ci_external_tests.load_manifest(path)

    def test_partition_covers_new_tests_without_changing_the_external_set(self):
        regular, external = ci_external_tests.verify_partition(
            ["mock", "external", "new_test"], ["external"],
        )
        self.assertEqual(regular, ["mock", "new_test"])
        self.assertEqual(external, ["external"])

    def test_partition_rejects_stale_names_duplicates_and_substring_skips(self):
        for baseline, external in [
            (["mock"], ["external"]),
            (["external", "external"], ["external"]),
            (["external"], ["external", "external"]),
            (["external", "external_more"], ["external"]),
        ]:
            with self.subTest(baseline=baseline, external=external), self.assertRaises(ValueError):
                ci_external_tests.verify_partition(baseline, external)

    def test_regular_skips_are_scoped_to_the_mixed_target(self):
        entries = [self.entry("external", "mixed"), self.entry("unrelated", "other")]
        command = ["cargo", "test", "--test", "mixed", "--", "--list"]
        with mock.patch.object(ci_external_tests, "load_manifest", return_value=entries):
            actual = ci_external_tests.regular_command(command, targets=["mixed"])
            self.assertEqual(actual, command + ["--skip", "external"])
            self.assertEqual(ci_external_tests.regular_command(command, targets=["pure"]), command)
        self.assertEqual(command[-1], "--list", "routing must not mutate the caller command")

    def test_libtest_enumeration_does_not_include_benchmarks(self):
        self.assertEqual(ci_external_tests.parse_names(
            "pure: test\nexternal: test\nspeed: benchmark\n2 tests, 1 benchmark\n"
        ), ["pure", "external"])

    def test_regular_entry_excludes_external_names_before_every_enumeration(self):
        results = [
            subprocess.CompletedProcess([], 0, "pure: test\nnew_test: test\n"),
        ]
        with mock.patch.object(ci_external_tests, "load_manifest", return_value=[self.entry()]), \
             mock.patch.object(ci_external_tests.subprocess, "run", side_effect=results) as run:
            names = ci_external_tests.verify_regular_target("Cargo.toml", "mixed")
        self.assertEqual(names, ["new_test", "pure"])
        self.assertEqual(run.call_count, 1)
        command = run.call_args.args[0]
        self.assertIn("--list", command)
        self.assertEqual(command[-2:], ["--skip", "external"])

    def test_regular_entry_rejects_duplicate_names_and_external_leaks(self):
        for output in ("pure: test\npure: test\n", "external: test\n"):
            with self.subTest(output=output), \
                 mock.patch.object(ci_external_tests, "load_manifest", return_value=[self.entry()]), \
                 mock.patch.object(ci_external_tests.subprocess, "run",
                                   return_value=subprocess.CompletedProcess([], 0, output)), \
                 self.assertRaises(ValueError):
                ci_external_tests.verify_regular_target("Cargo.toml", "mixed")

    def test_platform_limit_is_not_reported_as_cross_platform_coverage(self):
        self.assertTrue(ci_external_tests.applicable({"platforms": ["unix"]}, "darwin"))
        self.assertFalse(ci_external_tests.applicable({"platforms": ["linux"]}, "darwin"))
        self.assertFalse(ci_external_tests.applicable({"platforms": ["unix"]}, "win32"))

    def test_missing_dependency_fails_explicitly_instead_of_skipping(self):
        with mock.patch.object(ci_external_tests.shutil, "which", return_value=None), \
             self.assertRaisesRegex(ValueError, "missing external test dependency: git"):
            ci_external_tests.preflight([self.entry()])

    def test_missing_app_sdk_fixture_has_its_real_build_command(self):
        entry = {**self.entry(), "dependencies": ["app-sdk"]}
        with tempfile.TemporaryDirectory() as directory, \
             mock.patch.object(ci_external_tests, "ROOT", Path(directory)):
            with self.assertRaisesRegex(ValueError, "run make app-sdk"):
                ci_external_tests.preflight([entry])
            artifact = Path(directory) / "packages/app-sdk/dist/holon.js"
            artifact.parent.mkdir(parents=True)
            artifact.write_text("// fixture")
            ci_external_tests.preflight([entry])

    def test_suites_keep_pure_and_mock_tests_in_mixed_binaries(self):
        entries = [self.entry("external", "cli_json_contract")]
        with mock.patch.object(ci_external_tests, "load_manifest", return_value=entries):
            commands = ci_test_shards.suite_commands("cli", FAKE_TARGETS, "1")
        affected = [command for command in commands if "--skip" in command]
        self.assertEqual(len(affected), 1)
        self.assertIn("cli_json_contract", affected[0])
        self.assertNotIn("http_control", affected[0])
        self.assertIn("--skip", affected[0])
        self.assertNotIn("--ignored", affected[0])

    def test_external_entry_executes_exact_names_and_records_real_partition(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "report.json"
            args = argparse.Namespace(
                manifest_path=None, target=None, lib_group=None, name=None, report=report,
            )
            results = [
                subprocess.CompletedProcess([], 0, "pure_mock: test\nexternal: test\n"),
                subprocess.CompletedProcess([], 0, "pure_mock: test\n"),
                subprocess.CompletedProcess([], 0, "test result: ok. 1 passed; 0 failed; 0 ignored;\n"),
            ]
            with mock.patch.object(ci_external_tests, "load_manifest", return_value=[self.entry()]), \
                 mock.patch.object(ci_external_tests, "preflight"), \
                 mock.patch.object(ci_external_tests.subprocess, "run", side_effect=results) as run:
                ci_external_tests.run_external(args)
            command = run.call_args_list[-1].args[0]
            self.assertEqual(command[-4:], ["--", "--exact", "--test-threads=1", "external"])
            self.assertNotIn("--ignored", command)
            self.assertNotIn("--include-ignored", command)
            self.assertNotIn("pure_mock", command)
            record = json.loads(report.read_text())[0]
            self.assertEqual((record["baseline_count"], record["regular_count"],
                              record["external_count"], record["exit_status"]), (2, 1, 1, 0))

    def test_external_entry_rejects_empty_selection_zero_execution_and_all_ignored(self):
        args = argparse.Namespace(
            manifest_path=None, target=None, lib_group=None, name=None, report=None,
        )
        with mock.patch.object(ci_external_tests, "load_manifest", return_value=[]), \
             self.assertRaisesRegex(ValueError, "selection is empty"):
            ci_external_tests.run_external(args)
        for summary in ["test result: ok. 0 passed; 0 failed; 0 ignored;\n",
                        "test result: ok. 0 passed; 0 failed; 1 ignored;\n"]:
            results = [
                subprocess.CompletedProcess([], 0, "external: test\npure: test\n"),
                subprocess.CompletedProcess([], 0, "pure: test\n"),
                subprocess.CompletedProcess([], 0, summary),
            ]
            with mock.patch.object(ci_external_tests, "load_manifest", return_value=[self.entry()]), \
                 mock.patch.object(ci_external_tests, "preflight"), \
                 mock.patch.object(ci_external_tests.subprocess, "run", side_effect=results), \
                 self.assertRaisesRegex(ValueError, "did not run every"):
                ci_external_tests.run_external(args)

    def test_external_failure_keeps_other_groups_and_records_exit_statuses(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "report.json"
            args = argparse.Namespace(
                manifest_path=None, target=None, lib_group=None, name=None, report=report,
            )
            results = []
            for rc in [7, 0]:
                results += [
                    subprocess.CompletedProcess([], 0, "pure: test\nexternal: test\n"),
                    subprocess.CompletedProcess([], 0, "pure: test\n"),
                    subprocess.CompletedProcess(
                        [], rc, "test result: ok. 1 passed; 0 failed; 0 ignored;\n",
                    ),
                ]
            entries = [self.entry(target="a"), self.entry(target="b")]
            with mock.patch.object(ci_external_tests, "load_manifest", return_value=entries), \
                 mock.patch.object(ci_external_tests, "preflight"), \
                 mock.patch.object(ci_external_tests.subprocess, "run", side_effect=results) as run, \
                 self.assertRaises(SystemExit) as raised:
                ci_external_tests.run_external(args)
            self.assertEqual(raised.exception.code, 7)
            self.assertEqual(run.call_count, 6)
            self.assertEqual([item["exit_status"] for item in json.loads(report.read_text())], [7, 0])


class CiTestShardsTests(unittest.TestCase):
    def setUp(self):
        # These tests use synthetic libtest lists, independent of the audited
        # repository manifest. ExternalTestRoutingTests exercises that contract.
        patcher = mock.patch.object(ci_external_tests, "load_manifest", return_value=[])
        patcher.start()
        self.addCleanup(patcher.stop)

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
            ci_test_shards.ci_external_tests.regular_command(
                ["cargo", "test", "--lib", "--", "--list"], targets=["lib"],
            ),
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
                                     ci_test_shards.ci_external_tests.regular_command(
                                         ["cargo", "test", "--lib", "--", "--test-threads=2"],
                                         targets=["lib"],
                                     ))
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

    def test_task_fixture_modules_are_explicit_and_not_full_ci_shards(self):
        self.assertEqual(ci_test_shards.TASK_FIXTURE_MODULES, (
            "runtime_db::test_support::task_fixture_tests",
            "runtime_db::control_transition::tests::task_write_tests",
            "runtime_db::control_transition::tests::status_metadata_actor_tests",
        ))
        self.assertNotIn("task-fixtures", ci_test_shards.SUITES)
        self.assertEqual(ci_test_shards.LIB_COMPILE_GROUPS,
                         {"lib-a": ("a", "c"), "lib-b": ("b", "d")})
        text = MAKEFILE.read_text()
        self.assertRegex(text, r"(?m)^\.PHONY:.*\btest-task-fixtures\b")
        self.assertRegex(
            text,
            r"(?m)^test-task-fixtures:.*\n\tpython3 scripts/ci_test_shards.py task-fixtures$",
        )

    def test_task_fixture_command_selects_group_b_and_reports_real_commands(self):
        modules = ci_test_shards.TASK_FIXTURE_MODULES
        names = [f"{module}::one" for module in modules]
        names += [f"{modules[0]}::two", f"{modules[0]}_unrelated::one"]
        output = io.StringIO()
        with mock.patch.object(ci_test_shards, "lib_test_names", return_value=names) as listing, \
             mock.patch.object(ci_test_shards.subprocess, "run") as run, \
             redirect_stderr(output):
            run.return_value.returncode = 0
            args = ci_test_shards.build_parser().parse_args(["task-fixtures"])
            args.func(args)
        listing.assert_called_once_with("b")
        self.assertEqual(run.call_count, 3)
        for module, count, call in zip(modules, (2, 1, 1), run.call_args_list):
            command = [
                "cargo", "test", "--lib", f"{module}::",
                "--", "--test-threads=2",
            ]
            self.assertEqual(call.args[0],
                             ci_test_shards.ci_external_tests.regular_command(command, targets=["lib"]))
            self.assertEqual(call.kwargs["cwd"], ci_test_shards.REPO_ROOT)
            self.assertEqual(call.kwargs["env"]["HOLON_LIB_TEST_SHARD"], "b")
            self.assertIn(f"module {module}: {count} tests", output.getvalue())
            self.assertIn(
                f"$ HOLON_LIB_TEST_SHARD=b {' '.join(command)}", output.getvalue()
            )

    def test_task_fixture_zero_matches_fail_before_any_test_runs(self):
        modules = ci_test_shards.TASK_FIXTURE_MODULES
        for missing in modules:
            names = [f"{module}::one" for module in modules if module != missing]
            names.append(f"{missing}_unrelated::one")
            with self.subTest(missing=missing), \
                 mock.patch.object(ci_test_shards, "lib_test_names", return_value=names), \
                 mock.patch.object(ci_test_shards.subprocess, "run") as run:
                with self.assertRaisesRegex(ValueError, f"{missing} has no tests"):
                    ci_test_shards.command_task_fixtures(None)
                run.assert_not_called()

    def test_task_fixture_duplicate_enumeration_fails_before_any_test_runs(self):
        names = [f"{module}::one" for module in ci_test_shards.TASK_FIXTURE_MODULES]
        with mock.patch.object(ci_test_shards, "lib_test_names", return_value=names + names[:1]), \
             mock.patch.object(ci_test_shards.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "duplicate tests"):
                ci_test_shards.command_task_fixtures(None)
            run.assert_not_called()

    def test_task_fixture_overlapping_modules_are_rejected(self):
        module = ci_test_shards.TASK_FIXTURE_MODULES[0]
        with mock.patch.object(ci_test_shards, "TASK_FIXTURE_MODULES",
                               (module, f"{module}::nested")), \
             mock.patch.object(ci_test_shards, "lib_test_names",
                               return_value=[f"{module}::nested::one"]), \
             mock.patch.object(ci_test_shards.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "duplicate tests"):
                ci_test_shards.command_task_fixtures(None)
            run.assert_not_called()

    def test_task_fixture_enumeration_failure_is_propagated(self):
        failure = ci_test_shards.subprocess.CalledProcessError(7, ["cargo", "test"])
        with mock.patch.object(ci_test_shards, "lib_test_names", side_effect=failure), \
             mock.patch.object(ci_test_shards.subprocess, "run") as run:
            with self.assertRaises(ci_test_shards.subprocess.CalledProcessError) as raised:
                ci_test_shards.command_task_fixtures(None)
            self.assertEqual(raised.exception.returncode, 7)
            run.assert_not_called()

    def test_task_fixture_failure_is_propagated_after_all_modules_run(self):
        names = [f"{module}::one" for module in ci_test_shards.TASK_FIXTURE_MODULES]
        for failed in range(3):
            completed = [
                ci_test_shards.subprocess.CompletedProcess(
                    ["cargo", "test", "--lib", f"{module}::",
                     "--", "--test-threads=2"],
                    7 if index == failed else 0,
                )
                for index, module in enumerate(ci_test_shards.TASK_FIXTURE_MODULES)
            ]
            with self.subTest(failed=failed), \
                 mock.patch.object(ci_test_shards, "lib_test_names", return_value=names), \
                 mock.patch.object(ci_test_shards.subprocess, "run", side_effect=completed) as run, \
                 redirect_stderr(io.StringIO()):
                with self.assertRaises(ci_test_shards.subprocess.CalledProcessError) as raised:
                    ci_test_shards.command_task_fixtures(None)
                self.assertEqual(raised.exception.returncode, 7)
                self.assertEqual(raised.exception.cmd, completed[failed].args)
                self.assertEqual(run.call_count, 3)


if __name__ == "__main__":
    unittest.main()
