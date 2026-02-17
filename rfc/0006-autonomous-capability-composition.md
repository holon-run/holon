# RFC-0006: Autonomous Capability Composition Model (Event Source + Role + Skill)

| Metadata | Value |
| :--- | :--- |
| **Status** | **Draft** |
| **Author** | Holon Contributors |
| **Created** | 2026-02-10 |
| **Updated** | 2026-02-17 |
| **Parent** | RFC-0001, RFC-0002, RFC-0003, RFC-0004, RFC-0005 |
| **Issue** | TBD |

## 1. Summary

This RFC repositions Holon from a "project delivery automation tool" to an **autonomous capability platform**.

## Implementation Reality (2026-02-17)

- This RFC captures target composition direction.
- Current implementation is still converging on these concepts; use it as roadmap guidance rather than an exact runtime contract.

Core model:

`event source + role + skills => autonomous instance`

The same runtime can be composed into different agents (PM, Reviewer, Maintainer, SRE, Release Manager, etc.) by changing composition inputs rather than changing the runtime core.

Initial deployment model (instance-first):

- `1 holon serve process = 1 autonomous instance = 1 role`
- Multi-role support is a future extension via multi-instance orchestration.

## 2. Motivation

Current framing ("continuously move project delivery forward") is valid but too narrow.

As `holon serve` becomes a persistent control plane, its value is broader:

1. Event-driven autonomous operations beyond software delivery workflows.
2. Reusable role instances with clear behavior boundaries.
3. Pluggable capability composition without coupling to one provider or one workflow.

## 3. Goals and Non-Goals

### 3.1 Goals

1. Define a first-class composition model for autonomous roles.
2. Keep `holon run` as the stable execution kernel.
3. Make `holon serve` the persistent control plane for event-driven autonomy.
4. Standardize contracts for event, role decision output, and skill invocation lifecycle.
5. Enable capability reuse across domains by recomposition.

### 3.2 Non-Goals

1. Not a full BPM/workflow engine replacement.
2. Not a requirement to support every connector in initial rollout.
3. Not a hard commitment to one fixed role taxonomy.
4. Not replacing one-shot `holon run` and existing solve flows.

## 4. Product Model (Three Planes)

Holon is split into three planes with explicit boundaries.

### 4.1 Execution Plane (`holon run`)

- Sandbox execution
- Deterministic input/output contract
- Artifact and isolation guarantees

### 4.2 Capability Plane (Role + Skill)

- Role prompt defines mission, decision style, and boundaries.
- Skills define executable capabilities and side effects.
- Skills remain composable and independently evolvable.

Role semantics are unified:

- `role` in `holon run` and `holon serve` is the same concept.
- Difference is runtime mode only: one-shot (`run`) vs persistent (`serve`).

### 4.3 Control Plane (`holon serve`)

- Persistent event ingestion and dispatch
- Decision loop orchestration
- Runtime state, lifecycle control, and observability

## 5. Composition Contract (Normative Direction)

Each autonomous instance is minimally defined by:

1. `role`: role identity and behavioral prompt contract
2. `ingress`: zero or more event sources (GitHub/webhook/timer/queue/etc.)
3. `skills`: initialized skill set for this instance

Serve runtime model for this phase:

- Exactly one instance per `holon serve` process.
- Exactly one role per instance.
- Each role has isolated memory storage (runtime default).

### 5.1 Example (Conceptual)

```yaml
apiVersion: holon.dev/v2alpha1
kind: Instance
metadata:
  name: project-maintainer
spec:
  role:
    promptRef: roles/pm-maintainer.md
    mode: full
  ingress:
    - source: github.webhook
      repo: holon-run/holon
      events: [issues, issue_comment, pull_request, pull_request_review]
  skills:
    - project-pulse
    - github-issue-solve
    - github-review
    - github-pr-fix
```

### 5.2 Run vs Instance Relationship

`run` and `instance` share the same role/skill execution semantics.

Primary difference is trigger and lifecycle:

- `run`: one-shot execution with explicit single input.
- `instance`: persistent serve runtime that consumes events continuously.

If `ingress` is empty, an instance can be treated as a manual one-shot event consumer (run-like behavior).

## 6. Contract Surfaces To Standardize First

This RFC prioritizes contract-first evolution over feature-first evolution.

### 6.1 Event Contract

Required baseline fields:

- `id`, `source`, `type`, `scope`, `at`, `subject`, `dedupe_key`, `payload`

Required guarantees:

- at-least-once delivery tolerance
- idempotency key handling
- replay-safe cursoring

### 6.2 Role Decision Contract

Role output should be structured, small vocabulary, auditable:

- `no_op`, `invoke_skill`, `comment`, `merge`, `close`, `escalate`, `wait`

Each decision should include:

- `reason`, `target`, `priority`, `idempotency_key`, optional `requires_approval`

### 6.3 Skill Invocation Contract

Standardize:

- invocation context envelope
- expected output artifacts
- success/failure/retry semantics
- sync vs async execution mode

### 6.4 Constraints (Runtime Layering)

Constraints are not a separate composable schema module. They are enforced across runtime layers:

1. System design layer (hard constraints):
   - isolation boundaries
   - non-bypassable permissions
   - hard concurrency/rate/time limits
2. Configuration layer (instance constraints):
   - action allowlist/denylist
   - risk tier and approval thresholds
   - retry/rollback controls
3. Prompt layer (soft constraints):
   - decision guidance and escalation preference
   - conservative behavior hints

Precedence (required):

`system hard constraints > config constraints > prompt guidance`

### 6.5 Memory Contract

Memory is role-scoped in v2 baseline (runtime default):

- one isolated persistent store per role instance
- no shared cross-role mutable memory in the baseline
- optional cross-role data sharing should be explicit via artifacts/events, not implicit shared memory

## 7. Runtime Semantics

The control loop remains:

1. Ingest event
2. Normalize + dedupe
3. Ask role for decision
4. Enforce constraints
5. Execute skills/actions
6. Persist state and emit logs

Controller logs stay as first-class unified outputs:

- `events.ndjson`
- `decisions.ndjson`
- `actions.ndjson`

## 8. API Direction Alignment

This RFC aligns with RFC-0005:

1. Control plane: JSON-RPC subset, then expand.
2. Ingress: provider-specific paths first (`/ingress/<provider>/webhook`).
3. Generic `/v1/events`: deferred until multi-connector validation.

## 9. Rollout Plan (Capability-First)

### Phase A: Contract Baseline

1. Freeze minimal event + decision envelopes.
2. Publish minimal instance schema draft (`role + ingress + skills`).
3. Add compatibility tests for contracts.

### Phase B: Single-Role Production Loop

1. Ship one stable role (project-maintainer/PM).
2. Validate long-running reliability and governance controls.
3. Track autonomy KPIs (decision success rate, duplicate suppression, rollback rate).

### Phase C: Multi-Role and Multi-Connector Expansion

1. Add second role class (e.g., reviewer or release manager).
2. Add second connector type.
3. Revisit generic events API only after validation.

## 10. Success Criteria

Holon v2 platform direction is considered successful when:

1. One role instance can run persistently with bounded risk and clear audit trail.
2. A second role can be launched mostly by composition changes (not runtime rewrites).
3. Skills are reused across roles without role-specific forks.
4. Runtime constraints can prevent unsafe actions without breaking autonomy loop.
5. Role memory is isolated by default and remains replay-safe across restarts.

## 11. Open Questions

1. Should role prompts be versioned as first-class registry objects?
2. Should runtime constraints be statically loaded only, or dynamically adjustable at runtime?
3. What minimum approval model is required for critical actions in v2?
4. How much of JSON-RPC method surface should map to Codex-compatible names vs `holon/*` namespace?
