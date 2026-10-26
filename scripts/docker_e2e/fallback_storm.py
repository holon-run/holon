"""Isolated fault acceptance built on the Docker E2E evidence conventions."""
from __future__ import annotations

import argparse
from collections import Counter
import json
import re
import secrets
import subprocess
import time
from pathlib import Path

from .runner import redact_evidence, require, write_json

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "tests/e2e/docker"
SCRIPT = "/fixtures/fault_proxy/server.py"
LABEL = "holon.e2e.suite=fallback-storm"
CASES = {
    "C00": {},
    "N01": {"FAULT_A": "rst"},
    "N02": {"FAULT_A": "rst", "FAULT_B": "rst"},
    "N03-headers": {"FAULT_A": "blackhole_headers"},
    "N03-body": {"FAULT_A": "blackhole_body"},
    "N03-idle": {"FAULT_A": "blackhole_idle"},
    "H01-429": {"STATUS_A": "429"},
    "H01-503": {"STATUS_A": "503"},
}


def command(*args, check=True, data=None, timeout=30):
    return subprocess.run(list(args), input=data, text=True, capture_output=True,
                          check=check, timeout=timeout)


def request_bound(case, elapsed=0):
    """Dispatch guard, not the legacy in-turn retry policy, owns these calls.

    Persistent network recovery has three dispatches per epoch. Explicit due
    rechecks may start another network epoch (minimum transport floor 10s);
    there is no documented finite lifetime dispatch cap for a network outage.
    """
    return 3 * (1 + int(elapsed // 10)) if case == "N02" else 3


def require_build(version, expected_revision):
    """Check the binary, not an image tag or an OCI label."""
    revision = re.search(r"\(([0-9a-f]{8,40})\)", version)
    require(re.fullmatch(r"[0-9a-f]{8,40}", expected_revision) is not None,
            "expected revision must contain 8-40 lowercase hex characters")
    require(revision is not None, "binary version has no verifiable revision")
    actual = revision.group(1)
    require(actual.startswith(expected_revision) or expected_revision.startswith(actual),
            f"binary revision {actual} does not match {expected_revision}")


def evaluate(case, events, requests, proxy):
    completed = [e for e in events if e["type"] == "turn_terminal"
                 and e.get("payload", {}).get("kind") == "completed"]
    briefs = [e for e in events if e["type"] == "brief_created"
              and e.get("payload", {}).get("kind") == "result"]
    require(requests or proxy, "fault was not exercised")
    if case == "N02":
        require({p["side"] for p in proxy} == {"a", "b"}, "both routes must fail")
        require(not requests, "RST must not reach the HTTP mock")
        require(not completed and not briefs, "unreachable routes cannot complete the task")
        return "needs-parking-review"
    require(len(requests) <= request_bound(case), "HTTP request budget exceeded")
    require(len(proxy) <= request_bound(case), "TCP connection budget exceeded")
    require(len(completed) == 1, "expected exactly one completed turn")
    require(completed[0]["payload"].get("last_assistant_message") == "QA_FALLBACK_COMPLETED",
            "completed turn did not deliver the success marker")
    require(len(briefs) == 1, "expected exactly one final delivery")
    if case == "C00":
        require(len(requests) == len(proxy) == 1 and
                requests[0]["side"] == "a" and requests[0]["status"] == 200,
                "control must complete with one primary HTTP 200")
    else:
        success = [r for r in requests if r["status"] == 200]
        require(len(success) == 1 and success[0]["side"] == "b",
                "fallback must complete with exactly one backup HTTP 200")
        require(any(p["side"] == "a" for p in proxy), "primary fault was not exercised")
    return "verified-w1-isolated"


def persistent_budget_evidence(evidence):
    """Join wire observations to the original prompt's durable episode."""
    prompt = json.loads((evidence / "prompt.json").read_text())
    snapshot = json.loads((evidence / "database.json").read_text())
    database = snapshot["rows"]
    events_page = json.loads((evidence / "events.json").read_text())
    require(not events_page.get("has_newer"), "event capture truncated before final cursor")
    require(not snapshot.get("truncated_tables"), "database capture truncated")
    episodes = [e for e in database["provider_recovery_episodes"]
                if json.loads(e["binding_json"])["root_message_id"] == prompt["message_id"]]
    require(len(episodes) == 1, "one prompt must retain one canonical recovery episode")
    episode = episodes[0]
    requests = [r for r in database["provider_recovery_requests"]
                if r["episode_key"] == episode["episode_key"]]
    require(len({r["request_id"] for r in requests}) == len(requests),
            "duplicate durable request ID")
    per_epoch = Counter(r["epoch"] for r in requests)
    require(all(count <= 3 for count in per_epoch.values()), "persistent epoch budget exceeded")
    require(episode["total_requests"] == len(requests),
            "durable total disagrees with request ledger")
    proxy = json.loads((evidence / "proxy-tcp.json").read_text())
    require(len(requests) == len(proxy), "wire and durable dispatch ledger disagree")
    turns = [e for e in events_page["events"] if e["type"] == "turn_started"]
    require(len(turns) == len(requests), "unexpected empty model turn or missing dispatch")
    return {
        "root_message_id": prompt["message_id"], "episode_key": episode["episode_key"],
        "dispatches_by_epoch": dict(per_epoch), "total_dispatches": len(requests),
        "model_turns": len(turns), "last_state": episode["state"],
        "last_epoch": episode["epoch"], "not_before": episode["not_before"],
        "parking": database["provider_queue_parking"],
        "root_and_budget_invariants": "pass",
        "lifetime_permanent_parking": "not-required-by-current-rfc",
    }


class FaultRun:
    def __init__(self, args, case):
        self.args, self.case = args, case
        self.prefix = f"qa-fbstorm-{case.lower()}-{secrets.token_hex(3)}"
        self.evidence = args.evidence_dir / self.prefix
        self.evidence.mkdir(parents=True)
        self.containers = []
        self.networks = []
        self.volume = self.prefix + "-home"
        self.token = secrets.token_hex(24)
        self.daemon, self.controller = self.prefix + "-daemon", self.prefix + "-controller"
        self.proxy, self.mock = self.prefix + "-proxy", self.prefix + "-mock"

    def docker(self, *args, **kwargs):
        return command("docker", *map(str, args), **kwargs)

    def container(self, name, image, network, *args, env=None, mounts=()):
        cmd = ["run", "-d", "--pull=never", "--name", name, "--label", LABEL,
               "--network", network, "--cpus", "1", "--memory", "768m",
               "--pids-limit", "192", "--cap-drop", "ALL",
               "--security-opt", "no-new-privileges",
               "--log-opt", "max-size=10m", "--log-opt", "max-file=2"]
        for key, value in (env or {}).items():
            cmd += ["-e", f"{key}={value}"]
        for mount in mounts:
            cmd += ["-v", mount]
        cmd += ["--user", "10001:10001", "--entrypoint", "python"] if image == self.args.python_image else []
        cmd += [image, *args]
        self.docker(*cmd)
        self.containers.append(name)

    def api(self, method, path, body=None):
        payload = {"method": method, "url": "http://daemon:7878" + path,
                   "body": body, "token": self.token}
        result = self.docker("exec", "-i", self.controller, "python", SCRIPT, "request",
                             data=json.dumps(payload))
        envelope = json.loads(result.stdout)
        if envelope["status"] not in (200, 201, 202):
            raise AssertionError(f"{method} {path}: {envelope}")
        return envelope["body"]

    def start(self):
        for suffix in ("control", "front", "back"):
            name = self.prefix + "-" + suffix
            self.docker("network", "create", "--internal", "--label", LABEL, name)
            self.networks.append(name)
        control, front, back = self.networks
        self.docker("volume", "create", "--label", LABEL, self.volume)
        fixtures = f"{FIXTURES}:/fixtures:ro"
        self.container(self.mock, self.args.python_image, back, SCRIPT, "mock",
                       mounts=[fixtures])
        self.docker("network", "disconnect", back, self.mock)
        self.docker("network", "connect", "--alias", "mock", back, self.mock)
        self.container(self.proxy, self.args.python_image, front, SCRIPT, "proxy",
                       mounts=[fixtures])
        self.docker("network", "connect", back, self.proxy)
        self.docker("network", "disconnect", front, self.proxy)
        self.docker("network", "connect", "--alias", "proxy", front, self.proxy)
        self.container(self.controller, self.args.python_image, control, SCRIPT, "controller",
                       mounts=[fixtures, f"{self.volume}:/evidence-home:ro"])
        routes = [("qa-a", "a", 8081), ("qa-b", "b", 8082)]
        routes.extend(getattr(self, "extra_routes", []))
        config = {
            "model": {"default": "qa-a/gpt-5.4", "fallbacks": ["qa-b/gpt-5.4"]},
            "providers": {
                name: {"transport": "openai_responses",
                       "base_url": f"http://proxy:{port}/{side}/v1",
                       "auth": {"kind": "none", "source": "none"}}
                for name, side, port in routes
            },
            "models": {"catalog": {route: {"context_window": 128000,
                                           "max_output_tokens": 4096}
                                   for route in (f"{name}/gpt-5.4" for name, _, _ in routes)}},
        }
        # Only this new empty volume is initialized; no host configuration.
        self.docker("run", "--rm", "-i", "--pull=never", "--network", "none", "--user", "0",
                    "-v", f"{self.volume}:/var/lib/holon", "--entrypoint", "bash",
                    self.args.image, "-c",
                    "cat > /var/lib/holon/config.json; chown -R 10001:10001 /var/lib/holon",
                    data=json.dumps(config))
        http_timeout = self.args.http_timeout
        body_timeout = http_timeout
        idle_timeout = http_timeout * 1000
        if self.case == "N03-headers":
            body_timeout, idle_timeout = 7, 7000
        elif self.case == "N03-body":
            http_timeout, body_timeout, idle_timeout = 8, 2, 7000
        elif self.case == "N03-idle":
            http_timeout, body_timeout, idle_timeout = 8, 7, 1000
        env = {"HOLON_CONTROL_TOKEN": self.token, "HOLON_MODEL": "qa-a/gpt-5.4",
               "HOLON_MODEL_FALLBACKS": '["qa-b/gpt-5.4"]',
               "HOLON_DISABLE_PROVIDER_FALLBACK": "false",
               "HOLON_PROVIDER_HTTP_TIMEOUT_SECS": str(http_timeout),
               "HOLON_PROVIDER_RESPONSE_BODY_TIMEOUT_SECS": str(body_timeout),
               "HOLON_PROVIDER_STREAM_IDLE_TIMEOUT_MS": str(idle_timeout)}
        self.daemon_env = env
        self.start_daemon()
        readiness = self.wait_ready()
        write_json(self.evidence / "readiness.json", readiness)
        version = self.docker("exec", self.daemon, "holon", "--version").stdout.strip()
        write_json(self.evidence / "build.json",
                   {"expected_revision": self.args.expected_revision, "binary_version": version,
                    "image": self.args.image, "python_image": self.args.python_image})
        require_build(version, self.args.expected_revision)
        inspect = json.loads(self.docker("inspect", *self.containers).stdout)
        for item in inspect:
            require(not item["HostConfig"]["PortBindings"], "port published")
            require(item["HostConfig"]["NetworkMode"] != "host", "host network attached")
            require(all(m["Source"] == str(FIXTURES) or m["Type"] == "volume"
                        for m in item["Mounts"]), "unexpected host mount")
        write_json(self.evidence / "topology.json", redact_evidence([
            {"name": i["Name"], "image": i["Image"], "mounts": i["Mounts"],
             "networks": list(i["NetworkSettings"]["Networks"]),
             "ports": i["HostConfig"]["PortBindings"]} for i in inspect]))
        network_info = json.loads(self.docker("network", "inspect", *self.networks).stdout)
        require(all(n["Internal"] for n in network_info), "external network attached")
        write_json(self.evidence / "networks.json", network_info)
        for name in self.containers:
            route = self.docker("exec", name, "cat", "/proc/net/route").stdout
            require(not any(line.split()[1] == "00000000" for line in route.splitlines()[1:]),
                    "default external route attached")
            (self.evidence / f"{name}-route.txt").write_text(route)
        write_json(self.evidence / "policy.json",
                   {"max_retries": 2, "rate_limit_retries": 3, "fallback_limit": 2,
                    "shared_dispatch_guard": True, "dispatches_per_epoch": 3,
                    "automatic_network_probe_epochs": True,
                    "lifetime_network_dispatch_cap": None,
                    "transport_floor_seconds": [10, 30, 120, 300],
                    "task_result_recovery_applicable": False,
                    "http_request_upper_bound": request_bound(self.case, self.args.timeout),
                    "http_timeout_seconds": http_timeout,
                    "body_timeout_seconds": body_timeout,
                    "idle_timeout_ms": idle_timeout,
                    "default_deadlines_tested": False, "workload": "W1"})

    def start_daemon(self):
        control, front, _ = self.networks
        self.container(self.daemon, self.args.image, control, "serve", "--listen",
                       "0.0.0.0:7878", env=self.daemon_env,
                       mounts=[f"{self.volume}:/var/lib/holon"])
        self.docker("network", "connect", front, self.daemon)
        self.docker("network", "disconnect", control, self.daemon)
        self.docker("network", "connect", "--alias", "daemon", control, self.daemon)

    def wait_ready(self):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                readiness = self.api("GET", "/api/control/runtime/readiness")
                if readiness.get("healthy") is True:
                    return readiness
            except (subprocess.SubprocessError, AssertionError, ValueError):
                pass
            time.sleep(.25)
        raise AssertionError("isolated daemon readiness failed")

    def observe(self):
        state = self.api("GET", "/api/agents/qa/state")
        logs = self.docker("logs", self.mock).stdout.splitlines()
        requests = [json.loads(line) for line in logs if line.startswith("{")]
        proxy = [json.loads(line) for line in self.docker("logs", self.proxy).stdout.splitlines()
                 if line.startswith("{")]
        return state, requests, proxy

    def run(self):
        self.start()
        created = self.api("POST", "/api/control/agents/qa/create", {})
        write_json(self.evidence / "create.json", created)
        bootstrap_deadline = time.monotonic() + 45
        while time.monotonic() < bootstrap_deadline:
            events = self.api("GET", "/api/agents/qa/events?limit=200&order=asc")
            require(not events.get("has_newer"), "bootstrap event capture truncated")
            if any(e["type"] == "turn_terminal" and
                   e.get("payload", {}).get("kind") == "completed"
                   for e in events["events"]):
                break
            time.sleep(.5)
        else:
            raise AssertionError("healthy bootstrap did not complete")
        _, baseline_http, baseline_tcp = self.observe()
        write_json(self.evidence / "bootstrap-events.json", redact_evidence(events))
        write_json(self.evidence / "bootstrap-http.json", baseline_http)
        cursor = events.get("cursor_seq") or max(e["seq"] for e in events["events"])
        for name in (self.mock, self.proxy):
            self.docker("exec", "-i", name, "python", "-c",
                        "import pathlib,sys; pathlib.Path('/tmp/qa-fault.json').write_text(sys.stdin.read())",
                        data=json.dumps(CASES[self.case]))
        prompt = self.api("POST", "/api/control/agents/qa/prompt",
                          {"text": "Reply with QA_FALLBACK_COMPLETED. No tools."})
        write_json(self.evidence / "prompt.json", prompt)
        started = time.monotonic()
        last_activity, previous = started, None
        while time.monotonic() - started < self.args.timeout:
            state, requests, proxy = self.observe()
            requests, proxy = requests[len(baseline_http):], proxy[len(baseline_tcp):]
            events = self.api("GET", f"/api/agents/qa/events?limit=200&order=asc&after_seq={cursor}")
            elapsed = time.monotonic() - started
            signature = (len(requests), len(proxy), events.get("cursor_seq"),
                         len(events["events"]))
            if signature != previous:
                previous, last_activity = signature, time.monotonic()
            write_json(self.evidence / "state.json", redact_evidence(state))
            write_json(self.evidence / "mock-http.json", requests)
            write_json(self.evidence / "proxy-tcp.json", proxy)
            write_json(self.evidence / "events.json", redact_evidence(events))
            require(len(requests) <= request_bound(self.case, elapsed), "HTTP request budget exceeded")
            require(len(proxy) <= request_bound(self.case, elapsed), "TCP connection budget exceeded")
            require(not events.get("has_newer"), "event capture truncated before final cursor")
            # A stable snapshot is observation, NOT a pass. Final criteria
            # are independently evaluated from durable turns and delivery.
            quiet = max(self.args.quiet_seconds, 65 if self.case == "N02" else 0)
            if time.monotonic() - last_activity >= quiet:
                status = evaluate(self.case, events["events"], requests, proxy)
                return {"case": self.case, "status": status,
                        "http_requests": len(requests), "tcp_connections": len(proxy),
                        "elapsed_seconds": round(time.monotonic() - started, 2),
                        "quiet_seconds": quiet,
                        "evidence": str(self.evidence)}
            time.sleep(1)
        raise TimeoutError("case did not stabilize within the harness deadline")

    def cleanup(self):
        errors = []

        def attempt(*args):
            try:
                result = self.docker(*args, check=False)
                if result.returncode:
                    errors.append({"command": list(args), "error": result.stderr})
                return result
            except Exception as error:
                errors.append({"command": list(args), "error": str(error)})
                return None

        # Freeze dispatch before collecting logs/DB; live snapshots are not atomic.
        if self.daemon in self.containers:
            attempt("stop", "--time", "5", self.daemon)
        for name in self.containers:
            result = attempt("logs", name)
            if result is not None:
                (self.evidence / f"{name}.log").write_text(
                    redact_evidence(result.stdout + result.stderr))
        if self.controller in self.containers:
            result = attempt("exec", self.controller, "python", SCRIPT,
                             getattr(self, "database_mode", "database"))
            if result is not None and result.returncode == 0:
                try:
                    write_json(self.evidence / "database.json", json.loads(result.stdout))
                except ValueError as error:
                    errors.append({"command": ["database"], "error": str(error)})
        for name in reversed(self.containers):
            attempt("rm", "-f", name)
        for network in reversed(self.networks):
            attempt("network", "rm", network)
        attempt("volume", "rm", self.volume)
        write_json(self.evidence / "cleanup.json", {"errors": errors})
        return errors


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--python-image", required=True)
    parser.add_argument("--expected-revision", required=True,
                        help="8-40 character source revision; checked against the running binary")
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--case", action="append", choices=CASES)
    parser.add_argument("--http-timeout", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--quiet-seconds", type=int, default=20)
    args = parser.parse_args(argv)
    if min(args.http_timeout, args.timeout, args.quiet_seconds) <= 0:
        parser.error("timeouts and quiet seconds must be positive")
    if not re.fullmatch(r"[0-9a-f]{8,40}", args.expected_revision):
        parser.error("--expected-revision must contain 8-40 lowercase hex characters")
    args.evidence_dir = args.evidence_dir.resolve()
    # Resolve mutable tags once; never pull images or inherit provider credentials.
    for field in ("image", "python_image"):
        result = command("docker", "image", "inspect", getattr(args, field),
                         "--format", "{{.Id}}")
        setattr(args, field, result.stdout.strip())
    results = []
    for case in args.case or CASES:
        run = FaultRun(args, case)
        try:
            result = run.run()
        except Exception as error:
            result = {"case": case, "status": "failed-or-blocked",
                      "error": str(error), "evidence": str(run.evidence)}
        except KeyboardInterrupt:
            result = {"case": case, "status": "failed-or-blocked",
                      "error": "interrupted", "evidence": str(run.evidence)}
        finally:
            cleanup_errors = run.cleanup()
        if cleanup_errors:
            result["status"] = "failed-or-blocked"
            result["cleanup_errors"] = cleanup_errors
        if (run.evidence / "prompt.json").exists():
            try:
                budget = persistent_budget_evidence(run.evidence)
                write_json(run.evidence / "budget-analysis.json", budget)
            except (AssertionError, KeyError, ValueError, OSError) as error:
                result["status"] = "failed-or-blocked"
                result["budget_error"] = str(error)
        results.append(result)
        write_json(args.evidence_dir / "summary.json", results)
        print(json.dumps(result), flush=True)
        if result.get("error") == "interrupted":
            return 130
    return 0 if all(r["status"] == "verified-w1-isolated" for r in results) else 1
