"""Isolated-daemon acceptance for #3753, #3754, #3772 and #3773.

T3753: A produces a tool call, A cools before the continuation, independent B
must continue the same turn with the original tool result and a single delivery.

I3754: a provider-parked Interrupted operator source must be retained, a fresh
queued input must still interject, and the timer turn must settle durably.

T3772I: fresh operator input interrupts a native turn-local cooldown wait.
R3773I: an operator-rooted fresh turn admits one held-response follow-up and
leaves the scheduler-parked Queued recovery owned until its real deadline.
Neither new case replaces I3754 or covers an admission-after-claim race.
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

CASES = ("T3753", "I3754", "T3772", "T3772I", "R3773", "R3773I")
# I3754 keeps its original contract and is not part of this new selected set.
INTERJECTION_CASES = ("T3753", "T3772", "T3772I", "R3773", "R3773I")
TOOL_GATE = "qa-3753-gate"
INTERJECT_GATE = "qa-3754-gate"
PARK_GATE = "qa-3754-park-gate"
NATIVE_GATE = "qa-3772-native-gate"
FRESH_GATE = "qa-3773-fresh-gate"


def _native_tools(snapshot):
    return [row for row in _rows(snapshot, "tool_executions")
            if NATIVE_GATE in json.dumps(row)]


def _recovery_sources(snapshot):
    sources = []
    for row in _rows(snapshot, "messages"):
        payload = json.loads(row.get("payload_json") or "{}")
        if (row.get("agent_id") == "qa"
                and payload.get("kind") == "internal_followup"
                and payload.get("authority_class") == "runtime_instruction"
                and payload.get("origin") == {
                    "kind": "system", "subsystem": "model_lineage_recovery",
                }):
            sources.append(row)
    return sources


def evaluate_native_cooldown(events, http, snapshot, observation, message_id, *,
                             interjection=False):
    """Require a real fault, an IO-free active wait, and one unchanged continuation."""
    waiting = observation["snapshot"]
    for checkpoint in (snapshot, waiting):
        _complete_snapshot(checkpoint)
    state = observation["state"]
    wait = state["agent"]["agent"].get("provider_cooldown_wait")
    require(wait is not None and wait.get("turn_id") and wait.get("attempt_id"),
            "query must expose a turn/attempt-bound cooldown wait")
    require("qa-a" in str(wait.get("model")), "native continuation changed model")
    require(_instant(wait["not_before"]) > _instant(wait["started_at"]),
            "wait must expose an authoritative future deadline")
    require(observation["http_before"] == observation["http_after"]
            and observation["tcp_before"] == observation["tcp_after"],
            "cooldown observation must contain zero HTTP and TCP IO")
    require(observation["requests_before"] == observation["requests_after"],
            "cooldown wait must not charge a durable provider request")
    require(_instant(observation["sampled_until"]) < _instant(wait["not_before"]),
            "zero-IO sample must precede the authoritative deadline")
    require(observation["elapsed_seconds"] >= 1,
            "zero-IO observation needs a real bounded observation interval")
    source = _entry_with(waiting, "QA_NATIVE")
    require(source is not None and source["message_id"] == message_id
            and source["status"] not in ("interrupted", "processed"),
            "native source must remain active, not parked or prematurely consumed")
    require(not _source_rows(waiting, "provider_queue_parking", message_id),
            "turn-local wait must not park its source")
    cause = wait.get("cause", {})
    require(cause.get("failure_kind") == "server_error"
            and cause.get("http_status") == 503 and cause.get("occurred_at"),
            "wait cause must describe the actual helper HTTP 503")
    require(cause.get("retry_after", {}).get("source") == "server_header",
            "cause must expose the real Retry-After classification")
    require("fixture-only diagnostic" not in json.dumps(wait),
            "query leaked raw provider diagnostics")
    changed = [event for event in events if event["type"] == "agent_state_changed"
               and event.get("payload", {}).get("provider_cooldown_wait")]
    require(changed and any(event["payload"]["provider_cooldown_wait"] == wait
                            for event in changed),
            "events must expose the same cause and deadline as the query")
    ended = [event for event in events if event["type"] == "agent_state_changed"
             and event.get("payload", {}).get("provider_cooldown_wait_ended")]
    outcomes = [event["payload"]["provider_cooldown_wait_ended"].get("outcome")
                for event in ended]
    require(outcomes == (["interjected", "resumed"] if interjection else ["resumed"]),
            "cooldown wait must end once by resuming, not terminal repair")
    failures = [row for row in http if row.get("response") == "cooling"
                and row.get("side") == "a" and row.get("status") == 503]
    require(failures, "missing real helper HTTP failure")
    initial = [row for row in http if row.get("response") == "native_tool_call"]
    continued = [row for row in http if row.get("tool_output")]
    require(len(initial) == len(continued) == 1
            and initial[0]["side"] == continued[0]["side"] == "a"
            and initial[0].get("model") == continued[0].get("model"),
            "native round and its single continuation must use the exact same model")
    continuation = continued[0]
    native = continuation.get("native_context", [])
    require(continuation.get("previous_response_id") == "resp_a_native"
            or any(item.get("encrypted_content") == "qa-native-opaque"
                   and item.get("content") == [{
                       "type": "reasoning_text", "text": "QA_NATIVE_REASONING",
                   }] for item in native),
            "native provider context was lost or rebuilt without opaque state")
    outputs = continuation.get("tool_outputs", [])
    require(len(outputs) == 1 and outputs[0].get("call_id") == "call-native"
            and "QA_NATIVE_TOOL_DONE" in str(outputs[0].get("output")),
            "continuation must carry the original tool result exactly once")
    require(datetime.fromtimestamp(continuation["at"], timezone.utc)
            >= _instant(wait["not_before"]), "continuation dispatched before deadline")
    terminals = [event for event in events if event["type"] == "turn_terminal"]
    require(len(terminals) == 1 and len(_completed(events)) == 1
            and terminals[0]["payload"].get("turn_id") == wait["turn_id"]
            and terminals[0]["payload"].get("last_assistant_message")
            == ("QA_NATIVE_INTERJECT_DONE" if interjection else "QA_NATIVE_DONE"),
            "the original turn must complete once without a recovery/repair terminal")
    require(len(_result_briefs(events)) == 1, "expected one user-facing result")
    require(not any(event["type"] in ("turn_replay_started", "provider_candidate_reselected")
                    for event in events), "native continuation must not replay or reselect")
    tools = _native_tools(snapshot)
    require(len(tools) == 1
            and json.loads(tools[0]["payload_json"]).get("status") == "success",
            "native tool must execute successfully exactly once")
    processed = [event for event in events if event["type"] == "queue_entry_settled"
                 and event.get("payload", {}).get("message_id") == message_id
                 and event["payload"].get("status") == "processed"]
    require(len(processed) == 1
            and _entry_with(snapshot, "QA_NATIVE")["status"] == "processed",
            "source must be consumed exactly once")
    require(not _recovery_sources(snapshot), "turn-local wait created a repair input")
    return "verified-t3772-isolated"


def evaluate_recovery_cooldown(events, http, snapshot, before, *, competition=False):
    """Recovery input must survive a second, real shared-scope failure."""
    for checkpoint in (snapshot, before):
        _complete_snapshot(checkpoint)
    sources = _recovery_sources(before)
    require(len(sources) == 1, "expected one real runtime recovery source")
    source = sources[0]
    mid = source["message_id"]
    require(_source_rows(snapshot, "messages", mid) == [source],
            "recovery must preserve the canonical recovery input byte-for-byte")
    parked = _source_rows(before, "provider_queue_parking", mid)
    require(len(parked) == 1, "recovery input must really park before its deadline")
    entry = next((row for row in _queue_rows(before) if row["message_id"] == mid), None)
    require(entry is not None and entry["status"] == "queued",
            "scheduler-parked recovery input must remain Queued until admission")
    resumed = [row for row in http if mid in row.get("recovery_input", [])]
    require(len(resumed) == 1 and resumed[0].get("response") == "QA_RECOVERY_DONE",
            "recovery input must dispatch once")
    require(datetime.fromtimestamp(resumed[0]["at"], timezone.utc)
            >= _instant(parked[0]["not_before"]), "recovery dispatched before its deadline")
    require(any(row.get("response") == "source_failure"
                and row.get("status") == 503 for row in http),
            "missing original post-tool HTTP failure")
    require({row["side"] for row in http if row.get("response") == "cooling"
             and row.get("status") == 503} == {"a", "b"},
            "compound fault must use real shared-scope HTTP failures on both candidates")
    processed = [event for event in events if event["type"] == "queue_entry_settled"
                 and event.get("payload", {}).get("message_id") == mid
                 and event["payload"].get("status") == "processed"]
    require(len(processed) == 1 and not _source_rows(snapshot, "provider_queue_parking", mid),
            "retained recovery input must be consumed once and unparked")
    final_entry = next((row for row in _queue_rows(snapshot)
                        if row["message_id"] == mid), None)
    require(final_entry is not None and final_entry["status"] == "processed",
            "retained recovery input must finish in canonical Processed state")
    claims = [event for event in events if event["type"] == "queue_entry_claimed"
              and event.get("payload", {}).get("message_id") == mid]
    require(len(claims) == 1
            and _instant(claims[0]["ts"]) >= _instant(parked[0]["not_before"]),
            "scheduler must claim the retained input exactly once after its deadline")
    require(not any(event["type"] == "turn_replay_started"
                    and event.get("payload", {}).get("message_id") == mid
                    for event in events), "scheduler-parked input must not invent a replay")
    tools = _native_tools(snapshot)
    require(len(tools) == 1 and tools == _native_tools(before),
            "recovery must preserve the original tool evidence byte-for-byte")
    tool = json.loads(tools[0]["payload_json"])
    tool_id = tools[0].get("evidence_id")
    result = tool.get("output", {}).get("envelope", {}).get("result", {})
    require(tool_id and tool.get("id") == tool_id
            and tool_id in resumed[0].get("tool_evidence_refs", [])
            and tool.get("agent_id") == tools[0].get("agent_id") == "qa"
            and tool.get("output", {}).get("is_error") is False
            and tool.get("status") == "success" and result.get("exit_status") == 0
            and "QA_NATIVE_TOOL_DONE" in result.get("stdout_preview", ""),
            "recovery prompt must reference the original successful tool output")
    source_failure = [row for row in http if row.get("response") == "source_failure"]
    require(len(source_failure) == 1 and tool.get("completed_at")
            and _instant(tool["completed_at"])
            < datetime.fromtimestamp(source_failure[0]["at"], timezone.utc),
            "the original tool must finish before the source provider failure")
    recovery_completed = [event for event in _completed(events)
                          if event["payload"].get("last_assistant_message") == "QA_RECOVERY_DONE"]
    require(len(_completed(events)) == len(_result_briefs(events)) == (2 if competition else 1)
            and len(recovery_completed) == 1, "recovery must deliver once")
    require(not any(event["type"] == "turn_terminal"
                    and event.get("payload", {}).get("kind") == "aborted"
                    for event in events), "recovery must not abort")
    failed = [turn for turn in _turns(snapshot) if turn.get("terminal", {}).get("kind")
              == "provider_failed_needs_recovery"]
    completed_id = recovery_completed[0]["payload"].get("turn_id")
    completed = [turn for turn in _turns(snapshot) if turn.get("turn_id") == completed_id]
    recovery = json.loads(source["payload_json"])
    refs = recovery.get("source_refs", {})
    directive = recovery.get("metadata", {}).get("provider_recovery", {})
    source_mid = refs.get("source_message_id")
    require(len(failed) == len(completed) == 1 and failed[0].get("owner") is not None
            and failed[0]["owner"] == completed[0].get("owner")
            and refs.get("source_turn_id") == failed[0].get("turn_id")
            and directive.get("source_turn_id") == refs.get("source_turn_id")
            and directive.get("source_message_id") == source_mid
            and directive.get("source_terminal_kind") == "provider_failed_needs_recovery"
            and source_mid and failed[0].get("input_message_ids") == [source_mid]
            and tool.get("turn_id") == tools[0].get("turn_id") == failed[0].get("turn_id")
            and tool_id in failed[0].get("tool_execution_ids", [])
            and completed[0].get("trigger", {}).get("message_id") == mid
            and len(completed[0].get("input_message_ids", [])) == 2
            and set(completed[0]["input_message_ids"]) == {mid, source_mid},
            "recovery must inherit only its validated source and owner")
    original_settled = [event for event in events if event["type"] == "queue_entry_settled"
                        and event.get("payload", {}).get("message_id") == source_mid]
    require(len(original_settled) == 1
            and original_settled[0]["payload"].get("status") == "processed"
            and not any(event["type"] == "queue_entry_claimed"
                        and event.get("payload", {}).get("message_id") == source_mid
                        and _instant(event["ts"]) >= _instant(parked[0]["not_before"])
                        for event in events),
            "context source references must not re-consume the original operator input")
    return "verified-r3773-isolated"


def _message(snapshot, message_id):
    rows = _source_rows(snapshot, "messages", message_id)
    require(len(rows) == 1, "expected one canonical message, not a marker match")
    message = json.loads(rows[0]["payload_json"])
    require(message.get("id") == message_id
            and message.get("agent_id") == rows[0].get("agent_id") == "qa",
            "canonical message id/agent mismatch")
    return message


def _operator(message):
    require(message.get("kind") == "operator_prompt"
            and message.get("origin", {}).get("kind") == "operator"
            and message.get("authority_class") == "operator_instruction"
            and message.get("delivery_surface") == "http_control_prompt"
            and message.get("admission_context") in ("control_authenticated", "local_process"),
            "input must be an independently authenticated operator prompt")


def _exact_inputs(turn, expected):
    inputs = turn.get("input_message_ids", [])
    require(len(inputs) == len(expected) and set(inputs) == set(expected),
            "turn input ids must be exact, without duplicates or unrelated inputs")


def _fresh_interjection(events, http, snapshot, fresh_id, turn_id, boundary, text):
    fresh = _message(snapshot, fresh_id)
    _operator(fresh)
    require(fresh.get("body", {}).get("text") == text, "fresh canonical text mismatch")
    admitted = [event for event in events if event["type"] == "operator_interjection_admitted"]
    require(len(admitted) == 1 and admitted[0]["payload"].get("message_id") == fresh_id
            and admitted[0]["payload"].get("turn_id") == turn_id
            and admitted[0]["payload"].get("boundary") == boundary
            and admitted[0]["payload"].get("authority_class") == "operator_instruction",
            "fresh prompt must be admitted exactly once into the intended turn/boundary")
    entries = _source_rows(snapshot, "queue_entries", fresh_id)
    require(len(entries) == 1 and entries[0].get("status") == "interjected",
            "fresh queue entry must finish Interjected, not independently Processed")
    turns = [turn for turn in _turns(snapshot) if fresh_id in turn.get("input_message_ids", [])]
    require(len(turns) == 1 and turns[0].get("turn_id") == turn_id,
            "fresh input must not create an independent reply turn")
    require(not any(event["type"] == "queue_entry_claimed"
                    and event.get("payload", {}).get("message_id") == fresh_id
                    for event in events), "fresh input was independently claimed")
    rendered = [(row, item) for row in http for item in row.get("interjections", [])
                if item.get("message_id") == fresh_id]
    require(len(rendered) == 1, "fresh prompt must reach provider IO exactly once")
    row, item = rendered[0]
    require(item.get("text") == text
            and json.loads(item.get("origin", "null")) == fresh["origin"]
            and item.get("authority_class") == fresh["authority_class"]
            and item.get("delivery_surface") == fresh["delivery_surface"]
            and item.get("admission_context") == fresh["admission_context"],
            "provider interjection must retain the exact canonical operator provenance/text")
    return turns[0], row, admitted[0]


def evaluate_native_interjection(events, http, snapshot, observation, message_id, fresh_id):
    evaluate_native_cooldown(events, http, snapshot, observation, message_id,
                             interjection=True)
    original = _message(snapshot, message_id)
    _operator(original)
    wait = observation["state"]["agent"]["agent"]["provider_cooldown_wait"]
    reentered = observation["reentered"]
    _complete_snapshot(reentered["snapshot"])
    next_wait = reentered["state"]["agent"]["agent"].get("provider_cooldown_wait")
    require(next_wait is not None and all(next_wait.get(key) == wait.get(key)
            for key in ("turn_id", "attempt_id", "model", "not_before", "cause"))
            and _instant(next_wait["started_at"]) >= _instant(wait["started_at"])
            and _instant(reentered["sampled_at"]) < _instant(wait["not_before"]),
            "interjection must reenter the same execution/model with its original deadline")
    active = _source_rows(reentered["snapshot"], "queue_entries", message_id)
    require(len(active) == 1 and active[0].get("status") not in ("processed", "interrupted"),
            "original source must stay active across interjection")
    turn, request, admitted = _fresh_interjection(
        events, http, snapshot, fresh_id, wait["turn_id"],
        "before_provider_continuation", "QA_FRESH_NATIVE")
    _exact_inputs(turn, (message_id, fresh_id))
    require(turn.get("trigger", {}).get("message_id") == message_id
            and turn.get("agent_id") == "qa" and turn.get("owner"),
            "native turn must retain its original trigger and owner")
    require(request.get("tool_output") and request.get("response") == "QA_NATIVE_INTERJECT_DONE",
            "fresh text and original native tool output must share one continuation")
    tools = _native_tools(snapshot)
    tool = json.loads(tools[0]["payload_json"])
    require(tool.get("id") == tools[0].get("evidence_id") and tool.get("id")
            and tool.get("turn_id") == tools[0].get("turn_id") == wait["turn_id"]
            and tool.get("agent_id") == tools[0].get("agent_id") == "qa"
            and tool.get("output", {}).get("is_error") is False
            and tool.get("output", {}).get("envelope", {}).get("result", {}).get("exit_status") == 0
            and _native_tools(observation["snapshot"])
            == _native_tools(reentered["snapshot"]) == tools,
            "interjection must retain one immutable successful native tool execution")
    for checkpoint in (observation["snapshot"], reentered["snapshot"], snapshot):
        require(_source_rows(checkpoint, "messages", message_id)
                == _source_rows(snapshot, "messages", message_id),
                "native interjection changed the canonical original source")
        require(not _source_rows(checkpoint, "provider_queue_parking", message_id),
                "native interjection must never park its source")
    started = [(index, event["payload"]["provider_cooldown_wait"])
               for index, event in enumerate(events)
               if event["type"] == "agent_state_changed"
               and event.get("payload", {}).get("provider_cooldown_wait")]
    ended = [(index, event["payload"]["provider_cooldown_wait_ended"])
             for index, event in enumerate(events)
             if event["type"] == "agent_state_changed"
             and event.get("payload", {}).get("provider_cooldown_wait_ended")]
    require(len(started) == len(ended) == 2
            and started[0][1] == wait and started[1][1] == next_wait
            and started[0][0] < ended[0][0] < events.index(admitted)
            < started[1][0] < ended[1][0]
            and all(item.get("turn_id") == wait["turn_id"] for _, item in ended)
            and _instant(ended[0][1]["ended_at"]) < _instant(wait["not_before"])
            and _instant(ended[1][1]["ended_at"]) >= _instant(wait["not_before"]),
            "wait events must prove interjected -> same-deadline wait -> resumed")
    return "verified-t3772i-isolated"


def _incident_deadline(incident):
    deadlines = [incident[key] for key in ("next_retry_at", "server_not_before")
                 if incident.get(key)]
    require(deadlines, "missing actual incident cooldown deadline")
    return max(_instant(value) for value in deadlines)


def evaluate_recovery_interjection(events, http, snapshot, before, competition):
    evaluate_recovery_cooldown(events, http, snapshot, before, competition=True)
    boundary = competition["snapshot"]
    queued = competition["queued"]
    for checkpoint in (boundary, queued):
        _complete_snapshot(checkpoint)
    sources = _recovery_sources(snapshot)
    require(len(sources) == 1 and sources == _recovery_sources(before)
            == _recovery_sources(boundary), "recovery must remain unique and immutable")
    recovery_id = sources[0]["message_id"]
    recovery = _message(snapshot, recovery_id)
    refs = recovery.get("source_refs", {})
    directive = recovery.get("metadata", {}).get("provider_recovery", {})
    source_id = refs.get("source_message_id")
    source = _message(snapshot, source_id)
    _operator(source)
    require(recovery.get("delivery_surface") == "runtime_system"
            and recovery.get("admission_context") == "runtime_owned"
            and recovery.get("causation_id") == source_id
            and directive.get("root_message_id") == source_id
            and directive.get("source_message_id") == source_id
            and source.get("causation_id") is None,
            "recovery must preserve its authenticated source, causation and lineage root")
    resumed = [row for row in http if recovery_id in row.get("recovery_input", [])]
    require(resumed[0].get("recovery_input") == [recovery_id],
            "provider request must contain the one canonical recovery source exactly once")
    failed = [turn for turn in _turns(snapshot)
              if turn.get("turn_id") == refs.get("source_turn_id")]
    require(len(failed) == 1 and failed[0].get("agent_id") == "qa"
            and failed[0].get("trigger", {}).get("message_id") == source_id
            and failed[0]["trigger"].get("origin") == source["origin"]
            and failed[0]["trigger"].get("authority_class") == source["authority_class"],
            "failed source turn trigger/provenance mismatch")
    for checkpoint in (before, queued, boundary):
        require([turn for turn in _turns(checkpoint)
                 if turn.get("turn_id") == failed[0]["turn_id"]] == failed,
                "competition changed the failed source turn or its original owner")
        require(not any(turn.get("trigger", {}).get("message_id") == recovery_id
                        for turn in _turns(checkpoint))
                and not _source_rows(checkpoint, "conversation_input_assignments", recovery_id),
                "scheduler-parked recovery must not be claimed or assigned before its deadline")
    completed = [turn for turn in _turns(snapshot)
                 if turn.get("trigger", {}).get("message_id") == recovery_id]
    require(len(completed) == 1
            and completed[0]["trigger"].get("origin") == recovery["origin"]
            and completed[0]["trigger"].get("authority_class") == recovery["authority_class"]
            and completed[0].get("agent_id") == "qa",
            "recovery turn trigger/provenance mismatch")
    _exact_inputs(completed[0], (recovery_id, source_id))
    parked = _source_rows(before, "provider_queue_parking", recovery_id)
    require(parked[0].get("chain_fingerprint") and parked[0].get("episode_key"),
            "parking must expose its real chain fingerprint and episode owner")
    assignment = _source_rows(before, "conversation_input_assignments", source_id)
    require(len(assignment) == 1 and assignment[0].get("turn_id") == failed[0]["turn_id"]
            and assignment[0].get("agent_id") == "qa",
            "original input must retain its canonical source-turn owner")
    for checkpoint in (before, queued, boundary, snapshot):
        require(_source_rows(checkpoint, "messages", source_id)
                == _source_rows(snapshot, "messages", source_id)
                and _source_rows(checkpoint, "messages", recovery_id)
                == _source_rows(snapshot, "messages", recovery_id)
                and _source_rows(checkpoint, "conversation_input_assignments", source_id)
                == assignment, "competition changed original source or consumption owner")
    for checkpoint in (queued, boundary):
        require(_source_rows(checkpoint, "provider_queue_parking", recovery_id) == parked,
                "competition changed recovery deadline, fingerprint or episode")
    fresh_entry = _source_rows(queued, "queue_entries", competition["f1_id"])
    require(len(fresh_entry) == 1 and fresh_entry[0].get("status") == "queued",
            "fresh follow-up must queue while B holds the fresh root response")
    for mid in (competition["f0_id"], competition["f1_id"]):
        require(all(_source_rows(checkpoint, "messages", mid)
                    == _source_rows(snapshot, "messages", mid)
                    for checkpoint in (queued, boundary)),
                "competition changed the fresh operator inputs")
    entry = _source_rows(boundary, "queue_entries", recovery_id)
    require(len(entry) == 1 and entry[0].get("status") == "queued",
            "pre-claim parked recovery must stay Queued throughout competition")
    park_events = [event for event in events if event["type"] == "provider_queue_parked"
                   and event.get("payload", {}).get("message_id") == recovery_id]
    require(park_events and any(
        event["payload"].get("not_before") == parked[0]["not_before"]
        and event["payload"].get("episode_key") == parked[0]["episode_key"]
        for event in park_events), "missing actual scheduler ProviderParked evidence")
    fresh_final = [event for event in _completed(events)
                   if event["payload"].get("last_assistant_message") == "QA_FRESH_DONE"]
    require(len(fresh_final) == 1, "competition needs exactly one operator-rooted final")
    turn, request, admitted = _fresh_interjection(
        events, http, snapshot, competition["f1_id"], fresh_final[0]["payload"]["turn_id"],
        "before_tool_execution", "QA_FRESH_FOLLOW")
    root = _message(snapshot, competition["f0_id"])
    _operator(root)
    require(root.get("causation_id") is None
            and turn.get("trigger", {}).get("message_id") == competition["f0_id"]
            and turn.get("agent_id") == "qa" and turn.get("owner"),
            "fresh competition must run on an independent authenticated operator root")
    _exact_inputs(turn, (competition["f0_id"], competition["f1_id"]))
    follow = _message(snapshot, competition["f1_id"])
    require(follow.get("origin") == root.get("origin")
            and follow.get("authority_class") == root.get("authority_class")
            and follow.get("admission_context") == root.get("admission_context"),
            "interjected follow-up must not upgrade its source or authority")
    require(not any(event["type"] == "operator_interjection_admitted"
                    and event.get("payload", {}).get("message_id") == recovery_id
                    for event in events),
            "interjection drain must never consume the parked recovery")
    for mid in (competition["f0_id"], competition["f1_id"]):
        require(not any(_source_rows(checkpoint, "provider_queue_parking", mid)
                        for checkpoint in (queued, boundary, snapshot)),
                "fresh competition must never park or reset episode state")
    require(request.get("response") == "QA_FRESH_DONE" and request.get("side") == "b"
            and not request.get("recovery_input"),
            "healthy B must settle the operator-rooted fresh turn, not steal recovery")
    for row in resumed:
        current_ids = [item.get("id") for item in row.get("current_input", [])]
        require(competition["f0_id"] not in current_ids
                and competition["f1_id"] not in current_ids,
                "recovery request current input must stay isolated from the competition")
    failed_terminals = [event for event in events if event["type"] == "turn_terminal"
                        and event.get("payload", {}).get("kind")
                        == "provider_failed_needs_recovery"]
    require(len(failed_terminals) == 1
            and failed_terminals[0]["payload"].get("turn_id") == failed[0]["turn_id"]
            and len([event for event in events if event["type"] == "turn_terminal"]) == 3,
            "expected only the source failure, operator-rooted fresh and unique recovery final")
    until = _instant(parked[0]["not_before"])
    b_ready = _incident_deadline(competition["b_incident"])
    fresh_held = [row for row in http if row.get("response") == "fresh_response_held"
                  and row.get("side") == "b"]
    require(fresh_held and competition["b_incident"].get("last_model_ref")
            == "qa-b@default/gpt-5.4"
            and b_ready <= _instant(competition["started_at"])
            <= datetime.fromtimestamp(fresh_held[0]["at"], timezone.utc)
            <= _instant(admitted["ts"]) <= _instant(fresh_final[0]["ts"])
            <= _instant(competition["sampled_at"]) < until,
            "B-ready operator competition must dispatch, admit and finish before original D")
    cooling = [row for row in http if row.get("response") == "cooling"]
    require(any(row.get("side") == "a" and row.get("retry_after") == "24" for row in cooling)
            and any(row.get("side") == "b" and row.get("retry_after") == "1" for row in cooling),
            "competition must arise from different real server Retry-After headers")
    held = [row for row in http if row.get("response") == "source_failure_held"]
    failures = [row for row in http if row.get("response") == "source_failure"]
    require(len(held) == len(failures) == 1
            and all(held[0]["at"] < row["at"] < failures[0]["at"] for row in cooling),
            "original HTTP request must be held across the real dual-candidate failure")
    for mid in (source_id, recovery_id):
        settled = [event for event in events if event["type"] == "queue_entry_settled"
                   and event.get("payload", {}).get("message_id") == mid
                   and event["payload"].get("status") == "processed"]
        require(len(settled) == 1, "both retained sources must be consumed exactly once")
    return "verified-r3773i-isolated"


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
        self.scenario = {
            "T3753": "tool-continuation", "I3754": "interjection",
            "T3772": "native-cooldown", "R3773": "recovery-cooldown",
            "T3772I": "native-cooldown-interjection",
            "R3773I": "recovery-cooldown-interjection",
        }[case]

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
        fixture_names = ("fault_proxy/server.py", "openai_stub/server.py",
                         "openai_stub/continuation_cooldown.py")
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

    def native_source(self):
        cursor = self.create_and_bootstrap()
        self.create_and_bootstrap("qa2")
        prompt = self.api("POST", "/api/control/agents/qa/prompt", {"text": "QA_NATIVE"})
        write_json(self.evidence / "prompt.json", prompt)
        self.wait_until(lambda: any(row.get("response") == "native_tool_call"
                                    for row in self.mock_records()), 30, "native tool call")
        return cursor, prompt

    def run_t3772(self, interjection=False):
        self.begin(NATIVE_GATE, "#3772")
        cursor, prompt = self.native_source()
        helper_cursor = max(event["event_seq"] for event in self.events("qa2")["events"])
        self.api("POST", "/api/control/agents/qa2/prompt", {"text": "QA_COOL_NATIVE"})
        self.wait_until(self.cooling_fence, 15, "real shared-scope cooling fence")
        self.wait_completed("qa2", helper_cursor)
        self.release(NATIVE_GATE)

        def waiting():
            state = self.api("GET", "/api/agents/qa/state")
            return state if state["agent"]["agent"].get("provider_cooldown_wait") else None

        state = self.wait_until(waiting, 15, "queryable native cooldown wait")
        before = self.snapshot()
        _, http_before, tcp_before = self.observe()
        started = time.monotonic()
        time.sleep(1.1)
        _, http_after, tcp_after = self.observe()
        after = self.snapshot()
        observation = {
            "state": state, "snapshot": before,
            "http_before": http_before, "http_after": http_after,
            "tcp_before": tcp_before, "tcp_after": tcp_after,
            "requests_before": _rows(before, "provider_recovery_requests"),
            "requests_after": _rows(after, "provider_recovery_requests"),
            "sampled_until": datetime.now(timezone.utc).isoformat(),
            "elapsed_seconds": time.monotonic() - started,
        }
        write_json(self.evidence / "cooldown-observation.json", redact_evidence(observation))
        if interjection:
            # Finish the IO/budget observation before adding the independent input.
            fresh = self.api("POST", "/api/control/agents/qa/prompt",
                             {"text": "QA_FRESH_NATIVE"})
            write_json(self.evidence / "fresh-prompt.json", fresh)
            original_wait = state["agent"]["agent"]["provider_cooldown_wait"]

            def reentered():
                current = waiting()
                if current is None:
                    return None
                visible = current["agent"]["agent"]["provider_cooldown_wait"]
                if (visible.get("turn_id") != original_wait["turn_id"]
                        or visible.get("started_at") == original_wait["started_at"]):
                    return None
                snapshot = self.snapshot()
                entry = _source_rows(snapshot, "queue_entries", fresh["message_id"])
                if len(entry) != 1 or entry[0].get("status") != "interjected":
                    return None
                return {"state": current, "snapshot": snapshot,
                        "sampled_at": datetime.now(timezone.utc).isoformat()}

            observation["reentered"] = self.wait_until(
                reentered, 10, "same-execution wait after fresh interjection")
            write_json(self.evidence / "cooldown-observation.json", redact_evidence(observation))
        events = self.wait_completed("qa", cursor)
        final_state = self.api("GET", "/api/agents/qa/state")
        require(not final_state["agent"]["agent"].get("provider_cooldown_wait"),
                "completed wait was not cleared")
        self.docker("stop", "--time", "5", self.daemon)
        http, snapshot = self.mock_records(), self.snapshot()
        write_json(self.evidence / "events.json", redact_evidence(events))
        write_json(self.evidence / "http.json", http)
        write_json(self.evidence / "snapshot.json", redact_evidence(snapshot))
        if interjection:
            return evaluate_native_interjection(events["events"], http, snapshot,
                                                 observation, prompt["message_id"],
                                                 fresh["message_id"])
        return evaluate_native_cooldown(events["events"], http, snapshot, observation,
                                        prompt["message_id"])

    def run_t3772i(self):
        return self.run_t3772(interjection=True)

    def run_r3773(self, compete=False):
        self.begin(NATIVE_GATE, "#3773")
        self.configure(SCENARIO=self.scenario, HOLD_SOURCE_FAILURE=True)
        cursor, _ = self.native_source()
        self.release(NATIVE_GATE)
        self.wait_until(lambda: any(row.get("response") == "source_failure_held"
                                    for row in self.mock_records()), 30,
                        "real source continuation HTTP before shared failure")
        self.api("POST", "/api/control/agents/qa2/prompt", {"text": "QA_COOL_RECOVERY"})
        self.wait_until(lambda: self.cooling_fence()
                        and self.cooling_fence("qa-b@default/gpt-5.4"),
                        15, "real A and B shared-scope failures")
        if compete:
            b_incident = self.cooling_fence("qa-b@default/gpt-5.4")
            require(b_incident is not None, "lost real B cooling incident")
            b_deadline = _incident_deadline(b_incident)
            # Keep the real source HTTP request held until B is near recovery.
            # The source episode's own floor starts only after its 503 arrives.
            self.wait_until(lambda: (b_deadline - datetime.now(timezone.utc)).total_seconds()
                            <= 3, 20, "B cooldown nearly elapsed before source failure")
        self.configure(SCENARIO=self.scenario, HOLD_SOURCE_FAILURE=False)

        def recovery_parked():
            snapshot = self.snapshot()
            sources = _recovery_sources(snapshot)
            if len(sources) != 1:
                return None
            mid = sources[0]["message_id"]
            entry = next((row for row in _queue_rows(snapshot)
                          if row["message_id"] == mid), None)
            return snapshot if (entry and entry["status"] == "queued"
                                and _source_rows(snapshot, "provider_queue_parking", mid)) else None

        before = self.wait_until(recovery_parked, 10, "retained scheduler-parked recovery input")
        write_json(self.evidence / "recovery-parked.json", redact_evidence(before))
        if compete:
            self.wait_until(lambda: datetime.now(timezone.utc) >= b_deadline,
                            15, "real B cooldown deadline")
            sources = _recovery_sources(before)
            parking = _source_rows(before, "provider_queue_parking",
                                   sources[0]["message_id"])
            require(parking and parking[0].get("not_before"),
                    "missing the actual scheduler parking deadline")
            until = _instant(parking[0]["not_before"])
            # Fail closed on the real remaining margin: the window is claimed only
            # from the actual incident and parking deadlines, never from constants.
            require((until - datetime.now(timezone.utc)).total_seconds() >= 5,
                    "insufficient real margin before the original recovery deadline")
            started_at = datetime.now(timezone.utc).isoformat()
            self.configure(SCENARIO=self.scenario, HOLD_FRESH_RESPONSE=True)
            root = self.api("POST", "/api/control/agents/qa/prompt",
                            {"text": "QA_FRESH_COMPETE"})
            write_json(self.evidence / "fresh-root-prompt.json", root)
            self.wait_until(lambda: any(row.get("response") == "fresh_response_held"
                                        and row.get("side") == "b"
                                        for row in self.mock_records()), 15,
                            "healthy B holding the fresh root response")
            follow = self.api("POST", "/api/control/agents/qa/prompt",
                              {"text": "QA_FRESH_FOLLOW"})
            write_json(self.evidence / "fresh-follow-prompt.json", follow)
            queued = self.snapshot()
            write_json(self.evidence / "competition-queued.json", redact_evidence(queued))
            entry = _source_rows(queued, "queue_entries", follow["message_id"])
            require(len(entry) == 1 and entry[0].get("status") == "queued",
                    "fresh follow-up must queue while B holds the fresh root response")
            self.release(FRESH_GATE)
            self.configure(SCENARIO=self.scenario, HOLD_FRESH_RESPONSE=False)
            self.wait_completed("qa", cursor)
            competition = {
                "snapshot": self.snapshot(), "queued": queued, "b_incident": b_incident,
                "f0_id": root["message_id"], "f1_id": follow["message_id"],
                "started_at": started_at,
                "sampled_at": datetime.now(timezone.utc).isoformat(),
            }
            write_json(self.evidence / "competition.json", redact_evidence(competition))

            def recovered():
                captured = self.events("qa", cursor)
                require(not captured.get("has_newer"), "recovery events are truncated")
                return captured if len(_completed(captured["events"])) == 2 else None

            events = self.wait_until(recovered, self.args.timeout,
                                     "recovery after operator-rooted fresh competition")
        else:
            events = self.wait_completed("qa", cursor)
        self.docker("stop", "--time", "5", self.daemon)
        http, snapshot = self.mock_records(), self.snapshot()
        write_json(self.evidence / "events.json", redact_evidence(events))
        write_json(self.evidence / "http.json", http)
        write_json(self.evidence / "snapshot.json", redact_evidence(snapshot))
        if compete:
            return evaluate_recovery_interjection(events["events"], http, snapshot,
                                                  before, competition)
        return evaluate_recovery_cooldown(events["events"], http, snapshot, before)

    def run_r3773i(self):
        return self.run_r3773(compete=True)

    def run(self):
        status, error = None, None
        try:
            status = {
                "T3753": self.run_t3753, "I3754": self.run_i3754,
                "T3772": self.run_t3772, "R3773": self.run_r3773,
                "T3772I": self.run_t3772i, "R3773I": self.run_r3773i,
            }[self.case]()
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
