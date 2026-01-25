# Agent Serve Mode (Multi-turn Service) (Design Notes)

This document is **non-normative**. It proposes a container-internal scheme for running an agent as a **long-lived service** that can handle multiple turns (multi-round interaction) while still supporting a one-shot `run` mode.

Scope: this document only describes what happens **inside the agent container** (process model, filesystem layout, and the local IPC/API). Runner/host wiring (mounting the socket path, retries, authentication, etc.) is intentionally out of scope.

## Goals

- A single agent entrypoint (`agent.ts`) supports both:
  - **one-shot** execution (handle one request and exit), and
  - **serve** execution (wait for new requests and handle multiple turns).
- Multi-turn interaction is modeled as repeated **turn requests** within a session.
- Each turn has explicit **inputs**, **outputs**, and a replayable **event stream**.
- Artifact naming is **skill-defined**; the container entrypoint provides only minimal structure and isolation.

## Process model

The agent container runs a long-lived server process:

- PID 1: `agent.ts serve --socket /holon/ipc/agent.sock`
  - loads skills and config once
  - starts an HTTP server bound to a Unix domain socket
  - accepts turn requests in a loop

One-shot mode is a lifecycle policy, not a different implementation:

- `holon run` can start the same serve process, submit exactly one turn, then stop the container.

## Filesystem layout (inside the container)

The serve process uses these conventional paths:

- `/holon/ipc/agent.sock`
  - Unix socket created by `agent.ts serve`.
- `/holon/input/turns/<turn_id>/`
  - caller-provided inputs for a specific turn (files, references, context bundles).
- `/holon/output/turns/<turn_id>/`
  - all outputs for that turn (events, artifacts, and any skill-defined files).
- `/holon/state/`
  - optional persistent state across turns (e.g., saved Open Responses trajectories).

Notes:
- The agent MUST NOT assume it can change mounts at runtime. New inputs should arrive as new subdirectories under an already-mounted parent.
- To avoid cross-turn contamination, the agent SHOULD write only under the current turn’s output dir.

## API: HTTP over Unix socket

The serve process exposes a minimal API over the Unix socket.

### `GET /health`

Returns readiness + basic metadata (version, engine, enabled skills).

### `POST /responses` (Open Responses compatible)

The request and streaming response follow the **Open Responses** specification (see https://www.openresponses.org/), with Holon-specific fields carried via extensions.

Multi-turn continuity uses:
- `previous_response_id`: resume/extend a prior response without resending the full transcript.

Holon-specific extensions (one possible shape):

```json
{
  "model": "…",
  "input": [
    { "type": "input_text", "text": "…" }
  ],
  "previous_response_id": "resp_…",
  "holon": {
    "session_id": "sess_…",
    "turn_id": "turn_…",
    "input_dir": "/holon/input/turns/turn_…",
    "output_dir": "/holon/output/turns/turn_…"
  }
}
```

Response streaming:
- The server streams Open Responses events (NDJSON or SSE), including tool calls and incremental text deltas.
- The server SHOULD also write the same stream to `output_dir/events.ndjson` for replay/debugging.

### `POST /shutdown` (optional)

Requests a graceful shutdown. Useful for one-shot runs that start `serve` internally.

## Turn lifecycle (inside the container)

On each `POST /responses` request:

1. Determine `turn_id`, `input_dir`, `output_dir` (from request extensions or generated defaults).
2. Load continuation context:
   - If `previous_response_id` is set, load the corresponding stored trajectory from `/holon/state/` (or reconstruct the transcript if required by the underlying engine).
3. Execute the underlying engine/runtime headlessly:
   - allow skills/tools to be invoked as usual
   - stream Open Responses events back to the caller
4. Persist turn bookkeeping:
   - write `output_dir/turn.json` (recommended) containing at least `{session_id, turn_id, response_id, previous_response_id, timestamps}`
   - persist the response trajectory under `/holon/state/` so future turns can use `previous_response_id`
5. Finish the stream with a terminal “done” event and HTTP 200.

## Concurrency (recommended default)

For v1, the serve process SHOULD process turns **serially** (single in-flight request) to avoid:
- tool/runtime concurrency hazards,
- workspace races,
- unclear multi-turn ordering semantics.

If concurrency is added later, it SHOULD be constrained by:
- per-session locks (only one in-flight turn per `session_id`),
- explicit scheduling and cancellation semantics.

## Workspace mutation and isolation (optional, but recommended)

If a turn is allowed to modify a workspace, the serve process SHOULD isolate mutations per turn:

- create a per-turn working directory (e.g., a `git worktree` or copy) under a stable root
- run tools/engine against that per-turn directory
- write any skill-defined artifacts for the change set under `output_dir/`

This keeps a long-lived container from accumulating unintended state over many turns.

## Compatibility with existing one-shot agents

This design can be adopted incrementally:

- Keep the existing `agent.ts` one-shot entrypoint.
- Add `agent.ts serve` that uses the same internal “run one turn” function.
- Runners can continue using one-shot mode; `serve` becomes an opt-in capability for session-based applications.

