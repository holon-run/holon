# Holon Implementation Decisions

This document records implementation decisions made while carrying `Holon`
through roadmap milestones M0-M6.

The goal is to preserve the reasoning behind concrete choices so later reviews
can focus on whether the choice should change, not on reconstructing why it was
made.

## 1. Anthropic Compatibility Strategy

Decision:

- use `ANTHROPIC_AUTH_TOKEN` with `Authorization: Bearer ...`
- use `ANTHROPIC_BASE_URL` as the runtime endpoint
- always send `anthropic-version: 2023-06-01`

Reason:

- the local environment already provides those values in `~/.claude/settings.json`
- the configured endpoint is an Anthropic-compatible proxy, not necessarily the
  official Anthropic host
- this keeps live integration tests aligned with the real local setup

## 2. Context V1 And Compaction

Decision:

- keep a durable append-only `messages.jsonl` and `briefs.jsonl`
- build model-visible context from:
  - structured session working memory
  - durable episode memory for older work chunks
  - short session-memory delta
  - legacy compacted summary as fallback during migration
  - recent message window
  - recent brief window
  - current input
- make session-memory derivation, episode extraction, and compaction
  deterministic and local, not model-generated

Reason:

- deterministic memory derivation is easier to test and audit
- the first durable long-agent memory layer should preserve runtime evidence and
  revision boundaries before it tries to optimize quality
- immutable episode records are a better long-tail artifact than repeatedly
  rewriting one large summary blob
- durable history and model-visible context are different concerns

## 3. Background Task V1

Decision:

- implement the first real task primitive as an in-process sleep job
- route task lifecycle back through normal `task_status` and `task_result`
  queue events

Reason:

- this proves the runtime contract without committing to a distributed worker
  design too early
- the key property is not “remote execution”; it is “background work rejoins the
  same agent loop”

## 4. Policy Boundary V1

Decision:

- preserve a stable provenance/admission contract on runtime messages:
  - `origin`
  - `trust`
  - `delivery_surface`
  - `admission_context`
- keep `default_trust_for_origin` and origin/kind validation as the phase-1
  marking contract
- remove the early trust-based `can_create_task`, `can_create_timer`, and
  `can_control_agent` default gates from control surfaces
- record non-message control operations through audit events instead of forcing
  them into the message envelope

Reason:

- #47 was meant to freeze provenance and admission vocabulary, not a premature
  allow/deny matrix
- transport admission and runtime authority are different concerns
- early trust-based action gates would likely be reworked once execution policy
  and resource authority are designed together
- explicit audit provenance is enough for phase 1 without inventing hidden
  message traffic for control-plane mutations

## 4.1 Execution Policy Surface

Decision:

- treat `host_local` as the only implemented execution backend in phase 1
- expose execution policy through a derived capability snapshot instead of
  claiming a strong sandbox
- consolidate host-local enforcement through shared runtime helpers instead of
  per-call ad hoc checks
- gate only the surfaces Holon can honestly control today:
  - process execution exposure
  - background task availability
  - managed worktree availability
- allow worktree artifact control to target retained `git_worktree_root`
  artifacts even when the caller is currently in `canonical_root`

Reason:

- the runtime already knows projection, execution root, and provenance
- the runtime does not yet have real process confinement for reads, writes,
  network, secrets, or child processes
- making the capability boundary explicit is more honest and more useful than
  inventing a premature restriction matrix
- retained worktree artifacts are reviewable runtime artifacts, so their
  cleanup should be gated by artifact metadata rather than by requiring the
  caller to stay inside a worktree projection

## 4.2 Local TUI Surface

Decision:

- add `holon tui` as a thin local operator console on top of `holon serve`
- keep `serve` as the sole owner of `RuntimeHost`
- let the TUI talk to the existing local control surface instead of embedding a
  second runtime owner

Reason:

- `Holon` is meant to be long-lived and proactive, so runtime lifecycle cannot
  be tied to one terminal session
- operator dogfooding needs continuous state visibility without stitching
  together `status`, `tail`, `transcript`, and workspace commands by hand
- reusing the local control surface keeps the UI thin and avoids inventing a
  TUI-first architecture

Follow-on interaction decision:

- keep the TUI chat-first and remove pane-focus navigation
- keep prompt entry always available in the main surface
- move agents, transcript, tasks, and help into temporary overlays
- support `auto | always | never` alternate-screen behavior so the local TUI
  can preserve terminal scrollback in environments where full-screen mode is
  more harmful than helpful
- when the TUI consumes the native event stream, keep the main surface
  conversation-first: derive durable conversation items, transient in-flight
  activity, and an on-demand raw event inspector from the same stream instead
  of promoting raw events into a permanent primary pane

Reason:

- the old page/tab/pane model made basic operator actions require too much UI
  state tracking
- a local runtime console benefits more from fewer focus modes than from more
  persistent panes
- alternate-screen behavior needs to respect terminal multiplexers such as
  Zellij where preserving scrollback is often the better default

## 4.3 Local Daemon Lifecycle Surface

Decision:

- keep `holon serve` as the foreground runtime owner
- add `holon daemon start|stop|status|restart` as an operator lifecycle layer
  on top of the same runtime
- expose runtime-scoped local control routes for daemon lifecycle:
  - `GET /control/runtime/status`
  - `POST /control/runtime/shutdown`
- persist local runtime metadata under `<holon_home>/run/` and compare a
  sanitized effective-config fingerprint before deciding whether a start is
  idempotent or requires explicit restart
- make `serve` itself use the same startup preflight instead of silently
  deleting occupied socket paths
- derive a concise runtime activity summary for `daemon status` from current
  public agent snapshots instead of inventing a second runtime-state machine or
  forcing full runtime initialization
- surface the latest known runtime failure through `daemon status`, using
  persisted agent/runtime summaries instead of requiring operators to open logs
  first, while clearing daemon-level lifecycle failures after a later
  successful start/stop so the status surface stays current
- add `holon daemon logs` as the stable follow-up inspection surface, exposing
  local log paths, recent lifecycle failure summaries, and a bounded log tail
  without introducing remote logging infrastructure
- keep daemon/runtime service shutdown distinct from agent administrative stop:
  runtime shutdown exits loaded runtimes without durably persisting public
  persistent agents as `Stopped`, while explicit agent `stop` remains durable

Reason:

- the product language already treats the long-lived local runtime like a
  daemon/service
- operators need a first-class local lifecycle surface without manually holding
  open one `serve` terminal
- daemon lifecycle has to stay layered on top of `serve`, not become a second
  runtime mode
- startup recovery and socket ownership need to be explicit to avoid silent
  takeover of unrelated local processes
- operators need a cheap way to distinguish healthy-idle from healthy-busy
  without opening the TUI or a task-specific dashboard
- nearby daemon/runtime versions still need to interoperate, so newer runtime
  status fields must be optional on the decoding path
- operators also need a first-stop failure summary with phase and timestamp so
  log inspection becomes a follow-up step instead of the default first step
- daemon restart should not strand previously active public agents in a durable
  stopped posture unless an operator explicitly stopped them
- explicit agent `stopped` remains an administrative lifecycle boundary, so new
  external ingress must reject with resume guidance instead of silently
  enqueueing stranded work
- operator-facing status and wake surfaces should reinforce the same boundary:
  stopped agents advertise resume-required lifecycle hints, and `wake` must not
  present itself as a substitute for explicit `resume`
- callback-capability and public webhook ingress share the same public-agent
  lifecycle admission contract; enqueue-mode delivery must not bypass the
  stopped-boundary checks that already apply to prompts and other external
  ingress

## 4.4 Agent-Level Model Override

Decision:

- keep `model.default` and `model.fallbacks` as the runtime-wide baseline
- add an agent-scoped primary model override that only changes one agent's
  future provider turns
- keep the inherited runtime default in the effective chain after the override
  so one agent can try a different primary model without losing the runtime's
  fallback posture
- expose effective model selection through agent status with explicit source:
  `runtime_default` vs `agent_override`

Reason:

- long-lived multi-agent runtime makes model selection an agent concern, not
  only a process-global concern
- operators need to compare models on one agent without perturbing the rest of
  the runtime
- keeping the runtime default in the fallback chain preserves the existing
  fallback posture while still allowing one agent to prefer a different model

## 4.5 Local Operator Troubleshooting Workflow

Decision:

- document one recommended local troubleshooting path instead of leaving
  operators to choose ad hoc between `run`, `serve`, `daemon`, `status`, and
  `tui`
- make the phase-1 order:
  - `run --json` for one-shot reproduction
  - `daemon status` for long-lived runtime health
  - `daemon logs` for local lifecycle failure details
  - `status` / `tail` / `transcript` for agent-scoped inspection
  - `tui` for live observation after runtime health is confirmed
  - foreground `serve` for direct startup/runtime debugging

Reason:

- Holon now has enough operator entry points that "just try commands" is no
  longer a coherent workflow
- runtime health, agent state, and live interaction are different debugging
  layers and should be inspected in that order
- `daemon logs` and `provider_attempt_timeline` already provide stable
  structured diagnostics, so the missing piece was a documented inspection
  sequence rather than new runtime behavior

## 5. External Event Surfaces

Decision:

- implement three minimal external surfaces:
  - generic webhook
  - timer creation
  - remote prompt ingress
- authenticate remote prompt ingress with `HOLON_REMOTE_TOKEN`

Reason:

- these are enough to prove the runtime is truly event-driven
- the token-based remote route is intentionally small and explicit; it is not a
  full account or agent-sharing system

## 6. Multi-Agent Host Shape

Decision:

- use a `RuntimeHost` registry that lazily creates per-agent runtimes
- keep agent storage under `.holon/agents/<agent_id>/`
- keep one runtime loop per agent

Reason:

- this is the minimum clean shape for multi-agent isolation
- it avoids overloading a single runtime with cross-agent branching

## 7. Closure Outcome Versus Runtime Status

Decision:

- keep `AgentStatus` as runtime control/posture state
- derive a separate closure view for:
  - `completed`
  - `failed`
  - `waiting`
- carry semantic waiting reason separately from sleeping posture
- treat only blocking tasks as `awaiting_task_result`

Reason:

- runtime control flow and operator-facing closure meaning are not the same layer
- collapsing them made `run` and long-lived agents drift apart
- background tasks should remain observable without automatically becoming
  critical-path waiting

## 8. What Was Deliberately Not Done

- no UI surface
- no plugin marketplace
- no OS sandbox
- no distributed background workers
- no model-generated compaction summaries
- no MCP channel implementation yet

Those areas remain future work if they become justified by real usage.

## 9. Continuation Resolution Is Derived From Closure, Not Queue Mechanics

Decision:

- derive a typed continuation view from:
  - prior `ClosureDecision`
  - trigger kind
  - trigger contentfulness
- treat blocking `TaskResult` as the canonical delegated-work rejoin point
- allow `SystemTick` to remain `liveness_only`
- do not silently elevate external continuation into operator authority

Reason:

- wake, queue ingress, and model-visible continuation are different layers
- `TaskResult` had drifted into an observational event instead of a rejoin
  contract
- explicit continuation resolution makes follow-up behavior inspectable through
  `last_continuation` and audit events
- a model-visible `TaskResult` rejoin remains the canonical observable
  continuation for that processing cycle; more generally, the scheduler should
  not immediately obscure any model-visible turn with a synthetic
  `continue_active` tick

## 10. Tool Calling Shape

Decision:

- use native Anthropic-style `tool_use` / `tool_result`
- do not invent a Holon-specific JSON action protocol

Reason:

- the design reference for Holon’s coding path is the Claude Code runtime shape
- the configured local Anthropic-compatible endpoint already supports the same
  request/response model in practice
- keeping the core loop close to the model-native tool protocol avoids an extra
  adapter layer in the runtime contract

## 11. Objective State Was Retired In Favor Of Work-Queue Truth

Decision:

- remove the parallel `objective_state` pipeline from `AgentState`
- keep runtime work truth centered on persisted `WorkItemRecord` and
  `WorkPlanSnapshot`
- when continuity needs a new durable artifact, add it explicitly instead of
  reintroducing an agent-wide objective shadow state
- use structured session memory derived from work-item, work-plan, brief, tool,
  and waiting evidence as the primary continuity projection for prompt
  compaction

Reason:

- closure and continuation explain why work resumes, but not what work the
  runtime still believes it is doing
- recent transcript text and compaction summaries are not a stable scope
  contract
- task and child-agent rejoin should carry only the runtime metadata needed for
  re-entry, not a parallel objective-truth channel

## 9. Workspace Tool Boundary

Decision:

- restrict workspace file tools to the configured workspace root
- enforce that restriction in the tool layer itself, not only in higher-level
  policy
- use lexical path normalization rather than partial `canonicalize` checks

Reason:

- coding tools need a hard path boundary that survives model mistakes
- path policy must work even when the target path does not exist yet
- lexical normalization avoids macOS `/var` versus `/private/var` mismatches for
  temporary directories

## 10. Shell Exposure Model

Decision:

- expose shell tools only to `TrustedOperator` and `TrustedSystem`
- persist shell output through the same tool execution log as other tools

Reason:

- shell is necessary for real coding loops, but it is the sharpest local tool
- storing shell results in the same tool log keeps later context assembly and
  auditing coherent

## 11. Sleep As A Terminal Tool Round

Decision:

- treat a tool round containing only `Sleep` calls as a valid terminal state
- finalize the turn immediately instead of forcing another model round

Reason:

- with real Anthropic-compatible providers, the model may emit `Sleep` after it
  has already completed the user-facing answer
- without this rule, the runtime can loop until the tool-round cap despite the
  task being effectively done

## 12. Subagent V1 Shape

Decision:

- implement `subagent_task` as a bounded in-process background agent
- let it reuse the same provider and task/result rejoin path
- avoid nested task creation inside the first subagent implementation

Reason:

- this preserves the Claude Code-inspired idea that subagents are orchestration
  units, not a separate runtime
- keeping subagent V1 bounded avoids async recursion and uncontrolled task
  spawning in the first coding runtime

## 13. Context For Coding Follow-Ups

Decision:

- add recent tool executions to the model-visible context
- surface the latest completed result explicitly in addition to recent briefs

Reason:

- coding follow-ups depend heavily on command output and recent edits
- “recent briefs” alone were not reliable enough for follow-up recall in live
  provider tests

## 13a. Shared Tool Error Envelope

Decision:

- standardize agent-facing tool failures on one shared envelope
- keep `kind`, `message`, optional `details`, optional `recovery_hint`, and
  `retryable` available in transcripts and audit events
- let tools add narrow recovery hints for well-known contract failures instead
  of inventing per-tool ad hoc text formats

Reason:

- short freeform strings were mechanically correct but too thin for headless
  recovery
- the agent needs enough structured context to distinguish invalid input,
  permission issues, execution-root violations, and command spawn failures
- preserving the structured envelope alongside rendered text keeps provider
  compatibility while improving debugging and follow-up turns

## 14. Turn Terminal Settlement Before Closure

Decision:

- persist a turn-terminal record for each completed or aborted interactive turn
- make `run_once` and child-task result collection depend on that turn-terminal
  state instead of depending on terminal briefs
- remove the extra terminal-delivery model round that used to ask the model to
  restate completion

Reason:

- terminal settlement is a runtime fact and should not depend on textual
  heuristics such as `done`, `completed`, or `sleep requested`
- forcing a terminal-delivery round distorted the final result surface and
  created a second model-generated summary that could disagree with the actual
  terminal turn
- child-agent rejoin and one-shot `run_once` need the same lower-level notion
  of "the turn is over" before any higher-level objective semantics are added

## 15. Verification Strategy For The Coding Runtime

Decision:

- keep four test layers:
  - unit tests
  - integration tests
  - live Anthropic-compatible tests
  - fixture-based regression tests

Reason:

- coding behavior breaks in different ways at different layers
- live tests catch provider/runtime mismatches
- fixture tests preserve a replayable baseline for representative coding loops

## 16. Main Session Tool Loop Limits

Decision:

- do not enforce a default tool-round cap for the main agent
- keep loop limits as an optional per-flow control, not a global default

Reason:

- the design reference in Claude Code uses optional bounded turns for special
  helper flows, not a small hard cap on the primary interactive agent
- coding tasks frequently need more than a handful of model/tool/tool-result
  rounds to converge
- dead-loop prevention is better handled with more specific controls than a
  blanket low default on the main runtime path

## 17. Workspace Binding Model

Decision:

- model workspace attachment explicitly with host-owned workspace entries
- keep `workspace_anchor`, active workspace entry, `execution_root`, and `cwd`
  as separate runtime fields
- split `attach_workspace` from `enter_workspace` / `exit_workspace`
- treat access mode as `shared_read` vs `exclusive_write`
- treat managed worktree as an execution projection, not as a project identity
  change

Reason:

- daemon cwd and shell `cd` are not reliable sources of project identity
- instruction and skill loading need a stable workspace anchor
- active execution state needs an explicit projection/access contract
- worktree isolation should not silently rewrite attachment semantics

## 18. OpenAI Codex Transport Contract

Decision:

- support `openai-codex/*` through the required Responses streaming transport
  contract instead of routing it through the single-body JSON path

Reason:

- the Codex backend path requires `stream=true`
- the ChatGPT Codex backend rejects `max_output_tokens` on that streaming path,
  so Holon must keep the codex request shape separate from the standard
  `openai/*` Responses JSON body
- a naive `stream=true` toggle is insufficient unless Holon also consumes the
  streamed event feed, reconstructs terminal output from streamed
  `response.output_item.done` events when needed, and classifies explicit
  terminal failure events such as `response.failed`
- pretending the provider is available causes deterministic operator-facing
  runtime failures instead of a clear configuration-time diagnostic

## 19. Provider Retry Classification

Decision:

- classify provider transport failures into retryable vs fail-fast buckets
- retry transient provider failures at most two times before moving to the next
  configured fallback provider
- keep retry classification and retry exhaustion visible in provider
  diagnostics and aggregated provider errors

Reason:

- request timeouts, connection failures, rate limits, and short-lived `5xx`
  responses are often recoverable on the same provider path
- auth failures, contract errors, invalid JSON, and unsupported transport
  contracts are deterministic and should not burn retries
- Holon already has provider fallback ordering, so the runtime should first
  exhaust bounded retry on one provider and then continue to the next provider
  instead of failing the whole turn immediately
- surfacing retry policy and exhaustion in diagnostics keeps operator-visible
  runtime failures explicit instead of hiding retry behavior inside provider
  adapters

## 20. Provider Attempt Timeline

Decision:

- preserve a stable provider-attempt timeline alongside provider-turn outcomes
- expose the same timeline on operator-facing transcript and audit surfaces for
  both successful and failed turns
- keep the contract limited to retry / fail-fast / fallback diagnostics rather
  than introducing a new observability subsystem
- preserve structured transport diagnostics on failed attempts when adapters
  can observe reqwest/body/read context, so `unknown` failures remain
  inspectable without turning operator-facing summaries into verbose dumps

Reason:

- operators need to see which provider was tried, how many attempts happened,
  and when fallback advanced without reconstructing that behavior from one final
  error string
- successful fallback is still operationally ambiguous unless the failed
  attempts before the winning provider are retained
- attaching the same contract to transcript and audit surfaces keeps later TUI
  or CLI work additive instead of forcing provider-specific parsing
- `failure_kind=unknown` is still operationally useful when the timeline keeps
  stage, reqwest category signals, URL/model/provider, and source-chain detail

## 21. Failure Artifact Normalization

Decision:

- normalize operator-facing failures across provider, runtime, and task paths into
  a shared `FailureArtifact` contract
- keep category, kind, summary, and bounded metadata as the stable interface:
  - provider failures expose transport / protocol categories with retry context
  - runtime failures expose runtime message context
  - task failures expose task-oriented metadata including id and exit status
- surface the same artifact shape on all operator-facing outputs where failure is
  serialized:
  - persisted runtime failure summaries
  - run-task snapshots returned by task output tools
  - `holon run --json` top-level failure field
- keep raw payload detail in internal transcript logs and event surfaces, but keep
  operator-facing artifact bounded and portable

Reason:

- benchmarking and operator tooling can classify outcomes from one stable field
  instead of re-parsing raw task/runtime/protocol logs each time
- unknown failures remain actionable if provider/protocol details are absent
  because summary + metadata still provides stable lineage
- shared failure contracts reduce operator confusion between provider/runtime/task
  failure interpretation and reduce future migration risk when adding layers

## 22. Local Skills V1

Decision:

- discover local skills from user, agent, and workspace roots, each using the
  first existing compatibility directory in `.agents/skills`, `.codex/skills`,
  `.claude/skills` order
- expose skill catalogs and active skills as inspectable runtime metadata
- treat opening a discovered `SKILL.md` through an approved local inspection
  tool as the minimal phase-1 activation signal
- promote `turn_active` skills to `session_active` at successful turn
  completion
- do not add explicit `skill activate` control surfaces in phase 1
- scope non-default skill visibility to named and child agents rather than the
  older peer/delegated wording

Reason:

- this keeps skills local-first and file-rooted instead of introducing a large
  subsystem before the contract is stable
- discovery, attachment, and activation stay distinct and inspectable
- delegated execution can stay narrower than the default agent without hidden
  user-level inheritance

## 22. Operator-Facing Token Usage

Decision:

- expose token usage as a first-class structured contract on operator-facing
  run and status surfaces
- preserve per-turn token usage on transcript and audit metadata instead of
  only in cumulative agent counters
- allow runtime/provider failure surfaces to include token usage when provider
  diagnostics can aggregate it, but degrade gracefully when they cannot

Reason:

- cumulative totals alone do not explain the cost or context pressure of one
  recent model turn
- `holon run` and `holon status` are already stable operator entry points, so
  they should not require later TUI work to make token usage inspectable
- keeping the contract small and explicit avoids introducing a broader
  observability subsystem just to expose token counts

## 22. Tool Schema Source Of Truth

Decision:

- derive built-in tool input schemas from typed Rust argument structs using
  `schemars`
- keep `ToolSpec.input_schema` as the runtime-neutral schema representation for
  now instead of introducing a new schema AST
- keep provider-facing schema normalization as a thin Holon-owned policy layer
  on top of the derived source schema
- default provider emission to `strict: false`
- keep a stronger `Strict` emission mode available internally for validation
  and CI matrix coverage

Reason:

- hand-written JSON literals had already drifted from the actual tool argument
  contract and were not scaling across the growing built-in tool set
- Holon needs one stable source of truth for tool arguments, but it does not
  need a full custom schema engine
- provider strictness rules such as “all properties required” and
  “optional becomes nullable” are transport-emission policy, not the honest
  source schema
- keeping runtime default emission relaxed avoids surprising compatibility
  regressions while still letting CI enforce the stricter provider contract

## 23. Shell-First Repo Inspection

Decision:

- retire provider-facing `Read`, `Glob`, and `Grep` from the normal model tool
  surface
- make `exec_command` the primary repo-inspection primitive
- truncate oversized command output before reinjecting it into the active model
  conversation

Reason:

- a single inspection primitive is easier for models to select consistently
- shell-first inspection transfers better across local, container, and remote
  execution backends
- retiring duplicate read/search tools reduces prompt and benchmark ambiguity
- per-tool output truncation is the first protection against context blow-up,
  while broader context compaction can stay a separate runtime concern

## 24. Work-item rollout remains message-driven first

The work-item runtime rollout tracked by `#228` does not start by forcing every
ingress message through a semantic work-item resolver.

The early phases keep the existing message-driven runtime path intact and add
`WorkItem` as an explicit higher-level runtime container.

That means:

- `WorkItem` and `WorkPlan` use a persisted store separate from `AgentState`
- work items are optional during early rollout phases
- the planned explicit adoption path is `update_work_item` / `update_work_plan`
- a planned dedicated control surface can enqueue queued work items directly
- scheduler/tick integration comes only after persisted state, prompt
  projection, mutation, and turn-end transition commit all exist

This keeps the migration incremental and avoids introducing a second resolver
agent just to classify arbitrary ingress text into work items.

## 25. Work-Queue Prompt Projection Is Optional In Early Rollout

Decision:

- project work-item state into prompt context only when persisted work items
  already exist
- inject the full active work-item snapshot and current plan
- inject only compact summary entries for queued and waiting items
- exclude completed work items from the normal prompt projection
- do not synthesize a bootstrap work item when the runtime is still operating
  through the existing message-driven path

Reason:

- the rollout remains message-driven first in early phases
- prompt projection should reflect persisted work-item state, not invent it
- compact non-active summaries preserve awareness without overloading the prompt
- keeping the no-work-item case unchanged avoids coupling projection to a
  semantic ingress resolver that does not exist yet

## 26. Work-Item Adoption Uses Explicit Mutation Tools

Decision:

- add `update_work_item` and `update_work_plan` as the minimal trusted tool
  surface for explicit work-item adoption
- keep `update_work_item` on create-or-replace latest-snapshot semantics
- keep `update_work_plan` on full-snapshot replacement semantics
- do not expose these tools on the untrusted external tool surface in the
  first rollout

Reason:

- work-item state is a higher-level runtime container and should not be created
  implicitly from arbitrary ingress text
- explicit mutation is sufficient for early adoption without inventing a
  second semantic resolver agent
- full-snapshot plan writes keep prompt projection and persistence simple while
  the runtime model is still being established

## 27. Control-Plane Work-Item Enqueue Is Separate From Message Ingress

Decision:

- add a control route to enqueue queued work items directly
- persist the queued item without creating a normal transcript message
- do not let this route interrupt or replace the current active work item

Reason:

- operators sometimes need to queue future work intentionally while the agent
  is already busy
- forcing that through normal prompt ingress would either interrupt the current
  turn or reintroduce semantic inference from free-form message text
- keeping the path explicit preserves the message-driven runtime while still
  giving the control plane a precise queue-management surface

## 28. Turn-End Work-Item Commit Uses A Bound Active Snapshot

Decision:

- bind each interactive turn to the active work item snapshot visible at turn
  start
- commit turn-end work-item transitions only against that bound item
- treat the latest persisted bound-item snapshot as the agent's transition claim
- keep runtime fact checks intentionally small and conservative

Reason:

- prompt context already projects the current active work item, so turn-end
  commit should resolve against the same runtime object the model saw
- recomputing against whichever item is active later would let unrelated queue
  changes leak into the wrong turn settlement
- preserving the latest bound-item snapshot lets explicit `update_work_item`
  writes serve as the transition claim without introducing a second transition
  tool in this phase
- the conservative bias stays aligned with the RFC:
  - no explicit reason means default back to `active`
  - runtime waiting facts can force `waiting`
  - `completed` is rejected when runtime facts still show obvious unfinished
    waiting conditions

## 29. Idle Activation Comes From The Persisted Work Queue

Decision:

- when the runtime is idle, consult the persisted work queue before sleeping
- if an `active` work item exists, emit a runtime-owned system tick to continue
  it
- if no `active` work item exists but a `queued` item does, promote one queued
  item to `active` and emit a runtime-owned system tick for it
- keep the no-work-item case on the existing message-driven idle path
- keep coalesced wake-hint ticks in the same idle path so external wake signals
  are not starved

Reason:

- `#226` is the point where persisted work-item state should actually drive
  liveness instead of being prompt-only metadata
- consulting the work queue only when the runtime is otherwise idle preserves
  the current message queue semantics and keeps the scheduler non-preemptive
- activating queued work by writing a new `active` snapshot keeps scheduling
  explicit and auditable in the same append-only store as the rest of the
  work-item lifecycle
- retaining the no-work-item fallback avoids forcing old message-driven flows
  through a synthetic work-item bootstrap

## 30. `/status` Remains Agent-Facing While `/state` Stays Bootstrap-Oriented

Decision:

- keep `GET /agents/:id/status` as the concise agent-facing summary surface
- keep `GET /agents/:id/state` as the richer first-party projection bootstrap
  surface
- make `/state.agent` intentionally reuse the same `AgentSummary` contract as
  `/status`
- allow `/state` to carry additional projection-bootstrap slices such as
  session, tasks, transcript tail, briefs tail, waiting state, callback state,
  and workspace bootstrap details
- do not treat `/state` yet as the universal rich snapshot API for future
  third-party clients

Reason:

- the native event stream rollout needs one coherent bootstrap payload for
  first-party projection clients after replay loss
- operators and scripts still benefit from a smaller, more stable
  agent-inspection surface that answers "what is this agent's current summary?"
  without also carrying projection-only slices
- reusing `AgentSummary` inside `/state` keeps lifecycle, identity, model, and
  closure semantics aligned across both surfaces
- keeping the boundary explicit avoids silently turning TUI/bootstrap needs into
  the long-term public contract for every future client

## 31. Weak Verification Text Is Kept as Raw Evidence, Not Promoted Memory Fact

Decision:

- remove `latest_verified_result` from session working memory
- remove `result_hint` from turn deltas and archived episode memory
- keep verification evidence available through raw recent briefs and tool
  executions instead of elevating it into a durable prompt-facing fact

Reason:

- recent "verified" text is still a heuristic over tool and brief summaries,
  not a structured runtime verification artifact
- presenting that text as a durable memory fact creates false confidence and
  contaminates later episode retrieval when the derived conclusion is stale,
  partial, or wrong
- the runtime should only promote stronger, explicit continuity anchors; weak
  verification evidence can remain useful without being upgraded into truth
