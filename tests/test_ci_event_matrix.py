"""Static event selection and stubbed command contracts, not real CI runs."""

import json
import os
import re
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = (ROOT / ".github/workflows/ci.yml").read_text()
parts = re.split(r"(?m)^  ([\w-]+):\n", WORKFLOW.split("jobs:\n", 1)[1])
JOBS = dict(zip(parts[1::2], parts[2::2]))
EVENTS = ("push", "pull_request", "schedule", "workflow_dispatch")
APPLE = {"swift-sdk", "ios-app", "macos-rust-check", "macos-rust-cross-check", "macos-app"}


def field(block, name, indent):
    match = re.search(rf"(?m)^{' ' * indent}{name}: (.+)$", block)
    return match.group(1) if match else None


def steps(job):
    result = {}
    for block in re.split(r"(?m)^      - ", JOBS[job])[1:]:
        if block.startswith("name: "):
            name, _, body = block.partition("\n")
            result[name.removeprefix("name: ")] = body
    return result


def command(step):
    match = re.search(r"(?m)^        run: (.+)$", step)
    if match.group(1) != "|":
        return match.group(1)
    lines = []
    for line in step[match.end() + 1 :].splitlines():
        if line and not line.startswith("          "):
            break
        lines.append(line)
    return textwrap.dedent("\n".join(lines))


def condition(expression, event, changed=(), labels=(), docker="success"):
    """Evaluate only the expressions used by this workflow, with fixed inputs."""
    if expression is None:
        return True
    expression = expression.removeprefix("${{").removesuffix("}}").strip()
    expression = re.sub(
        r"contains\(github.event.pull_request.labels.\*.name, '([^']+)'\)",
        lambda m: repr(m.group(1) in labels),
        expression,
    )
    expression = expression.replace("github.event_name", repr(event))
    expression = expression.replace("needs.docker.result", repr(docker))
    expression = re.sub(
        r"needs.changes.outputs.(\w+)",
        lambda m: repr(
            "true"
            if (event in EVENTS[2:] if m.group(1) == "full_sweep" else m.group(1) in changed)
            else "false"
        ),
        expression,
    )
    expression = expression.replace("always()", "True").replace("&&", " and ").replace("||", " or ")
    return eval(expression, {"__builtins__": {}}, {})


def selected(event, changed=(), labels=()):
    return {
        name for name, body in JOBS.items()
        if condition(field(body, "if", 4), event, changed, labels)
    }


class CiEventMatrixTests(unittest.TestCase):
    def test_main_push_component_matrix(self):
        for component, expected in {
            "rust": {"rust-build", "conversation-sdk", "docker", "timing"},
            "web": {"web", "docker"},
            "conversation_sdk": {"conversation-sdk"},
            "android": {"android"},
            "docker": {"docker"},
            "template_validation": set(),
        }.items():
            with self.subTest(component=component):
                self.assertEqual(selected("push", {component}), expected | {"changes"})
        self.assertEqual(selected("push"), {"changes"})

    def test_pr_and_full_sweep_coverage(self):
        rust = {
            "changes", "conversation-sdk", "rust-lint", "rust-build",
            "rust-test", "rust-concurrent", "scheduler-e2e-required", "timing",
        }
        self.assertEqual(selected("pull_request", {"rust"}), rust)
        self.assertEqual(selected("pull_request", {"web"}), {"changes", "web"})
        self.assertEqual(selected("pull_request", {"android"}), {"changes", "android"})
        for event in EVENTS[2:]:
            self.assertEqual(selected(event), set(JOBS) - {"e2e-scheduler"})
        for event in EVENTS[:2]:
            self.assertFalse(selected(event, {"rust", "web", "android"}) & APPLE)
        self.assertIn(
            "task-result-controlled-required",
            selected("pull_request", {"rust"}, {"recovery-acceptance"}),
        )
        self.assertIn("e2e-scheduler", selected("pull_request", {"rust"}, {"e2e-scheduler"}))
        self.assertIn('cron: "40 2 * * *"', WORKFLOW)

    def test_mixed_steps_only_test_outside_push(self):
        for job, names in {
            "web": ["Test and build web GUI", "Set up Rust", "Set up Java", "Set up Gradle"],
            "conversation-sdk": ["Test conversation SDK protocol"],
            "docker": [
                "Smoke test image", "Validate Docker E2E runner",
                "Export image for downstream E2E jobs", "Upload image for downstream E2E jobs",
            ],
            "rust-build": ["Package check", "Check generated snapshots"],
            "android": ["Verify Android screenshots and lint"],
        }.items():
            for name in names:
                expression = field(steps(job)[name], "if", 8)
                for event in EVENTS:
                    with self.subTest(job=job, step=name, event=event):
                        self.assertEqual(
                            bool(condition(expression, event, {"rust", "docker"})),
                            event != "push",
                        )
        for job, name in [("web", "Build web GUI"), ("conversation-sdk", "Build conversation SDK")]:
            step = steps(job)[name]
            self.assertEqual(field(step, "if", 8), "github.event_name == 'push'")
        self.assertEqual(command(steps("conversation-sdk")["Build conversation SDK"]),
                         "npm ci && npm run build")
        self.assertEqual(command(steps("web")["Build web GUI"]), "make web")
        self.assertEqual(command(steps("web")["Test and build web GUI"]), "make web-ci")
        self.assertEqual(command(steps("conversation-sdk")["Test conversation SDK protocol"]),
                         "make conversation-sdk-ci")
        self.assertIn("holon-web-retrieval/Cargo.toml -- --check",
                      command(steps("rust-lint")["Check independent crate formatting"]))

    def run_shell(self, script, cwd, env):
        return subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c", script],
            cwd=cwd, env={**os.environ, **env}, text=True, capture_output=True, timeout=30,
        )

    def stub(self, path, log):
        path.write_text(
            "#!/usr/bin/env python3\nimport json, os, sys\n"
            f"with open({str(log)!r}, 'a') as f:\n"
            "    f.write(json.dumps([os.getcwd(), sys.argv]) + '\\n')\n"
            "code = int(os.environ.get('STUB_EXIT', '0'))\n"
            "if os.environ.get('STUB_FAIL_ARG', sys.argv[1]) not in sys.argv[1:]: code = 0\n"
            "sys.exit(code)\n"
        )
        path.chmod(0o755)

    def test_independent_crates_check_on_push_and_test_elsewhere(self):
        step = steps("rust-build")["Check or test independent Rust crates"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / "commands.jsonl"
            self.stub(root / "cargo", log)
            for event in EVENTS:
                mode = condition(field(step, "CRATE_COMMAND", 10), event)
                log.unlink(missing_ok=True)
                env = {"PATH": f"{root}:{os.environ['PATH']}", "CRATE_COMMAND": mode}
                result = self.run_shell(command(step), root, env)
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = [json.loads(line)[1][1:] for line in log.read_text().splitlines()]
                self.assertEqual(len(calls), 21)
                self.assertEqual(len({call[2] for call in calls}), 21)
                for call in calls:
                    self.assertEqual(call[0], "check" if event == "push" else "test")
                    self.assertEqual(call[1], "--manifest-path")
                    self.assertEqual(call[3:], ["--locked"])
                self.assertIn("crates/holon-web-retrieval/Cargo.toml", [call[2] for call in calls])
                log.unlink()
                result = self.run_shell(command(step), root, {**env, "STUB_EXIT": "42"})
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(log.read_text().splitlines()), 1)

    def test_android_build_tasks_and_failure_propagation(self):
        step = steps("android")["Test SDK and build APKs"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cwd = root / "apps/android"
            cwd.mkdir(parents=True)
            (root / "scripts").mkdir()
            log = root / "commands.jsonl"
            self.stub(cwd / "gradlew", log)
            for name in ["test-android-release-version.sh", "test-android-release-packaging.sh"]:
                (root / "scripts" / name).write_text(f"echo '[\"tooling\", []]' >> '{log}'\n")
            for name in ["test-android-release-channels.py", "verify-android-release-channels.py"]:
                (root / "scripts" / name).write_text(
                    f"with open({str(log)!r}, 'a') as f: f.write('[\"tooling\", []]\\n')\n"
                )
            for event in EVENTS:
                log.unlink(missing_ok=True)
                run_tests = condition(field(step, "RUN_TESTS", 10), event)
                env = {"RUN_TESTS": str(run_tests).lower(), "RUNNER_TEMP": directory}
                result = self.run_shell(command(step), cwd, env)
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = [json.loads(line) for line in log.read_text().splitlines()]
                gradle = next(args for location, args in calls if location != "tooling")
                self.assertEqual(":sdk:test" in gradle, event != "push")
                self.assertEqual(":app:testDirectDebugUnitTest" in gradle, event != "push")
                self.assertEqual(":app:testPlayDebugUnitTest" in gradle, event != "push")
                self.assertEqual(len(calls), 1 if event == "push" else 5)
                for task in [":app:assembleDirectDebug", ":app:assemblePlayDebug",
                             ":app:assembleDirectRelease", ":app:assemblePlayRelease",
                             ":app:bundlePlayRelease"]:
                    self.assertIn(task, gradle)
                result = self.run_shell(command(step), cwd, {**env, "STUB_EXIT": "42"})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("not retrying", result.stdout)

    def test_web_and_rust_build_call_chains(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / "commands.jsonl"
            for tool in ["npm", "cargo"]:
                self.stub(root / tool, log)
            env = {"PATH": f"{root}:{os.environ['PATH']}", "HOME": directory}
            for target in ["web", "build"]:
                log.unlink(missing_ok=True)
                result = self.run_shell(f"make {target}", ROOT, env)
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = [json.loads(line)[1][1:] for line in log.read_text().splitlines()]
                self.assertTrue(calls)
                for args in calls:
                    if "test" in args:
                        self.assertEqual(target, "build")
                        self.assertIn("--no-run", args)
                if target == "build":
                    self.assertIn(["build", "--all-targets"], calls)
                result = self.run_shell(
                    f"make {target}", ROOT,
                    {**env, "STUB_EXIT": "42", "STUB_FAIL_ARG": "build"},
                )
                self.assertNotEqual(result.returncode, 0)

    def test_npm_build_and_lifecycle_scripts_do_not_run_tests(self):
        for package in ["api-sdk", "app-sdk", "conversation-sdk", "../web-gui/app"]:
            path = ROOT / "packages" / package / "package.json"
            scripts = json.loads(path.read_text())["scripts"]
            for name in ["build", "build:sdk", "typecheck", "prebuild", "postbuild",
                         "preinstall", "install", "postinstall", "prepare"]:
                with self.subTest(package=package, script=name):
                    self.assertNotRegex(
                        scripts.get(name, ""),
                        r"\b(test|vitest|playwright|pytest|unittest)\b",
                    )

    def test_build_jobs_do_not_suppress_failures(self):
        for name in ["web", "conversation-sdk", "android", "rust-build", "docker"]:
            self.assertNotIn("continue-on-error:", JOBS[name])
        self.assertIn("always()", field(JOBS["timing"], "if", 4))
        self.assertIn("rust-build", JOBS["timing"])


if __name__ == "__main__":
    unittest.main()
