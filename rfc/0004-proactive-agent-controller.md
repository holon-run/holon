# RFC-0004: Proactive Agent Runtime (Gateway + Controller)

| Metadata | Value |
| :--- | :--- |
| **Status** | **Draft** |
| **Author** | Holon Contributors |
| **Created** | 2026-02-08 |
| **Updated** | 2026-02-08 |
| **Parent** | RFC-0001, RFC-0002, RFC-0003 |
| **Issue** | [#568](https://github.com/holon-run/holon/issues/568) |

## 1. Summary

This RFC proposes evolving Holon from a one-shot execution tool into a **proactive agent runtime** with a long-running control loop that continuously reacts to events and drives project progress.

The runtime has two first-class modules inside one service:

- **Gateway module**: event ingress and distribution (transport and normalization)
- **Controller module**: agentic decision and task orchestration

Controller consumes normalized events (GitHub, timer, and future connectors), reasons over current state, and chooses follow-up actions (solve, review, fix, merge, comment, ask for clarification).

This RFC keeps existing `holon run` behavior and introduces a new always-on mode as an additive capability.

## 2. Motivation

Current strengths of Holon (`issue -> PR`, `pr-fix`) are useful but reactive. The next product step is making the system **self-driven**:

- monitor repository activity continuously,
- decide what to do next without manual trigger comments,
- push work forward until completion.

Primary product value for this phase is **agent proactivity** (not deterministic artifact strictness).

## 3. Goals and Non-Goals

### 3.1 Goals

1. Add a long-running controller process that can autonomously drive workflows.
2. Add a generic event ingress layer that supports multiple connectors, not only GitHub.
3. Support event-driven behavior from both external events and periodic ticks.
4. Reuse existing skills (`github-issue-solve`, `github-review`, `github-pr-fix`) as action executors.
5. Keep setup simple for both:
   - users with Holon GitHub App installed,
   - users without app installation privileges.
6. Preserve compatibility with existing one-shot CLI and workflow usage.

### 3.2 Non-Goals (initial phase)

1. Full workflow-engine semantics (DAG authoring, enterprise RBAC, complex approvals).
2. Strong replay/audit platform requirements as the core value proposition.
3. Replacing existing `holon run` execution contract.

## 4. Design Principles

1. **Proactive-first**: optimize for autonomous progression and reduced manual babysitting.
2. **Single-service runtime**: gateway/controller are internal modules in one process.
3. **Agentic control loop**: controller decides actions; rules provide minimal boundaries.
4. **Skill-first actions**: domain behavior remains in skills; Holon provides runtime and event plumbing.
5. **Incremental adoption**: local setup first, hosted gateway second.
6. **Compatibility by default**: existing triggers continue to work.

## 5. Proposed Architecture

```
Connectors (GitHub / Timer / Webhook / Queue / Chat)
                    |
                    v
   Holon Serve (gateway + controller + executor)
   - ingest + normalize + route
   - observe -> agent decide -> act
   - dispatch action runs
                    |
                    v
      Side effects via skills / APIs
```

### 5.1 Components

1. **Connector**
   - Source-specific adapters that convert external inputs to `EventEnvelope`.
   - Examples: GitHub webhook, GitHub poll, timer tick, Telegram update, custom webhook.

2. **Gateway**
   - Receives connector events, validates basic metadata, normalizes into a common envelope.
   - Supports delivery to controller via local queue or stream.
   - Mode A: local forwarding (`gh webhook forward`, `smee`, tunnel).
   - Mode B: hosted gateway (webhook ingress + stream/replay).

3. **Controller Core**
   - Long-lived module in `holon serve`.
   - Hosts a long-running controller-agent session.
   - Maintains lightweight working state (last seen event, in-flight tasks, basic dedupe keys).
   - Runs the control loop: observe, agent decide, act.

4. **Controller Agent (Agent-first)**
   - Controller forwards events directly to a full-capability agent session.
   - Agent autonomously decides whether to:
     - handle immediately (comment/reply/wait),
     - trigger worker runs (`holon run --skill ...` / `holon solve`),
     - perform repository actions (merge/close) when policy allows.
   - Example action outputs: `run_skill`, `run_solve`, `comment`, `merge_pr`, `close_issue`, `wait`.

5. **Action Executor**
   - Converts decision outputs into concrete `holon run` invocations (or direct API calls when needed).
   - Executes actions with existing skills and runtime isolation.

## 6. Event Model

### 6.1 Event Types (initial)

- `github.issue.opened`
- `github.issue.comment.created`
- `github.pull_request.opened`
- `github.pull_request.synchronize`
- `github.pull_request_review.submitted`
- `github.check_suite.completed`
- `timer.tick`

### 6.2 Internal Envelope (conceptual)

```json
{
  "id": "evt_...",
  "source": "github|timer|webhook|chat|queue",
  "type": "github.pull_request.opened",
  "scope": {
    "tenant": "default",
    "repo": "owner/repo",
    "session": "optional"
  },
  "at": "2026-02-08T00:00:00Z",
  "subject": {
    "kind": "pull_request",
    "id": "123"
  },
  "dedupe_key": "github:owner/repo:pr:123:opened:sha",
  "payload": {}
}
```

The controller consumes the envelope and decides action(s).

### 6.3 Why this model is generic

- `type` and `source` are open sets.
- `scope` allows repository, project, or chat-session level isolation.
- `subject` gives the controller a stable entity to reason about.
- `dedupe_key` allows connector-specific duplicate suppression without hard-coding GitHub logic in controller.

## 7. Control Loop Semantics

Each loop iteration:

1. Pull next event(s)
2. Build minimal working context
3. Ask controller-agent for a decision
4. Execute decision
5. Record result and continue

Decision outputs should be lightweight and action-oriented. In this phase, strict schema is recommended for action reliability while keeping the action vocabulary small.

### 7.1 Action Intent (conceptual)

```json
{
  "id": "act_...",
  "type": "run_skill|comment|merge|wait",
  "target": {
    "repo": "owner/repo",
    "kind": "pull_request",
    "id": "123"
  },
  "args": {
    "skill": "github-review"
  },
  "priority": "normal",
  "idempotency_key": "..."
}
```

Controller outputs `ActionIntent`; executor performs side effects.

### 7.2 Agent-first behavior

The controller does not hardcode business flows such as:

- `PR opened => always review`
- `review changes requested => always pr-fix`

Instead, events are fed to the controller-agent, and the agent chooses the next action based on current context and recent outcomes.

## 8. Output Model (Controller + Worker Runs)

To keep output predictable while allowing skill-defined artifacts, this RFC defines a two-level model:

### 8.1 Controller-level unified logs

Controller writes stable logs under its run/state directory:

- `events.ndjson`: normalized incoming events
- `decisions.ndjson`: controller-agent decisions (structured actions)
- `actions.ndjson`: execution results for each action

These logs are the unified output of the proactive loop.

### 8.2 Worker-run artifacts remain skill-defined

When controller triggers `holon run`/`holon solve`, each worker run continues to produce its own skill artifacts (manifest, patch, summary, and others).

Controller does not reinterpret worker artifacts. It records references in `actions.ndjson`, such as:

- `run_id`
- `run_output_path`
- `manifest_path`
- `status`

## 9. Execution Strategy and Skills

### 8.1 Reuse Existing Skills

- `github-issue-solve`
- `github-review`
- `github-pr-fix`
- `github-context`
- `github-publish`

### 8.2 Typical Action Mapping

- New/updated issue requiring implementation -> `github-issue-solve`
- PR opened/synchronized -> `github-review`
- Review requests changes or CI fails -> `github-pr-fix`
- Ready state reached -> merge/comment action

The controller-agent chooses *when* to call these skills; skills keep defining *how* work is executed.

## 10. Deployment Modes

### 9.1 Mode A: Local Forwarding (MVP)

- User runs controller locally.
- Events forwarded from GitHub to local endpoint via CLI/tunnel tools.
- Best for fast validation and low setup overhead.

### 9.2 Mode B: Hosted Gateway

- Holon-hosted webhook receiver + stream/replay endpoint.
- Controller subscribes over WS/SSE and optionally resumes from `since_id`.
- Best for reliability and always-on operation.

Both modes remain supported long-term.

## 11. API/CLI Surface (Draft)

Primary command:

- `holon serve`

Subcommands:

- `holon serve status`
- `holon serve pause`
- `holon serve resume`

Possible flags:

- `--repo owner/repo`
- `--event-source local|gateway`
- `--gateway-url ...`
- `--tick-interval 60s`
- `--policy skill://controller-policy`

## 12. Rollout Plan

### Phase 1 (MVP, GitHub-first)

1. Add `holon serve` with local forwarding event input.
2. Add minimal local gateway module (normalization + envelope generation).
3. Add controller-agent session and structured `ActionIntent` execution.
4. Persist minimal serve state locally.
5. Emit controller logs (`events/decisions/actions.ndjson`).
6. Dogfood on `holon-run/holon`.

### Phase 2 (Hosted ingress + replay)

1. Add hosted ingress integration (`webhook -> stream`) for `holon serve`.
2. Add reconnect + replay (`since_id`) support.
3. Add auth model for App-installed and non-App scenarios.

### Phase 3 (Generalized connectors + productization)

1. Add non-GitHub connectors (timer/webhook/chat/queue).
2. Multi-repo orchestration.
3. Richer policy skills (PM role, milestone driving, prioritization).
4. Improve serve operability, docs, examples, and dogfood metrics.

### Phase/Epic Mapping

- Phase 1 -> Epic A + Epic B + Epic C
- Phase 2 -> Epic D
- Phase 3 -> Epic E

## 13. Risks and Mitigations

1. **Over-triggering / noisy actions**
   - Mitigation: per-PR/issue cooldown windows and lightweight dedupe keys.

2. **Decision quality variance**
   - Mitigation: provide clear controller-goal prompt and a bounded action vocabulary.

3. **Operational complexity**
   - Mitigation: keep local mode first and optional; hosted gateway as additive.

## 14. Open Questions

1. Should controller decisions be strict JSON only, or JSON + rationale text?
2. Which merge actions remain fully autonomous vs requiring explicit policy approval?
3. What is the minimum persistent state required for practical reliability?
4. Should controller invoke one action per event or allow small action batches?
5. Should controller-agent run as a single global session or per-scope session (repo/pr/issue)?
6. Should we keep only `holon serve`, or also expose advanced internal subcommands for debugging?

## 15. References

- RFC-0001: `rfc/0001-holon-atomic-execution-unit.md`
- RFC-0002: `rfc/0002-agent-scheme.md`
- RFC-0003: `rfc/0003-skill-artifact-architecture.md`
- Dual ingress issue: [#568](https://github.com/holon-run/holon/issues/568)

## 16. Task Breakdown

### Epic A: Core Runtime Split (Gateway + Controller)

1. Define `EventEnvelope` and `ActionIntent` Go types and docs.
2. Implement gateway normalization interface and in-process transport.
3. Implement controller loop (`observe -> agent decide -> act`) skeleton.
4. Add minimal persistent serve state (`last_event`, `inflight`, `dedupe`).
5. Add controller unified logs (`events.ndjson`, `decisions.ndjson`, `actions.ndjson`).

### Epic B: GitHub-first MVP

1. Add GitHub connector adapter for local forwarding input.
2. Feed GitHub events directly into controller-agent session.
3. Let controller-agent choose when to call existing skills (`github-review`, `github-pr-fix`, `github-issue-solve`).
4. Add guardrails for duplicate review/fix triggers in PR pipelines.

### Epic C: Controller-Agent Layer

1. Define controller-agent system prompt and event/action contract.
2. Implement bounded action vocabulary and execution validation.
3. Add policy hooks for merge/comment/escalation decisions.
4. Add action-result feedback loop back into controller-agent context.

### Epic D: Hosted Ingress for `holon serve`

1. Define hosted gateway API (webhook ingest + stream + replay).
2. Implement `since_id` catch-up protocol.
3. Add auth model for App-installed and non-App scenarios.

### Epic E: Operability and Productization

1. Add non-GitHub connectors (timer/webhook/chat/queue) in serve pipeline.
2. Expand to multi-repo orchestration and richer policy skills (PM/milestone/prioritization).
3. Add CLI operability UX around `holon serve` status/pause/resume.
4. Add docs and examples for local mode and hosted mode.
5. Add dogfood workflow and success metrics for autonomous progression.
