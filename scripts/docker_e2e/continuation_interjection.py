"""Isolated-daemon acceptance for #3753 and #3754.

T3753: A produces a tool call, A cools before the continuation, independent B
must continue the same turn with the original tool result and a single delivery.

I3754: a provider-parked Interrupted operator source must be retained, a fresh
queued input must still interject, and the timer turn must settle durably.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import time
from datetime import datetime, timezone
from pathlib import Path

from .fallback_storm import FaultRun, SCRIPT
from .runner import redact_evidence, require, write_json

CASES = ("T3753", "I3754")
TOOL_GATE = "qa-3753-gate"
INTERJECT_GATE = "qa-3754-gate"
PARK_GATE = "qa-3754-park-gate"


def _rows(snapshot, name):
    return snapshot.get("rows", {}).get(name, [])


def _completed(events):
    return [event for event in events if event["type"] == "turn_terminal"
            and event.get("payload", {}).get("kind") == "completed"]


def _result_briefs(events):
    return [event for event in events if event["type"] == "brief_created"
            and event.get("payload", {}).get("kind") == "result"]


def _queue_rows(snapshot):
    return _rows(snapshot, "queue_entries_current") or _rows(snapshot, "queue_entries")


def _entry_with(snapshot, marker):
    message_ids = {
        row.get("message_id") for row in _rows(snapshot, "messages")
        if marker in (row.get("payload_json") or "") or marker in (row.get("preview") or "")
    }
    for row in _queue_rows(snapshot):
        if row.get("message_id") in message_ids:
            return row
    return None


def _complete_snapshot(snapshot):
    require(not snapshot.get("truncated_tables"), "database evidence is truncated")


def _source_rows(snapshot, table, message_id):
    return [row for row in _rows(snapshot, table) if row.get("message_id") == message_id]


def _turns(snapshot):
    return [json.loads(row["payload_json"]) for row in _rows(snapshot, "turn_records")]


def _source_has_reasoning(snapshot, message_id):
    turns = {turn["turn_id"] for turn in _turns(snapshot)
             if message_id in turn.get("input_message_ids", [])}
    for row in _rows(snapshot, "transcript_entries"):
        entry = json.loads(row["payload_json"])
        data = entry.get("data", {})
        if entry.get("kind") == "assistant_round" and data.get("turn_id") in turns:
            if {"type": "reasoning_text", "text": "QA_PROVIDER_OWNED_REASONING"} in data.get("blocks", []):
                return True
    return False


def _instant(value):
    timestamp = str(value).replace("Z", "+00:00")
    # Python <3.11 accepts only 3/6 fractional digits; Rust emits nanoseconds.
    timestamp = re.sub(r"\.(\d+)(?=[+-]|$)",
                       lambda match: "." + match.group(1)[:6].ljust(6, "0"),
                       timestamp)
    return datetime.fromisoformat(timestamp)


def evaluate_tool_continuation(events, http, snapshot):
    _complete_snapshot(snapshot)
    completed = _completed(events)
    briefs = _result_briefs(events)
    require(len(completed) == 1, "expected exactly one completed turn")
    require(completed[0]["payload"].get("last_assistant_message") == "QA_TOOL_CONTINUED",
            "turn did not deliver the independent-B continuation marker")
    require(len(briefs) == 1, "expected exactly one user-facing delivery")
    require(any(row.get("side") == "a" and row.get("response") == "tool_call"
                for row in http), "A did not produce the first tool call")
    cooled_continuations = [row for row in http
                            if row.get("side") == "a" and row.get("tool_output")]
    require(not cooled_continuations,
            "cooled A received a tool continuation after the local guard")
    continuations = [row for row in http if row.get("side") == "b" and row.get("tool_output")]
    require(len(continuations) == 1,
            "independent B must receive exactly one continuation with tool history")
    reselected = [event for event in events
                  if event["type"] == "provider_candidate_reselected"]
    require(len(reselected) == 1, "expected one candidate-reselection audit event")
    payload = reselected[0].get("payload", {})
    require(payload.get("dispatch_occurred") is False,
            "reselection must be logged as a local non-dispatch")
    require(payload.get("predicate") == "scope_cooling",
            "reselection predicate must be scope_cooling")
    require("qa-b" in json.dumps(payload.get("model_ref")),
            "reselection must name the independent B candidate")
    executions = [row for row in _rows(snapshot, "tool_executions")
                  if TOOL_GATE in json.dumps(row)]
    require(len(executions) == 1, "the tool must execute exactly once")
    require(json.loads(executions[0]["payload_json"]).get("status") == "success",
            "the single tool execution must have succeeded")
    return "verified-t3753-isolated"


def evaluate_interjection(events, http, snapshot, before, boundary):
    for checkpoint in (snapshot, before, boundary):
        _complete_snapshot(checkpoint)
    completed = _completed(events)
    aborted = [event for event in events if event["type"] == "turn_terminal"
               and event.get("payload", {}).get("kind") == "aborted"]
    require(not aborted, "source and timer turns must not abort")
    briefs = _result_briefs(events)
    require(len(briefs) == 1, "expected exactly one user-facing delivery")
    require(len(completed) == 1
            and completed[0]["payload"].get("last_assistant_message") == "QA_TIMER_DONE",
            "timer turn did not complete with the expected delivery marker")
    admitted = [event for event in events
                if event["type"] == "operator_interjection_admitted"]
    require(len(admitted) == 1, "expected one durable operator interjection admission")
    parked = _entry_with(snapshot, "QA_PARK")
    fresh = _entry_with(snapshot, "QA_FRESH")
    require(parked is not None, "the interrupted source must stay in memory")
    require(parked["status"] == "interrupted",
            "the parked source status must stay interrupted, not be interjected")
    require(fresh is not None and fresh["status"] == "interjected",
            "fresh queued input must be the one interjected")
    require(parked["message_id"] != fresh["message_id"],
            "distinct sources are required for this case")
    source_id = parked["message_id"]
    timer_turn_id = completed[0]["payload"].get("turn_id")
    timer_turns = [turn for turn in _turns(snapshot)
                   if timer_turn_id and turn.get("turn_id") == timer_turn_id]
    require(len(timer_turns) == 1, "missing unique completed timer turn record")
    timer_inputs = timer_turns[0].get("input_message_ids", [])
    require(source_id not in timer_inputs, "timer must not consume the parked source")
    require(fresh["message_id"] in timer_inputs, "timer must consume the fresh input")
    initial = _entry_with(before, "QA_PARK")
    require(initial is not None and initial["status"] == "interrupted",
            "source must really park before the timer")
    parking = _source_rows(before, "provider_queue_parking", source_id)
    require(len(parking) == 1, "source must have one durable parking condition")
    assignment = _source_rows(before, "conversation_input_assignments", source_id)
    require(len(assignment) == 1,
            "missing the original durable input owner")
    require(any(turn.get("turn_id") == assignment[0].get("turn_id")
                and turn.get("agent_id") == assignment[0].get("agent_id")
                and source_id in turn.get("input_message_ids", [])
                for turn in _turns(before)),
            "input assignment must identify the original source turn")
    require(_instant(completed[0]["ts"]) < _instant(parking[0]["not_before"]),
            "timer must complete before the original parking deadline")
    require(any(event["type"] == "queue_entry_settled"
                and event.get("payload", {}).get("message_id") == source_id
                and event["payload"].get("status") == "interrupted"
                and event["payload"].get("reason") == "provider_dispatch_parked"
                for event in events), "missing real provider_dispatch_parked settlement")
    require(admitted[0].get("payload", {}).get("message_id") == fresh["message_id"]
            and admitted[0]["payload"].get("boundary") == "before_tool_execution",
            "fresh input must be admitted once at BeforeToolExecution")
    boundary_source = _entry_with(boundary, "QA_PARK")
    boundary_fresh = _entry_with(boundary, "QA_FRESH")
    require(boundary_source is not None and boundary_source["status"] == "interrupted"
            and boundary_fresh is not None and boundary_fresh["status"] == "queued",
            "both retained and fresh canonical inputs must exist before tool execution")
    for checkpoint in (boundary, snapshot):
        require(_source_rows(checkpoint, "provider_queue_parking", source_id) == parking,
                "timer must preserve the source parking owner and deadline")
        require(_source_rows(checkpoint, "conversation_input_assignments", source_id)
                == assignment,
                "timer must not change the original input consumption owner")
        require(_source_rows(checkpoint, "messages", source_id)
                == _source_rows(before, "messages", source_id),
                "timer must preserve the original canonical message")
    require(any(row.get("marker") == "QA_TIMER" and row.get("response") == "tool_call"
                and row.get("response_held") is True
                and row.get("side") == "b"
                for row in http), "healthy B must hold the timer response before tool execution")
    require(any(row.get("marker") == "QA_PARK" and row.get("side") == "a"
                and row.get("provider_owned_reasoning") == {
                    "type": "reasoning", "id": "rs_non_persisted",
                    "content": [{"type": "reasoning_text",
                                 "text": "QA_PROVIDER_OWNED_REASONING"}],
                    "encrypted_content": "opaque-reasoning",
                } for row in http), "missing original provider-owned opaque reasoning round")
    require(_source_has_reasoning(before, source_id),
            "source assistant_blocks must contain parsed provider-owned reasoning_text")
    return "verified-i3754-predeadline"


def evaluate_recovery(events, snapshot, before):
    _complete_snapshot(snapshot)
    source = _entry_with(before, "QA_PARK")
    require(source is not None, "missing original source")
    source_id = source["message_id"]
    final = _entry_with(snapshot, "QA_PARK")
    require(final is not None and final["status"] == "processed",
            "original source must eventually become Processed")
    processed = [event for event in events if event["type"] == "queue_entry_settled"
                 and event.get("payload", {}).get("message_id") == source_id
                 and event["payload"].get("status") == "processed"]
    require(len(processed) == 1, "original source must be Processed exactly once")
    replay = [event for event in events if event["type"] == "turn_replay_started"
              and event.get("payload", {}).get("message_id") == source_id
              and event["payload"].get("reason") == "interrupted_queue_claim_reentry"]
    require(len(replay) == 1, "missing unique interrupted_queue_claim_reentry")
    require(not any(event["type"] == "turn_terminal"
                    and event.get("payload", {}).get("kind") == "aborted"
                    for event in events), "source, timer and recovery turns must not abort")
    claim = [event for event in events if event["type"] == "queue_entry_claimed"
             and event.get("payload", {}).get("message_id") == source_id]
    require(len(claim) == 2, "expected the initial claim and one scheduler reclaim")
    parking = _source_rows(before, "provider_queue_parking", source_id)
    require(len(parking) == 1, "missing original parking deadline")
    require(_instant(claim[-1]["ts"]) >= _instant(parking[0]["not_before"]),
            "scheduler reclaimed the source before its original deadline")
    require(not _source_rows(snapshot, "provider_queue_parking", source_id),
            "processed source must no longer be parked")
    payload = replay[0]["payload"]
    original = [turn for turn in _turns(before)
                if turn.get("turn_id") == payload.get("source_turn_id")]
    recovered = [turn for turn in _turns(snapshot)
                 if turn.get("turn_id") == payload.get("replay_turn_id")]
    require(len(original) == len(recovered) == 1
            and original[0].get("owner") is not None
            and original[0]["owner"] == recovered[0].get("owner"),
            "reentry must preserve the original turn owner")
    require(recovered[0].get("replay", {}).get("reason")
            == "interrupted_queue_claim_reentry", "turn record lacks replay provenance")
    require(len(_completed(events)) == 2 and len(_result_briefs(events)) == 2,
            "timer and source recovery must each deliver once")
    require(sum(event["payload"].get("last_assistant_message") == "QA_PARK_DONE"
                for event in _completed(events)) == 1,
            "source replay must finish with the expected delivery marker")
    return "verified-i3754-isolated"


class AcceptanceRun(FaultRun):
    def __init__(self, args, case):
        super().__init__(args, case)
        self.scenario = "tool-continuation" if case == "T3753" else "interjection"

    def configure(self, **settings):
        payload = json.dumps(settings)
        script = ("import json,sys,pathlib;"
                  "pathlib.Path('/tmp/qa-fault.json').write_text(sys.stdin.read())")
        for name in (self.mock, self.proxy):
            self.docker("exec", "-i", name, "python", "-c", script, data=payload)

    def events(self, agent="qa", after_seq=None):
        suffix = "" if after_seq is None else f"&after_seq={after_seq}"
        return self.api("GET", f"/api/agents/{agent}/events?limit=400&order=asc{suffix}")

    def mock_records(self):
        lines = self.docker("logs", self.mock).stdout.splitlines()
        return [json.loads(line) for line in lines if line.startswith("{")]

    def snapshot(self):
        for _ in range(5):
            result = self.docker("exec", self.controller, "python", SCRIPT, "snapshot",
                                 check=False)
            if result.returncode == 0:
                return json.loads(result.stdout)
            time.sleep(0.2)
        raise AssertionError("runtime database snapshot failed")

    def wait_until(self, predicate, timeout, label):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(0.2)
        raise AssertionError(f"timed out waiting for {label}")

    def wait_completed(self, agent="qa", after_seq=None):
        def done():
            events = self.events(agent, after_seq)
            if events.get("has_newer"):
                raise AssertionError(f"{agent} event capture truncated")
            return events if _completed(events["events"]) else None
        return self.wait_until(done, self.args.timeout, f"{agent} completed turn")

    def create_and_bootstrap(self, agent="qa"):
        created = self.api("POST", f"/api/control/agents/{agent}/create", {})
        write_json(self.evidence / f"{agent}-create.json", created)
        events = self.wait_completed(agent)
        write_json(self.evidence / f"{agent}-bootstrap-events.json", redact_evidence(events))
        return events.get("cursor_seq") or max(e["event_seq"] for e in events["events"])

    def release(self, gate):
        self.docker("exec", self.daemon, "touch", f"/var/lib/holon/{gate}")

    def cooling_fence(self, model_ref="qa-a@default/gpt-5.4"):
        now = datetime.now(timezone.utc)
        for name, rows in self.snapshot().get("rows", {}).items():
            if "incident" not in name:
                continue
            for row in rows:
                if row.get("last_model_ref") != model_ref:
                    continue
                retry = row.get("next_retry_at")
                if retry and datetime.fromisoformat(str(retry).replace("Z", "+00:00")) > now:
                    return row
        return None

    def parked_source(self, message_id=None, marker="QA_PARK"):
        snapshot = self.snapshot()
        parked = _entry_with(snapshot, marker)
        if parked is None or parked["status"] != "interrupted":
            return None
        if message_id is not None and parked["message_id"] != message_id:
            return None
        parking = _rows(snapshot, "provider_queue_parking")
        return parked if any(row.get("message_id") == parked["message_id"]
                             for row in parking) else None

    def prepare_interrupted_source(self):
        # The real operator input must itself become the interrupted source.
        # Provider-owned opaque reasoning pins its continuation to A. Cool only
        # A while the tool is held; independent timer rounds can still use B
        # without changing the original chain fingerprint or parking deadline.
        self.create_and_bootstrap("qa2")
        prompt = self.api("POST", "/api/control/agents/qa/prompt", {"text": "QA_PARK"})
        write_json(self.evidence / "parked-prompt.json", prompt)
        self.wait_until(lambda: any(row.get("marker") == "QA_PARK"
                                    and row.get("response") == "tool_call"
                                    for row in self.mock_records()),
                        30, "original source tool call")
        self.api("POST", "/api/control/agents/qa2/prompt", {"text": "QA_COOL_CHAIN"})
        self.wait_until(self.cooling_fence, 15, "durable A cooling fence")
        require(self.cooling_fence("qa-b@default/gpt-5.4") is None,
                "B must remain healthy for the independent timer")
        write_json(self.evidence / "a-cooling.json", redact_evidence(self.snapshot()))
        self.release(PARK_GATE)
        self.wait_until(lambda: self.parked_source(prompt["message_id"]),
                        45, "original operator source interrupted at continuation preflight")
        source = self.snapshot()
        write_json(self.evidence / "original-reasoning.json", redact_evidence(source))
        require(_source_has_reasoning(source, prompt["message_id"]),
                "source assistant_blocks must contain parsed provider-owned reasoning_text")


    def begin(self, gate, issue):
        self.start()
        fixture_names = ("fault_proxy/server.py", "openai_stub/server.py")
        fixture_root = Path(__file__).resolve().parents[2] / "tests/e2e/docker"
        expected = {name: hashlib.sha256((fixture_root / name).read_bytes()).hexdigest()
                    for name in fixture_names}
        script = ("import hashlib,json,pathlib;"
                  f"names={fixture_names!r};"
                  "print(json.dumps({name:hashlib.sha256("
                  "(pathlib.Path('/fixtures')/name).read_bytes()).hexdigest()"
                  " for name in names}))")
        mounted = {name: json.loads(self.docker("exec", name, "python", "-c", script).stdout)
                   for name in (self.mock, self.proxy)}
        write_json(self.evidence / "mounted-fixture-hashes.json",
                   {"expected": expected, "mounted": mounted})
        require(all(hashes == expected for hashes in mounted.values()),
                "mounted fixtures differ from the QA source")
        self.configure(SCENARIO=self.scenario)
        write_json(self.evidence / "scenario.json", {
            "case": self.case, "issue": issue, "scenario": self.scenario,
            "tool_gate": f"/var/lib/holon/{gate}",
            "fixture": "tests/e2e/docker/fault_proxy/server.py",
        })

    def run_t3753(self):
        self.begin(TOOL_GATE, "#3753")
        cursor = self.create_and_bootstrap()
        prompt = self.api("POST", "/api/control/agents/qa/prompt",
                          {"text": "QA_TOOL_CONTINUE"})
        write_json(self.evidence / "prompt.json", prompt)
        self.wait_until(lambda: any(row.get("response") == "tool_call"
                                    and row.get("marker") == "QA_TOOL_CONTINUE"
                                    for row in self.mock_records()),
                        30, "A tool call")
        self.api("POST", "/api/control/agents/qa2/create", {})
        self.wait_completed("qa2")
        self.api("POST", "/api/control/agents/qa2/prompt", {"text": "QA_COOL_A"})
        self.wait_until(lambda: any(row.get("response") == "cooling"
                                    for row in self.mock_records()),
                        15, "A cooling HTTP response")
        self.wait_until(self.cooling_fence, 15, "durable A cooling fence")
        self.release(TOOL_GATE)
        events = self.wait_completed("qa", cursor)
        self.docker("stop", "--time", "5", self.daemon)
        http = self.mock_records()
        snapshot = self.snapshot()
        write_json(self.evidence / "events.json", redact_evidence(events))
        write_json(self.evidence / "http.json", http)
        write_json(self.evidence / "snapshot.json", redact_evidence(snapshot))
        return evaluate_tool_continuation(events["events"], http, snapshot)

    def run_i3754(self):
        self.begin(INTERJECT_GATE, "#3754")
        cursor = self.create_and_bootstrap()
        self.prepare_interrupted_source()
        before = self.snapshot()
        write_json(self.evidence / "before-timer.json", redact_evidence(before))
        self.configure(SCENARIO=self.scenario, HOLD_TIMER_RESPONSE=True)
        timer = self.api("POST", "/api/control/agents/qa/timers",
                         {"duration_ms": 300, "summary": "QA_TIMER"})
        write_json(self.evidence / "timer.json", timer)
        self.wait_until(lambda: any(row.get("response") == "tool_call"
                                    and row.get("marker") == "QA_TIMER"
                                    for row in self.mock_records()),
                        45, "timer turn tool call")
        fresh = self.api("POST", "/api/control/agents/qa/prompt", {"text": "QA_FRESH"})
        write_json(self.evidence / "fresh-prompt.json", fresh)
        boundary = self.snapshot()
        write_json(self.evidence / "before-tool.json", redact_evidence(boundary))
        self.release(INTERJECT_GATE)
        self.configure(SCENARIO=self.scenario)
        events = self.wait_completed("qa", cursor)
        http = self.mock_records()
        snapshot = self.snapshot()
        write_json(self.evidence / "timer-events.json", redact_evidence(events))
        write_json(self.evidence / "after-timer.json", redact_evidence(snapshot))
        evaluate_interjection(events["events"], http, snapshot, before, boundary)
        self.configure(SCENARIO=self.scenario, RECOVER_PARK=True)

        def source_processed():
            row = _entry_with(self.snapshot(), "QA_PARK")
            return row is not None and row["status"] == "processed"

        self.wait_until(source_processed, self.args.timeout, "deadline source reentry")
        # The event API must be read while the daemon is still serving.
        events = self.events("qa", cursor)
        require(not events.get("has_newer"), "recovery events are truncated")
        self.docker("stop", "--time", "5", self.daemon)
        snapshot = self.snapshot()
        http = self.mock_records()
        write_json(self.evidence / "events.json", redact_evidence(events))
        write_json(self.evidence / "http.json", http)
        write_json(self.evidence / "snapshot.json", redact_evidence(snapshot))
        return evaluate_recovery(events["events"], snapshot, before)

    def run(self):
        status, error = None, None
        try:
            status = self.run_t3753() if self.case == "T3753" else self.run_i3754()
        except Exception as failure:
            error = failure
        finally:
            cleanup_errors = self.cleanup()
        result = {"case": self.case, "evidence": str(self.evidence)}
        result["status"] = status or "failed-or-blocked"
        if error is not None:
            result["error"] = str(error)
        if cleanup_errors:
            result["status"] = "failed-or-blocked"
            result["cleanup_errors"] = cleanup_errors
        return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--python-image", required=True)
    parser.add_argument("--expected-revision", required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--case", action="append", choices=CASES)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--http-timeout", type=int, default=45)
    args = parser.parse_args(argv)
    if not re.fullmatch(r"[0-9a-f]{8,40}", args.expected_revision):
        parser.error("--expected-revision must contain 8-40 lowercase hex characters")
    args.evidence_dir = args.evidence_dir.resolve()
    for field in ("image", "python_image"):
        result = __import__("subprocess").run(
            ["docker", "image", "inspect", getattr(args, field), "--format", "{{.Id}}"],
            text=True, capture_output=True, check=True)
        setattr(args, field, result.stdout.strip())
    results = []
    for case in args.case or CASES:
        run = AcceptanceRun(args, case)
        try:
            result = run.run()
        except Exception as error:
            result = {"case": case, "status": "failed-or-blocked",
                      "error": str(error), "evidence": str(run.evidence)}
        results.append(result)
        write_json(args.evidence_dir / "summary.json", results)
        print(json.dumps(result), flush=True)
    return 0 if all(item["status"].startswith("verified-") for item in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
