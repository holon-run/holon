"""W2 task-result drills on the W1 isolated topology, not a W1 verdict override."""
from __future__ import annotations

import argparse
import json
import re
import time
from datetime import datetime, timezone
from pathlib import Path

from .fallback_storm import FaultRun, SCRIPT, command, require_build
from .runner import redact_evidence, require, write_json
from .task_result_oracle import (
    IDENTITY_FIELDS, payload, recovery_ledger, recovery_root,
    require_coexisting_fairness, require_first_admission,
    require_live_task_owner, require_recovery_exhausted, require_restart_preserved,
    require_sixth_rejected, require_task_reentry,
)

MANIFEST = Path(__file__).resolve().parents[2] / "tests/e2e/docker/task-result-recovery.json"
TABLES = {
    "tasks", "task_result_settlements", "task_result_recovery_admissions",
    "messages", "briefs", "wait_conditions", "turn_records", "work_items",
    "execution_protocol_attempts", "execution_protocol_outcomes",
}
COMMAND_GATE = "/var/lib/holon/recovery-command-release"
FAIRNESS_GATE = "/var/lib/holon/recovery-fairness-command-release"
ACCEPTANCE_DIRECTORY = "/var/lib/holon/agents/qa/recovery-acceptance"


def validate_manifest(manifest):
    require(manifest["version"] == 1, "unknown recovery manifest")
    require(manifest["backoff_seconds"] == [60, 120, 240, 480]
            and manifest["recovery_limit"] == 5, "production recovery policy changed")
    cases = manifest["cases"]
    require(set().union(*map(set, manifest["suites"].values())) == set(cases),
            "recovery suite omitted a case")
    for name, case in cases.items():
        require(case["goal"] in (0, 1, 5) and case["outcome"] in ("success", "exhausted"),
                f"invalid recovery case: {name}")
        require(case["goal"] != 1 or (
            case.get("first_order") == "scanner-first" and case["outcome"] == "success"
        ), "single-root drill must be the controlled scanner-first case")
        if case.get("first_order"):
            require(case["first_order"] in {"exact-first", "scanner-first"}
                    and case["goal"] == int(case["first_order"] == "scanner-first")
                    and case["outcome"] == "success" and not case.get("sixth"),
                    "controlled order must retain its designated first canonical root")
        if case.get("sixth"):
            require(case["goal"] == 5 and case["outcome"] == "exhausted",
                    "explicit sixth source requires real five-root exhaustion")
        minimum = 1800 if case["goal"] == 5 else 180
        require(case["timeout_seconds"] >= minimum, f"insufficient case timeout: {name}")
    return manifest


def unwrap_database(database):
    require(not database["truncated_tables"], "task-result database capture truncated")
    require(TABLES <= set(database["rows"]), "task-result database evidence is incomplete")
    return database["rows"]


class RecoveryRun(FaultRun):
    database_mode = "task-result-database"

    def __init__(self, args, case, spec):
        if spec.get("alive_owner"):
            args = argparse.Namespace(**{**vars(args), "http_timeout": 90})
        super().__init__(args, case)
        self.spec = spec
        self.captured = None
        self.releases = {}
        self.ignored_requests = set()
        self.joins = []
        self.restarted = False
        self.setup_identity = None
        self.alive_activation = None
        self.alive_owner_verified = False
        self.fairness_started = False
        self.fairness_setup = None
        self.fairness_captured = None
        self.acceptance_request = self.prefix.rsplit("-", 1)[-1]
        self.sixth_requested = False
        self.order_captured = False
        self.order_verified = False

    def acceptance_event(self, name):
        path = f"/evidence-home/agents/qa/recovery-acceptance/{self.acceptance_request}.{name}.json"
        result = self.docker(
            "exec", self.controller, "python", "-c",
            "import json,pathlib,sys; p=pathlib.Path(sys.argv[1]);"
            "print(p.read_text() if p.exists() else 'null')", path,
        )
        value = json.loads(result.stdout)
        if value is not None:
            write_json(self.evidence / f"acceptance-{name}.json", value)
        return value

    def acceptance_control(self, record, *, inject_sixth=False,
                           hold_scanner=False, hold_task_result=False, prearm=False):
        value = {
            "protocol": 1, "request_id": self.acceptance_request,
            "agent_id": record["agent_id"], "task_id": record["task_id"],
            "work_item_id": record["work_item_id"], "hold_scanner": hold_scanner,
            "hold_task_result": hold_task_result,
            "inject_sixth": inject_sixth,
            "fence": None if prearm else {
                "result_message_id": record["message_id"],
                **{key: record[key] for key in (
                    "result_identity", "rejoin_generation", "parent_turn_id", "recovery_epoch",
                )},
            },
        }
        self.docker(
            "exec", "-i", self.daemon, "bash", "-c",
            'umask 077; mkdir -p "$1"; cat > "$1/control.pending"; '
            'mv "$1/control.pending" "$1/control.json"', "recovery-acceptance",
            ACCEPTANCE_DIRECTORY, data=json.dumps(value),
        )

    def observe_order(self, snapshot, barriers):
        order = self.spec.get("first_order")
        if not order or self.order_verified:
            return True
        held_name = "task-result-held" if order == "scanner-first" else "scanner-held"
        held = self.acceptance_event(held_name)
        scanner = self.acceptance_event("scanner-enqueued") if order == "scanner-first" else None
        if not self.order_captured:
            if held is None or not barriers or (order == "scanner-first" and scanner is None):
                return False
            if self.record(snapshot)["state"] != "caller_admitted":
                return False
            require_first_admission(
                snapshot, captured=self.captured, order=order, held=held, scanner=scanner,
                released=None, request_id=self.acceptance_request,
                expected_build=json.loads((self.evidence / "build.json").read_text())["binary_version"],
            )
            write_json(self.evidence / "order-before.json", redact_evidence(snapshot))
            self.acceptance_control(self.record(snapshot))
            self.order_captured = True
        released = self.acceptance_event(
            "task-result-released" if order == "scanner-first" else "scanner-released",
        )
        if released is None:
            return False
        require_first_admission(
            json.loads((self.evidence / "order-before.json").read_text()),
            captured=self.captured, order=order, held=held, scanner=scanner,
            released=released, request_id=self.acceptance_request,
            expected_build=json.loads((self.evidence / "build.json").read_text())["binary_version"],
        )
        self.order_verified = True
        return True

    def observe_sixth(self, snapshot):
        record = require_recovery_exhausted(snapshot, captured=self.captured)
        if not self.sixth_requested:
            write_json(self.evidence / "sixth-before.json", redact_evidence(snapshot))
            self.acceptance_control(record, inject_sixth=True)
            self.sixth_requested = True
        enqueued = self.acceptance_event("sixth-enqueued")
        rejected = self.acceptance_event("budget-rejected")
        if enqueued is None or rejected is None:
            return False
        if not any(row["message_id"] == enqueued.get("source_message_id")
                   and row["status"] in {"processed", "dropped"}
                   for row in snapshot["queue_entries"]):
            # The read-only snapshot can precede the ACK's final queue commit.
            return False
        require_sixth_rejected(
            json.loads((self.evidence / "sixth-before.json").read_text()), snapshot,
            captured=self.captured, enqueued=enqueued, rejected=rejected,
            request_id=self.acceptance_request,
            expected_build=json.loads((self.evidence / "build.json").read_text())["binary_version"],
        )
        return True

    def configure(self, container, path, value):
        # Atomic replacement affects only provider fixture control, never the DB.
        self.docker("exec", "-i", container, "python", "-c",
                    "import pathlib,sys; p=pathlib.Path(sys.argv[1]);"
                    "q=p.with_suffix('.new'); q.write_text(sys.stdin.read()); q.replace(p)",
                    path, data=json.dumps(value))

    def snapshot(self, label):
        result = self.docker("exec", self.controller, "python", SCRIPT, self.database_mode)
        database = json.loads(result.stdout)
        write_json(self.evidence / f"{label}-db.json", redact_evidence(database))
        return unwrap_database(database)

    def barriers(self):
        records = [
            json.loads(line) for line in self.docker("logs", self.mock).stdout.splitlines()
            if line.startswith("{")
        ]
        rows = [row for row in records if row.get("layer") == "recovery_barrier"]
        require(
            all(row["request_id"] in self.ignored_requests for row in rows
                if row["state"] == "expired"),
            "provider barrier expired before an explicit controller release",
        )
        finished = {row["request_id"] for row in rows if row["state"] != "waiting"}
        return [row for row in rows if row["state"] == "waiting"
                and row["request_id"] not in finished | self.ignored_requests]

    def record(self, snapshot):
        records = [
            row for row in snapshot["task_result_settlements"] if row["agent_id"] == "qa"
        ]
        if self.spec.get("fairness") and self.captured is not None:
            records = [row for row in records
                       if row["result_identity"] == self.captured["result_identity"]]
        require(len(records) <= 1, f"fixture created more than one obligation: {records}")
        if not records:
            return None
        record = records[0]
        if self.captured is None:
            require(record["work_item_id"], "fixture task has no captured WorkItem owner")
            if self.setup_identity is not None:
                require(
                    all(record[key] == value for key, value in self.setup_identity.items()),
                    "terminal task result migrated away from the pre-terminal exact waiter",
                )
            self.captured = {key: record[key] for key in IDENTITY_FIELDS}
            write_json(self.evidence / "captured-identity.json", self.captured)
        require(all(record[key] == self.captured[key] for key in IDENTITY_FIELDS),
                "task identity/owner/generation drifted during recovery")
        recovery_ledger(snapshot, record)
        return record

    def start_fairness(self, snapshot):
        require_recovery_exhausted(snapshot, captured=self.captured)
        write_json(self.evidence / "fairness-before.json", redact_evidence(snapshot))
        self.configure(self.mock, "/tmp/qa-recovery.json", {
            "releases": self.releases, "fairness_owner": self.captured["work_item_id"],
        })
        self.api("POST", "/api/control/agents/qa/prompt", {
            "work_item_id": self.captured["work_item_id"],
            "text": (
                f"SCHEDULER-TASK-FAIRNESS under {self.captured['work_item_id']}: "
                "run one new promoted command, WaitFor its exact result with delivery=silent, "
                "then WaitFor operator_input with delivery=final and report "
                "SCHEDULER-TASK-FAIRNESS-COMPLETE. Keep this shared WorkItem open; "
                "do not rearm or consume the exhausted task."
            ),
        })
        self.fairness_started = True

    def observe_fairness(self, snapshot):
        if self.fairness_setup is None:
            waits = [
                row for row in snapshot["wait_conditions"]
                if row["agent_id"] == "qa" and row["status"] == "active"
                and row["kind"] == "task" and row["subject_ref"] != self.captured["task_id"]
            ]
            if not waits:
                return False
            require(len(waits) == 1
                    and waits[0]["work_item_id"] == self.captured["work_item_id"],
                    "fresh fairness task migrated away from the shared owner")
            tasks = [payload(row) for row in snapshot["tasks"]
                     if row["task_id"] == waits[0]["subject_ref"]]
            require(len(tasks) == 1
                    and tasks[0].get("work_item_id") == self.captured["work_item_id"]
                    and tasks[0].get("status") in {"queued", "running"}
                    and tasks[0]["detail"]["parent_turn_id"] == waits[0]["last_turn_id"],
                    "fresh fairness waiter is not bound to a real owned task")
            self.fairness_setup = {
                "agent_id": "qa", "task_id": tasks[0]["id"],
                "work_item_id": waits[0]["work_item_id"],
                "parent_turn_id": waits[0]["last_turn_id"],
                "rejoin_generation": tasks[0]["detail"]["rejoin_generation"],
            }
            write_json(self.evidence / "fairness-pre-terminal.json", self.fairness_setup)
            self.docker("exec", self.daemon, "bash", "-c", f"touch {FAIRNESS_GATE}")
        records = [
            row for row in snapshot["task_result_settlements"]
            if row["agent_id"] == "qa" and row["task_id"] == self.fairness_setup["task_id"]
        ]
        require(len(records) <= 1, "duplicate fresh fairness obligation")
        if not records:
            return False
        record = records[0]
        require(all(record[key] == value for key, value in self.fairness_setup.items()),
                "fresh fairness result changed its pre-terminal owner or generation")
        self.fairness_captured = {key: record[key] for key in IDENTITY_FIELDS}
        write_json(self.evidence / "fairness-captured.json", self.fairness_captured)
        attempts = [payload(row) for row in snapshot["execution_protocol_attempts"]
                    if row["agent_id"] == "qa"
                    and row["attempt_id"] == record.get("activation_id")]
        if (len(attempts) != 1 or not any(
            row["agent_id"] == "qa" and row["kind"] == "result"
            and row.get("turn_id") == attempts[0].get("turn_id")
            for row in snapshot["briefs"]
        )):
            return False
        require_coexisting_fairness(
            json.loads((self.evidence / "fairness-before.json").read_text()), snapshot,
            captured=self.captured, fresh=self.fairness_captured,
        )
        return True

    def restart(self, before, *, backoff):
        write_json(self.evidence / "restart-before.json", redact_evidence(before))
        old_volume = self.volume
        self.ignored_requests.update(row["request_id"] for row in self.barriers())
        self.docker("kill", "--signal", "KILL", self.daemon)
        (self.evidence / "pre-crash-daemon.log").write_text(
            redact_evidence(self.docker("logs", self.daemon).stdout))
        self.docker("rm", "-f", self.daemon)
        self.containers.remove(self.daemon)
        self.docker(
            "run", "--rm", "--pull=never", "--network", "none", "--user", "0",
            "-v", f"{self.volume}:/var/lib/holon", "--entrypoint", "bash",
            self.args.image, "-c", "rm -rf /var/lib/holon/run",
        )
        self.start_daemon()
        self.wait_ready()
        require(self.volume == old_volume, "restart did not reuse the same HOLON_HOME volume")
        version = self.docker("exec", self.daemon, "holon", "--version").stdout.strip()
        require_build(version, self.args.expected_revision)
        inspected = json.loads(self.docker("inspect", self.daemon).stdout)[0]
        require(any(mount.get("Name") == old_volume
                    and mount["Destination"] == "/var/lib/holon"
                    for mount in inspected["Mounts"]), "restarted daemon mounted another volume")
        require(inspected["Image"] == self.args.image, "restarted daemon changed candidate image")
        write_json(self.evidence / "restart-topology.json", {
            "image": inspected["Image"], "mounts": inspected["Mounts"],
            "networks": list(inspected["NetworkSettings"]["Networks"]),
        })
        write_json(self.evidence / "restart-build.json", {
            "volume": self.volume, "image": self.args.image, "binary_version": version,
        })
        after = self.snapshot("restart-after")
        require_restart_preserved(before, after, captured=self.captured, backoff=backoff)
        self.restarted = True

    def run(self):
        self.start()
        version = json.loads((self.evidence / "build.json").read_text())["binary_version"]
        controlled = bool(self.spec.get("sixth") or self.spec.get("first_order"))
        require(("[recovery-acceptance-test]" in version) == controlled,
                "ordinary and compile-time acceptance binaries must not be substituted")
        write_json(self.evidence / "policy.json", {
            "workload": "W2", "recovery_limit": 5,
            "backoff_seconds": [60, 120, 240, 480],
            "production_deadlines_unchanged": True, "case": self.case,
            "case_spec": self.spec, "timeout_seconds": self.args.timeout,
            "barrier": "canonical-source" if self.spec.get("first_order") else "provider-only",
            "scanner_order_controlled": bool(self.spec.get("first_order")),
        })
        self.api("POST", "/api/control/agents/qa/create", {})
        deadline = time.monotonic() + self.args.timeout
        while time.monotonic() < deadline:
            events = self.api("GET", "/api/agents/qa/events?limit=200&order=asc")
            require(not events.get("has_newer"), "bootstrap events truncated")
            if any(row["type"] == "turn_terminal"
                   and row.get("payload", {}).get("kind") == "completed"
                   for row in events["events"]):
                break
            time.sleep(.5)
        else:
            raise TimeoutError("healthy bootstrap did not finish")
        self.configure(self.mock, "/tmp/qa-fault.json", {"TASK_RESULT_RECOVERY": True})
        if self.spec.get("alive_owner"):
            self.configure(self.proxy, "/tmp/qa-fault.json", {"UPSTREAM_TIMEOUT": 90})
        marker = self.prefix.rsplit("-", 1)[-1]
        self.api("POST", "/api/control/agents/qa/prompt", {"text": (
            f"Create one ready WorkItem SCHEDULER-TASK-WAIT-{marker}. "
            f"Run one promoted command producing SCHEDULER-TASK-RESULT-{marker}, "
            "WaitFor that exact task with delivery=silent. On its result, complete "
            f"that same WorkItem with SCHEDULER-TASK-WAIT-COMPLETE-{marker}."
        )})
        # Command cannot finish until its real exact waiter and owner exist.
        while time.monotonic() < deadline:
            snapshot = self.snapshot("setup")
            waits = [row for row in snapshot["wait_conditions"]
                     if row["agent_id"] == "qa" and row["status"] == "active"
                     and row["kind"] == "task" and row["subject_ref"]]
            if waits:
                require(len(waits) == 1 and waits[0]["work_item_id"],
                        "setup must have one owned exact task wait")
                tasks = [payload(row) for row in snapshot["tasks"]
                         if row["task_id"] == waits[0]["subject_ref"]]
                require(len(tasks) == 1
                        and tasks[0].get("work_item_id") == waits[0]["work_item_id"],
                        "setup waiter is not bound to the real promoted task")
                self.setup_identity = {
                    "agent_id": "qa", "task_id": tasks[0]["id"],
                    "work_item_id": waits[0]["work_item_id"],
                    "parent_turn_id": waits[0]["last_turn_id"],
                    "rejoin_generation": tasks[0]["detail"]["rejoin_generation"],
                }
                write_json(self.evidence / "pre-terminal-identity.json", self.setup_identity)
                break
            time.sleep(.5)
        else:
            raise TimeoutError("real owned task wait was not established")
        if self.spec.get("fallback"):
            self.configure(self.proxy, "/tmp/qa-fault.json", {"FAULT_A": "rst"})
        if self.spec.get("first_order"):
            self.acceptance_control(
                self.setup_identity, prearm=True,
                hold_scanner=self.spec["first_order"] == "exact-first",
                hold_task_result=self.spec["first_order"] == "scanner-first",
            )
        self.docker("exec", self.daemon, "bash", "-c", f"touch {COMMAND_GATE}")
        quiet_since = None
        while time.monotonic() < deadline:
            snapshot = self.snapshot("observation")
            record = self.record(snapshot)
            if record is None:
                time.sleep(.5)
                continue
            barriers = self.barriers()
            if not self.observe_order(snapshot, barriers):
                time.sleep(.5)
                continue
            if self.spec.get("alive_owner") and not self.alive_owner_verified and barriers:
                first = self.alive_activation is None
                self.alive_activation = require_live_task_owner(
                    snapshot, captured=self.captured, activation_id=self.alive_activation,
                )
                if first:
                    write_json(self.evidence / "alive-owner-before.json", redact_evidence(snapshot))
                due = datetime.fromisoformat(record["next_recheck_at"].replace("Z", "+00:00"))
                observed_at = datetime.now(timezone.utc)
                if (observed_at - due).total_seconds() < 5:
                    time.sleep(.5)
                    continue
                require_live_task_owner(
                    snapshot, captured=self.captured, activation_id=self.alive_activation,
                    observed_at=observed_at,
                )
                require(len(barriers) == 1, "more than one provider request owns the live result")
                write_json(self.evidence / "alive-owner-after.json", redact_evidence(snapshot))
                write_json(self.evidence / "alive-owner-observed.json", {
                    "at": observed_at.isoformat(), "activation_id": self.alive_activation,
                    "provider_request_id": barriers[0]["request_id"],
                })
                self.alive_owner_verified = True
            restart = self.spec.get("restart")
            if restart and not self.restarted:
                due = record.get("next_recheck_at")
                in_backoff = (
                    restart == "backoff" and record["recovery_attempts"] == 1
                    and not barriers and due
                    and datetime.fromisoformat(due.replace("Z", "+00:00"))
                    > datetime.now(timezone.utc)
                )
                if in_backoff or (restart == "fifth"
                                  and record["recovery_attempts"] == 5 and barriers):
                    self.restart(snapshot, backoff=restart == "backoff")
                    continue
            for barrier in barriers:
                request_id = barrier["request_id"]
                if request_id in self.releases:
                    continue
                success = (self.spec["outcome"] == "success"
                           and record["recovery_attempts"] == self.spec["goal"])
                self.releases[request_id] = "success" if success else "fail"
                self.joins.append({
                    **barrier, "action": self.releases[request_id],
                    "activation_id": record["activation_id"],
                    "recovery_attempts": record["recovery_attempts"],
                    "root_attempt_id": recovery_root(
                        snapshot, "qa", record["activation_id"]),
                    "result_identity": record["result_identity"],
                    "recovery_epoch": record["recovery_epoch"],
                })
                self.configure(self.mock, "/tmp/qa-recovery.json",
                               {"releases": self.releases})
                write_json(self.evidence / "barrier-joins.json", self.joins)
            completed = any(
                row["work_item_id"] == record["work_item_id"] and row["state"] == "completed"
                for row in snapshot["work_items"]
            )
            if (self.spec["outcome"] == "success" and record["state"] == "settled"
                    and completed):
                require_task_reentry(
                    snapshot, agent_id="qa", task_id=record["task_id"],
                    parent_turn_id=record["parent_turn_id"], captured=self.captured,
                )
                break
            if (self.spec["outcome"] == "exhausted"
                    and record.get("recovery_diagnostic_at") and not barriers):
                require_recovery_exhausted(snapshot, captured=self.captured)
                if self.spec.get("sixth") and not self.observe_sixth(snapshot):
                    time.sleep(.5)
                    continue
                if self.spec.get("fairness"):
                    if not self.fairness_started:
                        self.start_fairness(snapshot)
                    if not self.observe_fairness(snapshot):
                        time.sleep(.5)
                        continue
                quiet_since = quiet_since or time.monotonic()
                if time.monotonic() - quiet_since >= self.args.quiet_seconds:
                    break
            time.sleep(.5)
        else:
            raise TimeoutError("task-result recovery did not satisfy its strict oracle")
        return {"case": self.case, "status": "awaiting-frozen-oracle",
                "evidence": str(self.evidence)}

    def verify_frozen(self):
        snapshot = unwrap_database(json.loads((self.evidence / "database.json").read_text()))
        record = self.record(snapshot)
        require(record is not None, "final task-result settlement is missing")
        rows = recovery_ledger(
            snapshot, record, expected_count=self.spec["goal"],
            real_backoff=self.spec["goal"] == 5,
        )
        for index, row in enumerate(rows):
            expected = ("success" if self.spec["outcome"] == "success"
                        and index == len(rows) - 1 else "fail")
            require(any(join["root_attempt_id"] == row["root_attempt_id"]
                        and join["action"] == expected for join in self.joins),
                    f"charged recovery root lacks its actual provider outcome: {row}")
        require(
            all(join["action"] != "success"
                or join["recovery_attempts"] == self.spec["goal"] for join in self.joins),
            "provider released success before the designated recovery root",
        )
        if self.spec["outcome"] == "exhausted":
            require_recovery_exhausted(snapshot, captured=self.captured)
            if self.spec.get("sixth"):
                require(self.sixth_requested, "explicit sixth-source checkpoint was skipped")
                require_sixth_rejected(
                    json.loads((self.evidence / "sixth-before.json").read_text()), snapshot,
                    captured=self.captured,
                    enqueued=json.loads((self.evidence / "acceptance-sixth-enqueued.json").read_text()),
                    rejected=json.loads((self.evidence / "acceptance-budget-rejected.json").read_text()),
                    request_id=self.acceptance_request,
                    expected_build=json.loads((self.evidence / "build.json").read_text())["binary_version"],
                )
            if self.spec.get("fairness"):
                fresh = json.loads((self.evidence / "fairness-captured.json").read_text())
                setup = json.loads((self.evidence / "fairness-pre-terminal.json").read_text())
                require(all(fresh[key] == value for key, value in setup.items()),
                        "frozen fairness result lost its pre-terminal identity")
                require_coexisting_fairness(
                    json.loads((self.evidence / "fairness-before.json").read_text()),
                    snapshot, captured=self.captured, fresh=fresh,
                )
        else:
            require_task_reentry(
                snapshot, agent_id="qa", task_id=record["task_id"],
                parent_turn_id=record["parent_turn_id"], captured=self.captured,
            )
            if rows:
                require(recovery_root(snapshot, "qa", record["activation_id"])
                        == rows[-1]["root_attempt_id"], "success did not consume the fifth root")
            else:
                root = recovery_root(snapshot, "qa", record["activation_id"])
                attempts = [
                    payload(row) for row in snapshot["execution_protocol_attempts"]
                    if row["agent_id"] == "qa" and row["attempt_id"] == root
                ]
                require(len(attempts) == 1
                        and attempts[0]["source"]["identity"]["kind"] == "task_result",
                        "fast exact baseline was not admitted from its actual TaskResult")
        order = self.spec.get("first_order")
        if order:
            require(self.order_verified, "controlled first-admission checkpoint was skipped")
            require_first_admission(
                json.loads((self.evidence / "order-before.json").read_text()),
                captured=self.captured, order=order,
                held=json.loads((self.evidence / (
                    "acceptance-task-result-held.json" if order == "scanner-first"
                    else "acceptance-scanner-held.json")).read_text()),
                scanner=json.loads((self.evidence / "acceptance-scanner-enqueued.json").read_text())
                if order == "scanner-first" else None,
                released=json.loads((self.evidence / (
                    "acceptance-task-result-released.json" if order == "scanner-first"
                    else "acceptance-scanner-released.json")).read_text()),
                request_id=self.acceptance_request,
                expected_build=json.loads((self.evidence / "build.json").read_text())["binary_version"],
                after=snapshot,
            )
        require(not self.spec.get("restart") or self.restarted, "restart checkpoint was skipped")
        if self.spec.get("alive_owner"):
            observed = json.loads((self.evidence / "alive-owner-observed.json").read_text())
            for name in ("before", "after"):
                active = json.loads((self.evidence / f"alive-owner-{name}.json").read_text())
                require_live_task_owner(
                    active, captured=self.captured, activation_id=observed["activation_id"],
                    observed_at=datetime.fromisoformat(observed["at"].replace("Z", "+00:00"))
                    if name == "after" else None,
                )
        if self.spec.get("fallback"):
            proxy = (self.evidence / f"{self.proxy}.log").read_text()
            require('"fault": "rst"' in proxy and any(row["side"] == "b" for row in self.joins),
                    "fallback route was not actually exercised")
        return {"case": self.case, "status": "verified-task-result-isolated",
                "recovery_roots": len(rows), "evidence": str(self.evidence),
                "scanner_order_controlled": bool(order),
                "sixth_source_rejected": bool(self.spec.get("sixth"))}


def main(argv=None):
    manifest = validate_manifest(json.loads(MANIFEST.read_text()))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--validate-manifest", action="store_true")
    parser.add_argument("--image")
    parser.add_argument("--python-image")
    parser.add_argument("--expected-revision")
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument("--suite", choices=manifest["suites"], default="fast")
    parser.add_argument("--case", action="append", choices=manifest["cases"])
    parser.add_argument("--timeout", type=int)
    parser.add_argument("--http-timeout", type=int, default=30)
    parser.add_argument("--quiet-seconds", type=int, default=65)
    args = parser.parse_args(argv)
    if args.validate_manifest:
        print(f"valid task-result recovery manifest: {MANIFEST}")
        return 0
    if not all((args.image, args.python_image, args.expected_revision, args.evidence_dir)):
        parser.error("--image, --python-image, --expected-revision and --evidence-dir are required")
    cases = args.case or manifest["suites"][args.suite]
    minimum = max(manifest["cases"][case]["timeout_seconds"] for case in cases)
    if args.timeout is not None and args.timeout < minimum:
        parser.error(f"--timeout must be at least {minimum} for selected cases")
    if args.http_timeout < 25 or args.quiet_seconds < 65:
        parser.error("provider barrier needs http-timeout >=25; pause observation needs >=65")
    if not re.fullmatch(r"[0-9a-f]{8,40}", args.expected_revision):
        parser.error("expected revision must contain 8-40 lowercase hex characters")
    args.timeout = args.timeout or minimum
    args.evidence_dir = args.evidence_dir.resolve()
    for field in ("image", "python_image"):
        result = command("docker", "image", "inspect", getattr(args, field),
                         "--format", "{{.Id}}")
        setattr(args, field, result.stdout.strip())
    results = []
    for case in cases:
        run = RecoveryRun(args, case, manifest["cases"][case])
        result = None
        interrupted = False
        try:
            result = run.run()
        except KeyboardInterrupt:
            interrupted = True
            result = {"case": case, "status": "failed-or-blocked", "error": "interrupted"}
        except Exception as error:
            result = {"case": case, "status": "failed-or-blocked", "error": str(error)}
        finally:
            errors = run.cleanup()
        if result["status"] == "awaiting-frozen-oracle" and not errors:
            try:
                result = run.verify_frozen()
            except Exception as error:
                result = {"case": case, "status": "failed-or-blocked", "error": str(error)}
        result["evidence"] = str(run.evidence)
        if errors:
            result.update(status="failed-or-blocked", cleanup_errors=errors)
        results.append(result)
        write_json(args.evidence_dir / "summary.json", results)
        print(json.dumps(result), flush=True)
        if interrupted:
            return 130
    return 0 if all(row["status"] == "verified-task-result-isolated" for row in results) else 1
