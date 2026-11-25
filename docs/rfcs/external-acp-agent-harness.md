---
title: "RFC: External ACP Agent Harness Integration"
date: 2026-11-25
status: draft
related_issue: 4531
---

# RFC: External ACP Agent Harness Integration

Handle: `rfc-external-acp-agent-harness`

## Summary

Holon should be able to start and supervise an external coding-agent harness
that speaks the Agent Client Protocol (ACP, wire v1) over a single `stdio`
stream, and expose it to callers as an ordinary bounded subagent.

The first target is Google Antigravity CLI (`agy acp`). The contract below is
deliberately vendor-neutral so the same client can later serve other
ACP-compatible harnesses, but only the Antigravity profile is normative here.

The integration lives in the agent execution-engine layer as a turn driver. It
does not become a model provider, does not add a second control-plane tool
surface, and does not change Holon's agent identity, delegation, or waiting
contracts. Those remain defined by
[Unified Agent Identity, Relations, And Message Delivery](./agent-identity-relations-and-message-delivery.md)
and
[Subagent Residency And Reclamation](./subagent-residency-and-reclamation.md).

## Status And Precedence

This RFC is a draft contract for ACP client integration. It is normative for
Holon-side semantics: layering, object mapping, capability policy, prompt
layering, event projection, permission decisions, and cancellation.

It is not normative for the ACP wire schema itself. Method names and payload
shapes follow upstream ACP wire v1; if upstream changes a field shape, the wire
shape follows upstream while the runtime semantics in this document still
apply. Unknown or future `session/update` variants are preserved as evidence
rather than rejected.

Scope is a local `stdio` child process bound to one managed execution root.
Remote HTTP or WebSocket ACP transport is out of scope. So is using ACP as a
general peer-to-peer orchestration protocol between Holon agents.

## 1. Layering And Responsibility Boundary

### 1.1 Turn drivers sit beside the provider, not inside it

| Layer | Component | Responsibility |
| --- | --- | --- |
| Control plane | Holon runtime tools | `CreateAgent`, `SendAgentMessage`, `InvokeAgent`, `WaitFor(wake=task_result)`, `TaskStop` |
| Execution engine | `NativeTurnDriver` | Drive one physical turn through `holon-provider` inference plus `holon-tools` dispatch |
| Execution engine | `AcpTurnDriver` | Spawn and supervise the harness process, handshake, session lifecycle, prompt dispatch, event projection, permission mediation, cancellation |
| Inference | `holon-provider` | One-step model inference contract: fully assembled prompt, supplied tool specs, per-`ToolUse` local dispatch |

`AcpTurnDriver` and `NativeTurnDriver` are peers. Both turn an admitted run into
physical turns and settle it through the same runtime surfaces. They differ only
in who owns the inner loop:

- `NativeTurnDriver` owns it: Holon assembles the prompt, supplies `ToolSpec`s,
  dispatches every managed tool call locally, and closes the round.
- The external harness owns it: the harness assembles its own base prompt, runs
  its own multi-step tool loop, persists its own session, and returns one final
  message per `session/prompt`.

### 1.2 Why ACP harnesses do not become `AgentProvider`

An ACP harness must not be modeled as an `AgentProvider` or a new
`ProviderTransportKind`:

- The provider contract assumes Holon owns the prompt and the tool loop. An ACP
  harness owns both; forcing it into the provider contract creates two competing
  tool loops and a double tool-execution surface.
- The provider contract assumes Holon can rebuild prompt prefixes for cache
  reuse and round accounting. An ACP harness owns its own context construction
  and compaction, so Holon cannot honor that contract on its behalf.
- The provider contract assumes Holon decides each round's closure. An ACP
  harness returns one terminal message per prompt; treating its internal steps
  as provider rounds would break `WaitFor` closure and task settlement.
- Consumer or CLI login state must stay behind an explicit process/ACP boundary
  rather than being mixed into an HTTP provider, following
  [`065-xai-grok-api-and-login-boundary`](../implementation-decisions/065-xai-grok-api-and-login-boundary.md).

The harness owns, and Holon must not duplicate or override:

- model selection and model-side parameters used inside the harness;
- internal retries, backoff, and failure handling inside the harness;
- internal context compaction and harness-side token budgeting;
- the harness's own tool definitions and their execution.

Holon does not wrap the harness in the `holon-provider` HTTP fallback chain.
Cross-profile or cross-model retry inside one physical turn is not a Holon
behavior. A harness-level failure surfaces as a structured `RuntimeError` on the
current task unless this RFC defines a session-level recovery (§2.4).

### 1.3 Orthogonality with Holon As An ACP Server (#768)

#768 is the dual direction: Holon exposing itself as an ACP Agent for editors.
This RFC is Holon acting as an ACP Client toward an external harness. They are
independent:

- neither side may assume the other exists or is reachable;
- sharing wire types or a protocol crate is allowed, but must not couple the two
  runtime surfaces;
- neither direction changes agent identity, queue, scheduling, or authority
  semantics.

### 1.4 No second control-plane surface

The harness is reached exclusively through existing runtime operations. A new
`AgentProvider`-style or ACP-specific tool family must not be added.

| Caller intent | Existing operation | ACP-side effect |
| --- | --- | --- |
| Create a bounded child backed by a harness | `CreateAgent` / `InvokeAgent(new_subagent)` with an ACP driver profile | No process or session until first work needs it |
| Ask for more work | `SendAgentMessage` or `InvokeAgent(existing_agent)` | One new physical turn, `session/prompt` |
| Wait for outcome | `WaitFor(wake=task_result)` | Task settles on turn or task completion, not on transport events |
| Stop work | `TaskStop` | `session/cancel` plus pending-permission resolution (§4.4) |
| Reclaim an idle child | Parent lifecycle operation | Process exit, identity and durable state retained |

Driver selection is configuration, not protocol. An agent template or runtime
profile selects `driver = acp` and a harness profile name; callers never address
a session or a process directly.

## 2. Object And Lifecycle Mapping

### 2.1 Objects

| Holon object | ACP object | Cardinality and ownership |
| --- | --- | --- |
| `AgentIdentity.agent_id` | none | ACP has no agent identity; the Holon agent is the only routing and audit key |
| `acp_session_state` | one ACP session (`sessionId`) | One per agent incarnation while the session is retained; owned by agent durable state |
| Physical Turn | one `session/prompt` request/response pair | Strictly one-to-one |
| `TaskKind::ActorInvocation` | none | One invocation task spans zero or more physical turns |
| Workspace binding / bound execution root | harness process `cwd` and sandbox scope | Bound at session creation; immutable for that session |
| `WorkItem` | none | Scheduling, memory, and continuation remain Holon-owned |

`acp_session_state` contains at least:

```text
acp_session_state
  profile                 # harness profile, e.g. antigravity
  resume_session_id?      # opaque, rotatable ACP session identifier
  bound_execution_root    # execution root or worktree the session was created in
  bound_additional_dirs   # attached workspace directories bound at session start
  capability_snapshot     # what this client advertised for this connection
  session_grants          # session-scoped permission grants (§4.3)
```

### 2.2 `agent_id` is not `acp_session_id`

This is a hard invariant:

1. `agent_id` is the canonical routing, audit, and authority key. It never
   changes when the ACP session changes.
2. `acp_session_id` is an opaque, rotatable resume credential. It is never a
   routing key, never a task or `WorkItem` identifier, and never a source of
   authority.
3. A new session may replace the credential without creating a new agent, and a
   deleted agent may discard the credential without notifying the harness.
4. ACP sessions are never shared between agents. Two agents never point at one
   session id.

### 2.3 One prompt is one physical turn, one session is not one turn

- One `session/prompt` round trip is one Holon physical turn. The harness may do
  many internal steps inside it; Holon sees one request and one final response.
- One ACP session is the continuation credential for one bounded subagent, not
  one turn. Reusing the session preserves the harness's exploration context,
  remote cache, session mode, and session-scoped grants across follow-ups,
  review-fix loops, and `elicitation` answers.
- One actor invocation task spans one or more physical turns. Task lifecycle is
  Holon state; it never depends on transport events alone.
- A one-shot delegation naturally degrades to a single-turn session with no
  extra retention cost.

Concurrency is serialized at the session: at most one in-flight
`session/prompt` per session. Additional admitted work waits in Holon's queue
and is not sent to the harness until the prior prompt settles.

### 2.4 Session lifecycle and state transitions

```text
create agent with driver=acp
            |
            v
     Uninitialized ---- initialize (handshake) ----> Ready
            |                                          |
            |                                    session/new
            |                                          v
            |                                      Active <-----+
            |                                       |  |         |
            |                                       |  +-- session/prompt round trip
            |                                       |            |
            |                                 turn settled, process resident
            |                                       v            |
            |                                     Idle ----------+
            |                                       |
            |                          Idle Retirement (process exit)
            |                                       v
            |                                   Retired
            |                                       |
            |                     next work: resume / drained load
            |                                       v
            |                                   Resuming
            |                                  /        \
            |                    resume/load ok    -32002 or unsupported
            |                              |                  |
            |                           Active        Rebuild (session/new)
            |
            +-- handshake or auth failure --> Failed (structured RuntimeError)
```

| Transition | Trigger | Guard | Effect |
| --- | --- | --- | --- |
| Uninitialized -> Ready | First admitted turn | Profile resolves to a launchable command | Spawn process, `initialize`, record capability snapshot |
| Ready -> Active | First prompt | Bound execution root exists and policy permits execution | `session/new`, persist `resume_session_id`, `session/prompt` |
| Active -> Active | Subsequent prompt | Session still bound to the same root and additional dirs | `session/prompt` on the existing session |
| Active -> Idle | Turn settled | No in-flight prompt, process still healthy | Keep process resident while reuse is expected |
| Idle -> Retired | Idle retirement policy | No owned execution, no active wake obligation | Exit process, keep identity and durable session state |
| Retired -> Resuming | New work arrives | Same agent incarnation and same bound workspace set | Spawn process, `initialize`, then attempt `session/resume` (or `session/load` if resume is undeclared) |
| Resuming -> Active | Resume or drained load accepted | Peer declared `session/resume`, or declared `loadSession` and replay stream was drained | Continue on the same logical session |
| Resuming -> Rebuild | `-32002` (unknown session) or neither `resume` nor `loadSession` declared (`UnsupportedByPeer`) | None | Discard credential, `session/new`, include bounded task-level handoff summary |
| Resuming -> Failed | Auth (`-32000`) or repeated transport failure | None | Structured `RuntimeError` with recovery hint; no silent model fallback |
| Active -> Failed | Process crash or protocol violation | None | Mark session retired after process reaping; next turn re-enters Resuming |
| Any -> Deleted | Agent deletion | Lifecycle fences satisfied | Terminate process, discard credential, retain only required evidence |

Rules for rebuild and recovery:

- `session/resume` is preferred over `session/load`. In ACP wire v1, `session/resume`
  restores the peer's persisted session without streaming historical updates,
  whereas `session/load` causes the peer to stream historical `session/update`
  notifications back to the client before returning.
- `session/load` is used only when `session/resume` is undeclared and `loadSession`
  is declared. While `session/load` runs (and before the next `session/prompt`
  starts), the client must drain and discard all replayed `session/update`
  notifications so historical blocks are never re-appended to Holon's ledger or
  mistaken for updates of the next turn.
- If the peer declares neither `session/resume` nor `loadSession`, or if resume/load
  returns `-32002` (session not found), the runtime treats it as an expected
  recovery signal rather than a task failure: it discards `resume_session_id`,
  calls `session/new`, and supplies bounded task-level handoff summary content
  (never the internal context envelope). A failure of that rebuild attempt
  surfaces as a structured turn error.
- Idle retirement follows
  [Subagent Residency And Reclamation](./subagent-residency-and-reclamation.md):
  the retired instance keeps identity, supervision, conversation, workspace, and
  artifacts; retirement never deletes the agent. A new invocation loads an
  instance, read-only inspection does not.
- Changing the agent's bound execution root or `attached_workspaces` set does not
  silently rebind an existing session. The session must be retired and rebuilt
  against the new workspace binding, or the runtime rejects the turn with a
  binding conflict.

## 3. Handshake Capability Trimming And Prompt Layering

### 3.1 Default capability set

The client advertises a deliberately minimal capability set during
`initialize`. Omission means unsupported; the client must never advertise a
capability whose handler is absent.

| Capability | Default | Later opt-in | Boundary |
| --- | --- | --- | --- |
| `fs.readTextFile` / `fs.writeTextFile` | Not declared | No | The harness executes file tools natively inside the bound root; a second file path through Holon would bypass execution policy and create two mutation surfaces |
| `terminal` | Not declared | No | Avoids double process execution, and avoids reliance on the client terminal surface that ACP v2 is retiring |
| `plan` | Not declared | Optional | Observation only: plan updates are recorded as evidence, never executed by Holon |
| `subagents` | Not declared | Optional, read-only | Progress observation only; grants no cross-protocol delegation, messaging, or authority |
| `elicitation.form` | Not declared | Optional | Only when the parent execution can actually wait for an answer; answers are turn-scoped input, not operator authority |

Capability rules:

- The snapshot is fixed for the life of the process connection. Changing it
  requires a new connection, and therefore a session rebuild.
- The client must not infer a capability from a successful method call; it
  decides from its own advertised set.
- Because `fs.*` and `terminal` are not declared, the harness must use its own
  native tools. That is the intended single execution surface.
- Optional capabilities are enabled per profile only after the corresponding
  projection and waiting behavior exists. The default deployment stays minimal.

### 3.2 Prompt layering

| Content | Send to harness? | Boundary |
| --- | --- | --- |
| Clean task handoff: objective, acceptance, constraints, workspace boundary | Yes, first prompt of the session | Task-level content only; no runtime framing |
| Incremental follow-up messages in the same session | Yes | Normal continuation, still task-level |
| Holon internal context envelope, memory projections, runtime refs | Never | Leaks internal provenance/framing and duplicates harness assembly |
| Holon tool-use prompt or `ToolSpec` text | Never | The harness owns its own tool loop and prompt |
| Workspace `AGENTS.md` contents | Never re-injected | `agy acp` loads workspace instructions from `cwd` natively; re-injecting causes drift and duplication |
| Persona or role instructions | Yes, through negotiated channels only | See §3.3 |
| Prior transcript replay | Only after session loss, and only task-level content | Never internal framing; never a second copy of the same handoff |

Interim user-visible text in the task handoff must be the same content Holon
would give a native subagent, expressed without internal refs. Paths in prompts
are execution-host absolute paths inside the bound root.

### 3.3 Persona injection precedence

Choose at most one dedicated persona channel:

1. Negotiated `systemPrompt (append)` over the wire (`session/new` /
   `session/resume`), when the peer declares
   `agentCapabilities.sessionCapabilities.systemPrompt` with append semantics
   verified not to replace the harness base prompt. This is preferred for
   runtime-supplied role instructions because it creates no filesystem or Git
   worktree side effects.
2. Pre-existing `--agent <role>`, when the profile or template references a named
   agent already configured in the workspace or on the host. The runtime must
   never materialize temporary `agent.md` files inside a target Git workspace or
   managed worktree (which would pollute `detect_changed_files`, block clean
   `RemoveWorktree`, or risk accidental commits) and must never mutate
   host-global user directories such as `~/.gemini/config/agents/`.
3. Neither available: express role expectations as ordinary handoff prose in the
   first prompt. Do not fall back to sending a Holon system prompt.

Never combine 1 and 2 for the same session. Never send a full replacement system
prompt.

## 4. Event Projection, Permission Gating, And Cancellation

### 4.1 `session/update` projection

ACP streams `session/update` notifications while a prompt is in flight. They are
projected into the append-only ledger and, where applicable, into model blocks.
They never trigger local tool dispatch.

| ACP `session/update` variant | Model projection | Ledger | Brief | Local dispatch |
| --- | --- | --- | --- | --- |
| Agent message chunk | Accumulated as one canonical `ModelBlock::Text` per physical turn at settlement; raw chunks also recorded | Append | Final text feeds `BriefRecord` | No |
| Agent thought / reasoning chunk | `ModelBlock::ReasoningText` | Append | Never | No |
| Tool call | `ModelBlock::ProviderToolUse` with `provider_data.format = "acp_v1"` | Append | No | Never |
| Tool call update | `ModelBlock::ProviderToolResult` for terminal status, or a status update on the existing provider tool block | Append, referencing the same `tool_call_id` | No | Never |
| Plan update | No model block; typed plan snapshot evidence | Append | No | No |
| Usage update (`usage_update`) | No model block; session context-window occupancy (`used` / `size`) and optional cost evidence | Append | No | No |
| Available commands update | No model block; capability evidence | Append | No | No |
| Unknown or future variant | No model block; opaque evidence carrying `format = "acp_v1"` | Append | No | No |

Projection rules:

- The ledger is append-only. An update produces a new entry; it must not rewrite
  an earlier block.
- `ReasoningText` is never rendered as user-visible assistant text and is never
  re-sent as prompt content.
- `ProviderToolUse` and `ProviderToolResult` are audit and verbose-display
  evidence. They must never be dispatched to `holon-tools` or counted as managed
  tool calls in Holon round accounting.
- Per-turn block order follows arrival order. Settlement finalizes exactly one
  canonical `Text` block for the turn, which is what replay and briefing use.
- `usage_update` reports session-level context-window occupancy (`used` / `size`)
  and optional cumulative `cost`; it is recorded as context/cost telemetry and
  must not be summed as per-turn `input_tokens` / `output_tokens` deltas. Per-turn
  token counts are recorded only when explicitly reported by the peer; absent
  counts remain absent (`None`), never flattened to zero.
- ACP `stopReason` maps to existing settlement categories: normal completion,
  truncation evidence, refusal evidence, cancellation, or structured runtime
  error.
- Final answer text enters the standard result path: `BriefRecord` plus task
  result, subject to
  [Result Closure](./result-closure.md) and
  [Runtime Object And Reference Model](./runtime-object-and-reference-model.md).
  Intermediate messages do not create briefs.

### 4.2 Permission requests are policy decisions, not harness authority

`session/request_permission` is the harness asking Holon for approval of a tool
call it wants to execute itself. Holon is the deciding client. The request is
evidence, never authority: harness-provided titles or arguments never justify
approval on their own.

Decision inputs:

- the session's bound execution root or worktree and the resulting
  `EffectiveExecution` boundary;
- Holon execution policy for the requesting agent;
- the agent's authority and the current task's scope;
- active session grants;
- whether the parent execution can actually wait for operator input.

### 4.3 Decision model and scopes

| Situation | Decision | Scope |
| --- | --- | --- |
| Target paths and commands are inside the bound `EffectiveExecution` and policy allows the tool kind | `allow_once` (default) | Single request only |
| Same tool kind and path scope repeat within the session, and the profile permits pre-approval | `allow_always` | Session-scoped grant, stored in `acp_session_state.session_grants` |
| Request leaves the bound root, crosses into an unapproved root, or escalates process/network/secret scope | `reject_once` plus structured evidence | Does not fail the session by itself |
| Request is destructive or irreversible and policy requires confirmation | Deny unless an operator approval path is available and the parent execution can wait | Approval is turn-scoped |
| No matching option exists in the request | Cancel the request | Never leave it pending |

Grant rules:

- `allow_always` is scoped to one session and one tool-kind/path scope. It is
  revoked on rebuild, retirement-to-new-session, or agent deletion, and is never
  transferred to another session or agent.
- Grants are recorded in the ledger. A grant never overrides a later policy
  change; policy is re-checked per request.
- A denied request must produce actionable evidence. If the turn cannot proceed,
  it settles as a structured `RuntimeError`, not a silent hang.

### 4.4 Cancellation and deadlock avoidance

Cancellation must never deadlock on a permission request that the harness is
waiting for while Holon is also waiting for the same harness.

On `TaskStop` or any cancellation of the current turn:

1. Persist the cancellation intent on the task before touching transport state.
2. Immediately resolve every pending `session/request_permission` for that
   session with `cancelled` (or the protocol's reject option), and stop issuing
   new permission decisions for that turn.
3. Send `session/cancel` for the in-flight prompt.
4. Wait for the harness to flush in-flight state, bounded by a profile timeout.
   Do not block on further permission requests during this window.
5. Settle the task as cancelled with evidence. Late updates that arrive after
   settlement are appended as post-cancel evidence and never reopen the turn.
6. If the harness does not exit or acknowledge within the timeout, terminate its
   process group, mark the session retired, and rebuild on the next turn.

Additional rules:

- Cancellation is idempotent; repeated stops do not re-send unbounded cancels.
- A cancelled turn still records the partial assistant text and any tool evidence
  it produced before cancellation.
- Deleting the agent while a turn is in flight uses the same cancel sequence,
  followed by process reaping and credential discard.

### 4.5 Antigravity (`agy acp`) first profile

| Field | Contract |
| --- | --- |
| Launch | `agy acp` over `stdio`, one process per session |
| Role injection | Negotiated `systemPrompt (append)` for runtime-supplied role text; `--agent <role>` only for pre-existing named agents (§3.3); never write temporary `agent.md` files into target worktrees or `~/.gemini/config` |
| Working directory | The bound execution root or worktree; never the Holon process cwd by default |
| Additional directories | Map `EffectiveExecution.attached_workspaces` to repeated `--add-dir <path>` flags; changing the attached set requires retiring and rebuilding the session (§2.4) |
| Sandbox | Map execution policy to the profile's `--sandbox` mode; when no sandbox mode is available, the runtime must not claim confinement |
| Authentication | Reuse the host's existing `agy acp --login` state or explicitly configured/allowlisted environment credentials (`GEMINI_API_KEY`, `GOOGLE_API_KEY`, and for Vertex/Enterprise `agent-platform` ADC: `GOOGLE_APPLICATION_CREDENTIALS`, `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, `AGY_ADC_AUTH`) |
| Credential files | Never parse, copy, or scrape private harness session files |
| `-32000` authentication required | Structured `RuntimeError` with a recovery hint (log in or provide credentials); no retry loop, no fallback model |
| `-32002` session not found | Rebuild the session per §2.4 |
| Capabilities | Default trimmed set per §3.1; no `fs.*`, no `terminal` |
| Unsupported request | Reject with evidence; never emulate an unsupported client capability |

## 5. Non-Goals

- No Rust runtime, protocol, or `crates/**` change is defined by this RFC; the
  implementing units are tracked separately under #4530.
- No remote HTTP or WebSocket ACP transport.
- No use of ACP as a general peer-to-peer orchestration protocol between Holon
  agents.
- No new agent identity, delegation, waiting, or control-plane tool semantics.
- No attempt to make Holon mirror or re-implement harness-internal model choice,
  retry, or compaction.

## 6. Acceptance

- Layering is explicit: `AcpTurnDriver` is an execution-engine turn driver,
  parallel to `NativeTurnDriver`, and is not an `AgentProvider`.
- Every object relation and state transition in §2 is unambiguous, including
  `agent_id != acp_session_id`, resume-before-load, `-32002` rebuild, idle
  retirement, and workspace rebinding.
- Capability trimming and prompt layering are testable statements, including all
  three "never send" rules and the persona precedence order.
- The `session/update` projection matrix, permission decision table, and
  cancellation sequence cover normal, denied, cancelled, crashed, and
  authentication-failed paths.
- Terminology matches
  [Agent Identity, Relations, And Message Delivery](./agent-identity-relations-and-message-delivery.md)
  and
  [Subagent Residency And Reclamation](./subagent-residency-and-reclamation.md).

## Related Documents

- [Agent Identity, Relations, And Message Delivery](./agent-identity-relations-and-message-delivery.md)
- [Subagent Residency And Reclamation](./subagent-residency-and-reclamation.md)
- [Execution Policy And Virtual Execution Boundary](./execution-policy-and-virtual-execution-boundary.md)
- [Agent Delegation Tool Plane](./agent-delegation-tool-plane.md)
- [Result Closure](./result-closure.md)
- [Runtime Object And Reference Model](./runtime-object-and-reference-model.md)
- [Workspace Binding And Execution Roots](./workspace-binding-and-execution-roots.md)
- [External Login And Process Boundary Decision](../implementation-decisions/065-xai-grok-api-and-login-boundary.md)
- [Tracker #4530](https://github.com/holon-run/holon/issues/4530)
- [Holon As ACP Server (#768)](https://github.com/holon-run/holon/issues/768)
