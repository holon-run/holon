"""Real-policy route-throttle acceptance against an isolated, revision-pinned daemon."""
from __future__ import annotations

import argparse
from collections import Counter
from datetime import datetime
from email.utils import parsedate_to_datetime
import json
import re
import signal
import time
import traceback
from pathlib import Path

from .fallback_storm import FaultRun, command, require_build
from .runner import redact_evidence, require, write_json
from .task_result_oracle import require_result_brief

SCRIPT = "/fixtures/fault_proxy/server.py"
CASES = {
    f"no-identity-{header}-{routes}": (header, routes)
    for header in ("relative", "absolute") for routes in ("healthy", "both")
}
LAYERED_COVERAGE = {
    "missing_malformed_zero_retry_after": {
        "layer": "Rust unit (not executed by this Python gate)",
        "test": "provider::retry::tests::parse_retry_after_ignores_missing_malformed_and_zero_values",
    },
    "past_http_date": {
        "layer": "Rust unit (not executed by this Python gate)",
        "test": "provider::retry::tests::parse_retry_after_ignores_past_http_date",
    },
    "identity_shared_quota_and_isolation": {
        "layer": "Rust unit (not executed by this Python gate)",
        "test": "api_credential_quota_identity_is_coarse_and_does_not_cross_endpoint_or_credential",
    },
    "network_origin": "not covered: this gate uses authenticated Operator prompts",
    "task_result_consumption": "not applicable: no task is created; W2 is a separate gate",
}


def timestamp(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()


def header_deadline(row):
    value = row["retry_after"]
    return (row["at"] + int(value) if str(value).isdigit()
            else parsedate_to_datetime(value).timestamp())


def root_episode(rows, root):
    found = [e for e in rows["provider_recovery_episodes"]
             if json.loads(e["binding_json"])["root_message_id"] == root]
    require(len(found) == 1, f"root must retain one canonical episode: {found}")
    return found[0]


def episode_requests(rows, episode):
    return sorted([r for r in rows["provider_recovery_requests"]
                   if r["episode_key"] == episode["episode_key"]],
                  key=lambda r: (r["dispatched_at"], r["request_id"]))


def require_no_identity(rows):
    require(not rows["provider_quota_incidents"], "unexpected quota identity in no-auth gate")
    for row in rows["provider_recovery_requests"]:
        require(all(s["scope"]["kind"] != "quota" for s in json.loads(row["scopes_json"])),
                "no-auth route acquired a fabricated quota identity")


def require_budget(rows, roots, wire):
    requests = []
    episodes = []
    for root in roots:
        episode = root_episode(rows, root)
        ledger = episode_requests(rows, episode)
        require(len({r["request_id"] for r in ledger}) == len(ledger), "duplicate dispatch ID")
        counts = Counter(r["epoch"] for r in ledger)
        require(all(count <= 3 for count in counts.values()), "epoch exceeded three dispatches")
        require(episode["total_requests"] == len(ledger), "persistent budget was refunded/reset")
        require(episode["request_count"] == counts[episode["epoch"]],
                "current epoch count disagrees with durable ledger")
        failed = [(r["epoch"], r["failed_scope_json"]) for r in ledger
                  if r["outcome"] == "rate_limited"]
        require(len(set(failed)) == len(failed), "same failed scope charged twice in one epoch")
        require(all(json.loads(scope)["kind"] == "route_throttle" for _, scope in failed),
                "429 was not charged to the configured route-throttle scope")
        requests += ledger
        episodes.append(episode)
    require(len(requests) == len(wire), "wire requests differ from durable dispatch charges")
    outcomes = Counter(r["outcome"] for r in requests)
    require(outcomes["rate_limited"] == sum(r["status"] == 429 for r in wire),
            "a real HTTP 429 was not durably settled as RateLimited")
    require(outcomes["available_response"] == sum(r["status"] == 200 for r in wire),
            "healthy HTTP success disagrees with durable settlement")
    require(outcomes["non_recovery_response"] + outcomes["other_failure"]
            == sum(r["status"] == 400 for r in wire)
            and sum(outcomes[k] for k in ("available_response", "rate_limited",
                                         "non_recovery_response", "other_failure")) == len(wire),
            "wire response has an unsettled or unexpected durable outcome")
    require_no_identity(rows)
    return episodes


def require_completion(rows, root, agent="qa"):
    turns = completed_sources(rows, root, agent)
    require(len(turns) == 1, "expected exactly one completed source-bound turn")
    turn = turns[0]
    stored = json.loads(turn["payload_json"])
    require(stored["owner"] == json.loads(root_episode(rows, root)["binding_json"])["owner"],
            "completed fallback turn changed the original root owner")
    require_result_brief(rows, agent_id=agent, turn_id=turn["turn_id"],
                         message_id=turn["trigger_message_id"], work_item_id=None)
    return turn["turn_id"]


def require_restart_preserved(before, after, root):
    # No updated-at normalization: these are the exact persisted fences/budget.
    require(root_episode(before, root) == root_episode(after, root),
            "crash/restart changed episode identity, epoch, cursor, deadline or budget")
    for table in ("provider_recovery_requests", "provider_route_throttle_incidents",
                  "provider_queue_parking"):
        ordered = lambda rows: sorted(rows[table], key=lambda r: json.dumps(r, sort_keys=True))
        require(ordered(before) == ordered(after), f"crash/restart changed {table}")


def require_cooling_observation(before, after, root, wire, previous_count, observed_at):
    due = timestamp(root_episode(before, root)["not_before"])
    require(all(row["at"] >= due for row in wire[previous_count:]),
            "extra HTTP before durable episode deadline")
    prior_ids = {r["request_id"] for r in episode_requests(before, root_episode(before, root))}
    for request in episode_requests(after, root_episode(after, root)):
        if request["request_id"] not in prior_ids:
            require(timestamp(request["dispatched_at"]) >= due,
                    "dispatch charge before durable episode deadline")
    # Docker DB/log collection can straddle the deadline; check exact dispatch
    # times above, and compare frozen state only for wholly pre-deadline samples.
    if observed_at < due:
        require_restart_preserved(before, after, root)


def require_failed_cycle(rows, root, wire, previous=None):
    episode = require_budget(rows, [root], wire)[0]
    ledger = episode_requests(rows, episode)
    require(episode["state"] in {"cooling", "budget_exhausted"}, "429 sequence resolved/ready")
    require(all(r["outcome"] == "rate_limited" for r in ledger), "unexpected probe outcome")
    require({r["side"] for r in wire} == {"a", "b"}, "both real routes were not exercised")
    require(bool(episode["not_before"]), "missing durable episode deadline")
    parking = [p for p in rows["provider_queue_parking"]
               if p["episode_key"] == episode["episode_key"]]
    require(len(parking) == 1 and parking[0]["agent_id"]
            == json.loads(episode["binding_json"])["agent_id"],
            "root lacks canonical durable parking")
    require(timestamp(parking[0]["not_before"]) >= timestamp(episode["not_before"]),
            "parking bypassed the episode deadline")
    require(not completed_sources(rows, root), "failed probes fabricated completion")
    incidents = rows["provider_route_throttle_incidents"]
    require(len(incidents) == 2 and all(i["resolved_at"] is None for i in incidents),
            "failed routes were cleared or not persisted")
    for side, incident in ((side, next(i for i in incidents
                                     if i["last_model_ref"].split("/")[0].split("@")[0]
                                     == f"qa-{side}"))
                           for side in ("a", "b")):
        latest = [r for r in wire if r["side"] == side][-1]
        require(timestamp(incident["server_not_before"]) >= header_deadline(latest) - .01,
                "Retry-After deadline was shortened")
        require(timestamp(incident["next_retry_at"]) >= timestamp(incident["server_not_before"]),
                "local cooldown bypassed the server deadline")
    if previous:
        old = root_episode(previous, root)
        require(episode["episode_key"] == old["episode_key"]
                and episode["binding_json"] == old["binding_json"], "probe changed source/owner")
        require(episode["epoch"] == old["epoch"] + 1, "probe skipped/reopened an epoch")
        require(episode["total_requests"] > old["total_requests"], "no real failed probe occurred")
        require(episode["failed_epochs"] > old["failed_epochs"], "failed cycle did not advance")
        new = [r for r in ledger if r["epoch"] == episode["epoch"]]
        require(all(timestamp(r["dispatched_at"]) >= timestamp(old["not_before"]) for r in new),
                "probe dispatched before the previous durable deadline")
        for request in new:
            failed = json.loads(request["failed_scope_json"])
            leases = [s for s in json.loads(request["scopes_json"]) if s["scope"] == failed]
            require(len(leases) == 1 and leases[0]["was_probe"] is True,
                    "due retry was not an admitted route-throttle probe")
        old_incidents = {i["identity_key"]: i for i in previous["provider_route_throttle_incidents"]}
        for incident in incidents:
            old_incident = old_incidents[incident["identity_key"]]
            require(incident["consecutive_failures"] >= old_incident["consecutive_failures"],
                    "unrelated response reset a route failure sequence")
            for request in new:
                scope = json.loads(request["failed_scope_json"])
                if scope["key"] == incident["identity_key"]:
                    require(timestamp(request["dispatched_at"])
                            >= timestamp(old_incident["next_retry_at"]),
                            "probe bypassed a route cooldown")
    return episode


class RouteThrottleRun(FaultRun):
    def __init__(self, args, case):
        super().__init__(args, case)
        self.header, self.routes = CASES[case]
        self.database_mode = "route-throttle-database"
        self.baseline_http, self.baseline_tcp = [], []
        self.roots = []

    def configure(self, b_status):
        hint = {"seconds": self.args.retry_after_seconds,
                "format": "http-date" if self.header == "absolute" else "seconds"}
        settings = {"STATUS_A": 429, "STATUS_B": b_status,
                    "RETRY_AFTER_A": hint, "RETRY_AFTER_B": hint}
        # Configuration only touches the disposable fixture, never the runtime DB.
        self.docker("exec", "-i", self.mock, "python", "-c",
                    "import pathlib,sys; pathlib.Path('/tmp/qa-fault.json').write_text(sys.stdin.read())",
                    data=json.dumps(settings))

    def snapshot(self, name=None):
        result = self.docker("exec", self.controller, "python", SCRIPT, self.database_mode)
        snapshot = json.loads(result.stdout)
        self.last_snapshot = snapshot
        require(not snapshot["truncated_tables"], "database evidence truncated")
        _, wire, tcp = self.observe()
        self.wire, self.tcp = wire[len(self.baseline_http):], tcp[len(self.baseline_tcp):]
        rows = snapshot["rows"]
        if name:
            write_json(self.evidence / f"{name}.json", redact_evidence(snapshot))
        write_json(self.evidence / "mock-http.json", self.wire)
        write_json(self.evidence / "proxy-tcp.json", self.tcp)
        sample = {"at": time.time(), "http": len(self.wire),
                  "episodes": rows["provider_recovery_episodes"],
                  "incidents": rows["provider_route_throttle_incidents"],
                  "parking": rows["provider_queue_parking"]}
        with (self.evidence / "observations.jsonl").open("a") as output:
            output.write(json.dumps(sample) + "\n")
        return rows

    def wait(self, predicate, label):
        while time.monotonic() < self.deadline:
            rows = self.snapshot()
            if predicate(rows):
                write_json(self.evidence / f"{label}.json", redact_evidence(self.last_snapshot))
                return rows
            time.sleep(.5)
        raise TimeoutError(f"not verified before harness deadline: {label}")

    def bootstrap(self):
        self.start()
        binary = self.docker("exec", self.daemon, "which", "holon").stdout.strip()
        digest = self.docker("exec", self.daemon, "sha256sum", binary).stdout.split()[0]
        write_json(self.evidence / "binary-sha256.json", {"path": binary, "sha256": digest})
        for agent in ("qa", "qa-control"):
            self.api("POST", f"/api/control/agents/{agent}/create", {})
            deadline = time.monotonic() + 45
            while time.monotonic() < deadline:
                events = self.api("GET", f"/api/agents/{agent}/events?limit=200&order=asc")
                require(not events.get("has_newer"), "bootstrap events truncated")
                if any(e["type"] == "turn_terminal"
                       and e.get("payload", {}).get("kind") == "completed"
                       for e in events["events"]):
                    break
                time.sleep(.5)
            else:
                raise TimeoutError(f"{agent} healthy bootstrap did not complete")
        _, self.baseline_http, self.baseline_tcp = self.observe()
        write_json(self.evidence / "bootstrap-http.json", self.baseline_http)
        self.deadline = time.monotonic() + self.args.timeout
        write_json(self.evidence / "policy.json", {
            "case": self.case, "identity": "none", "root_origin": "authenticated Operator",
            "retry_after_format": self.header, "retry_after_seconds": self.args.retry_after_seconds,
            "dispatches_per_epoch": 3, "required_failed_probe_cycles": 2,
            "production_deadlines_unchanged": True, "layered_coverage": LAYERED_COVERAGE,
        })

    def prompt(self, agent="qa"):
        prompt = self.api("POST", f"/api/control/agents/{agent}/prompt",
                          {"text": "Return a concise final reply. No tools."})
        self.roots.append(prompt["message_id"])
        write_json(self.evidence / f"prompt-{len(self.roots)}.json", prompt)
        return prompt["message_id"]

    def healthy(self):
        self.configure(200)
        root = self.prompt()
        rows = self.wait(lambda r: root_completed(r, root), "healthy-backup")
        episode = require_budget(rows, self.roots, self.wire)[0]
        require(episode["state"] == "resolved" and episode["epoch"] == 1,
                "healthy backup did not resolve the original bounded epoch")
        require(Counter((r["side"], r["status"]) for r in self.wire)
                == Counter({("a", 429): 1, ("b", 200): 1}), "backup was not selected exactly once")
        require_completion(rows, root)
        incidents = [i for i in rows["provider_route_throttle_incidents"]
                     if i["resolved_at"] is None]
        require(len(incidents) == 1, "healthy backup was spuriously throttled")
        incident = incidents[0]
        require(incident["resolved_at"] is None and incident["consecutive_failures"] == 1,
                "unrelated healthy response reset A's incident")
        require(timestamp(incident["server_not_before"]) >= header_deadline(self.wire[0]) - .01,
                "A Retry-After deadline was shortened")
        self.configure(400)
        self.prompt("qa-control")
        rows = self.wait(lambda r: any(w["side"] == "b" and w["status"] == 400
                                      for w in self.wire), "unrelated-http400")
        # A real 400 is evidence, not an expected-failure pass for the whole gate.
        current_incidents = lambda rows: [i for i in rows["provider_route_throttle_incidents"]
                                         if i["resolved_at"] is None]
        require(current_incidents(rows) == incidents,
                "unrelated HTTP400 reset A's cooldown")
        self.configure(200)
        third = self.prompt()
        rows = self.wait(lambda r: root_completed(r, third), "healthy-after-http400")
        require_completion(rows, third)
        quiet_until = time.monotonic() + 3
        while time.monotonic() < quiet_until:
            rows = self.snapshot()
            require(time.time() < timestamp(incident["next_retry_at"]),
                    "healthy checks did not finish inside A's real cooldown")
            require(sum(w["side"] == "a" for w in self.wire) == 1,
                    "A received extra HTTP while cooling")
            require(current_incidents(rows) == incidents,
                    "unrelated success/HTTP400 cleared A's deadline or failure sequence")
            time.sleep(.5)
        require_budget(rows, self.roots, self.wire)
        require_completion(rows, root)
        require_completion(rows, third)
        return {"healthy_backup": "pass", "cooldown_no_extra_http": "pass",
                "unrelated_success_and_http400": "pass", "structured_unique_delivery": "pass"}

    def restart(self, root, before):
        require(timestamp(root_episode(before, root)["not_before"]) - time.time() > 5,
                "not enough pre-deadline time to prove restart preservation")
        volume = self.volume
        self.docker("kill", "--signal", "KILL", self.daemon)
        (self.evidence / "pre-crash-daemon.log").write_text(
            redact_evidence(self.docker("logs", self.daemon).stdout))
        self.docker("rm", "-f", self.daemon)
        self.containers.remove(self.daemon)
        self.docker("run", "--rm", "--pull=never", "--network", "none", "--user", "0",
                    "-v", f"{volume}:/var/lib/holon", "--entrypoint", "bash",
                    self.args.image, "-c", "rm -rf /var/lib/holon/run")
        self.start_daemon()
        self.wait_ready()
        require_build(self.docker("exec", self.daemon, "holon", "--version").stdout.strip(),
                      self.args.expected_revision)
        inspected = json.loads(self.docker("inspect", self.daemon).stdout)[0]
        require(inspected["Image"] == self.args.image and any(
            m.get("Name") == volume and m["Destination"] == "/var/lib/holon"
            for m in inspected["Mounts"]), "restart changed image or HOLON_HOME volume")
        write_json(self.evidence / "restart-topology.json", {
            "image": inspected["Image"], "mounts": inspected["Mounts"],
            "networks": list(inspected["NetworkSettings"]["Networks"]),
        })
        after = self.snapshot("restart-after")
        require_restart_preserved(before, after, root)
        require_failed_cycle(after, root, self.wire)
        return after

    def both(self):
        self.configure(429)
        root = self.prompt()
        def parked(rows, epoch):
            episodes = [e for e in rows["provider_recovery_episodes"]
                        if json.loads(e["binding_json"])["root_message_id"] == root]
            return bool(episodes and episodes[0]["epoch"] >= epoch
                        and episodes[0]["state"] in {"cooling", "budget_exhausted"}
                        and any(p["episode_key"] == episodes[0]["episode_key"]
                                for p in rows["provider_queue_parking"])
                        and all(r["outcome"] for r in episode_requests(rows, episodes[0])))
        before = self.wait(lambda r: parked(r, 1), "restart-before")
        require_failed_cycle(before, root, self.wire)
        previous = self.restart(root, before)
        for cycle in (1, 2):
            old = root_episode(previous, root)
            count = len(self.wire)
            while time.time() < timestamp(old["not_before"]) - .5:
                rows = self.snapshot()
                require_cooling_observation(previous, rows, root, self.wire, count, time.time())
                require(time.monotonic() < self.deadline, "deadline not reached within timeout")
                time.sleep(.5)
            rows = self.wait(lambda r: parked(r, old["epoch"] + 1), f"failed-probe-cycle-{cycle}")
            require_failed_cycle(rows, root, self.wire, previous)
            previous = rows
        return {"initial_durable_parking": "pass", "same_volume_sigkill_restart": "pass",
                "deadline_and_budget_preserved": "pass", "two_failed_probe_cycles": "pass"}

    def run(self):
        self.bootstrap()
        checks = self.healthy() if self.routes == "healthy" else self.both()
        self.snapshot("final")
        require(len(self.wire) == len(self.tcp), "settled HTTP/TCP dispatch count disagreement")
        return {"case": self.case, "status": "verified-route-throttle-core",
                "checks": checks, "http_requests": len(self.wire),
                "evidence": str(self.evidence), "layered_coverage": LAYERED_COVERAGE}


def root_completed(rows, root):
    return bool(completed_sources(rows, root))


def completed_sources(rows, root, agent="qa"):
    return [t for t in rows["turn_records"] if t["agent_id"] == agent
            and t["terminal_kind"] == "completed"
            and (t["trigger_message_id"] == root
                 or root in json.loads(t.get("payload_json", "{}")).get("input_message_ids", []))]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--python-image", required=True)
    parser.add_argument("--expected-revision", required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--case", action="append", choices=CASES)
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("--retry-after-seconds", type=int, default=30)
    args = parser.parse_args(argv)
    if args.timeout <= 0 or args.retry_after_seconds < 15:
        parser.error("timeout must be positive; Retry-After must allow at least 15 real seconds")
    if not re.fullmatch(r"[0-9a-f]{8,40}", args.expected_revision):
        parser.error("expected revision must contain 8-40 lowercase hex characters")
    args.http_timeout, args.quiet_seconds = 3, 3
    args.evidence_dir = args.evidence_dir.resolve()
    for field in ("image", "python_image"):
        setattr(args, field, command("docker", "image", "inspect", getattr(args, field),
                                    "--format", "{{.Id}}").stdout.strip())
    results = []
    def interrupt(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupt)
    for case in args.case or CASES:
        run = RouteThrottleRun(args, case)
        try:
            result = run.run()
        except (Exception, KeyboardInterrupt) as error:
            result = {"case": case, "status": "failed-or-blocked",
                      "error": str(error) or "interrupted", "evidence": str(run.evidence)}
            (run.evidence / "failure.txt").write_text(redact_evidence(traceback.format_exc()))
        finally:
            errors = run.cleanup()
        if errors:
            result.update(status="failed-or-blocked", cleanup_errors=errors)
        results.append(result)
        write_json(args.evidence_dir / "summary.json", results)
        print(json.dumps(result), flush=True)
        if result.get("error") == "interrupted":
            return 130
    return 0 if all(r["status"] == "verified-route-throttle-core" for r in results) else 1
