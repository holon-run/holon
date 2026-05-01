# Holon Runtime Spec v0

This document defines the first runtime contract for `Holon`.

The goal of v0 is not to solve every future integration. The goal is to make
the core lifecycle explicit enough that the first local runtime can be built
without hidden assumptions.

## Scope

This spec covers:

- agent state
- message envelope
- queue item model
- wake and sleep lifecycle
- background task model
- user-facing `brief` output

This spec does not yet define:

- transport-specific adapters
- model-provider APIs
- sandbox internals
- distributed execution

## Provider Transport Contract

Provider selection is runtime-visible and fail-closed when a configured
transport contract is unsupported.

Current phase-1 contract:

- `openai/*` uses non-streaming OpenAI-style Responses JSON
- `anthropic/*` uses non-streaming Anthropic Messages JSON
- `openai-codex/*` uses OpenAI-style Responses streaming (`stream=true`)
- compatible provider ids may use `openai_chat_completions` when their HTTP API
  accepts the OpenAI Chat Completions request shape

`openai-codex/*` requires the streaming transport contract. Holon consumes the
streamed event feed for that provider and parses the terminal response event
into the same runtime block model used by the other providers. The codex
streaming request shape is not identical to the standard `openai/*` Responses
JSON path: Holon omits `max_output_tokens` on the `openai-codex/*` transport
because the ChatGPT Codex backend rejects that parameter. Holon also preserves
Codex-compatible request fields such as `reasoning: null` and `include: []`,
reconstructs terminal `output` from streamed `response.output_item.done`
events when needed, and classifies `response.failed` / `response.incomplete`
terminal events directly instead of collapsing them into generic
`invalid_response` failures.

Runtime rules:

- unsupported provider transport contracts must fail closed during candidate
  selection
- provider diagnostics must surface transport-contract failures explicitly
- provider fallback may continue to later configured providers after an
  unsupported transport candidate is rejected
- built-in compatible provider defaults must only cover providers whose
  endpoint/auth behavior fits an existing transport contract; providers that
  need native auth, custom headers, SDK credential chains, or dynamic gateway
  discovery remain explicit configuration until Holon defines that contract

Provider request retry is also runtime-visible and bounded.

Current phase-1 retry contract:

- retry classification happens per provider request attempt
- Holon retries transient failures on the same provider before moving to the
  next configured fallback provider
- retry exhaustion is visible in the final aggregated provider failure
- retry, fail-fast, and fallback decisions are also preserved in a stable
  provider-attempt timeline on transcript and audit surfaces
- `provider_doctor` exposes the current retry policy alongside provider
  availability

Current retry policy:

- max retries per provider request: `2`
- retryable failures:
  - request timeout
  - connection failure
  - HTTP `429`
  - eligible HTTP `5xx`
- fail-fast failures:
  - auth failure (`401` / `403`)
  - deterministic client / contract errors (`4xx`)
  - invalid provider JSON / unsupported response shape
  - unsupported transport contracts that do not match the runtime's configured
    provider implementation

Fallback rules:

- retry happens inside one provider candidate before moving to the next
  configured provider
- exhausting retries on provider A must still allow fallback to provider B
- fail-fast errors on provider A may also continue to provider B; only the
  current provider candidate is aborted
- if a provider request still returns `context_length_exceeded`, Holon treats
  that as a turn-level terminal failure, surfaces an operator-visible failure
  brief, and does not auto-reactivate the active work item through
  `continue_active`

Provider attempt timeline contract:

```ts
type ProviderAttemptOutcome =
  | 'retrying'
  | 'retries_exhausted'
  | 'fail_fast_aborted'
  | 'succeeded'

type ProviderAttemptRecord = {
  provider: string
  model_ref: string
  attempt: number
  max_attempts: number
  failure_kind?: string
  disposition?: string
  outcome: ProviderAttemptOutcome
  advanced_to_fallback: boolean
  backoff_ms?: number
  token_usage?: TokenUsage
  transport_diagnostics?: {
    stage: string
    provider?: string
    model_ref?: string
    url?: string
    status?: number
    reqwest?: {
      is_timeout: boolean
      is_connect: boolean
      is_request: boolean
      is_body: boolean
      is_decode: boolean
      is_redirect: boolean
      status?: number
    }
    source_chain?: string[]
  }
}

type ProviderAttemptTimeline = {
  attempts: ProviderAttemptRecord[]
  winning_model_ref?: string
  aggregated_token_usage?: TokenUsage
}
```

Phase-1 visibility rules:

- successful provider rounds may still include prior failed attempts before the
  winning provider
- each provider-attempt record may carry its own `token_usage` when that
  attempt reported usage
- failed provider rounds preserve the attempt timeline alongside the final
  aggregated error
- transport-backed failures may attach structured `transport_diagnostics` per
  attempt so runtime artifacts can distinguish timeout vs connect vs body/read
  failures without expanding operator-facing summaries
- the timeline is attached to:
  - `provider_round_completed`
  - `terminal_delivery_round_completed`
  - `runtime_error`
   - transcript `AssistantRound` entries
   - transcript `RuntimeFailure` entries
- this issue does not define any TUI presentation; future surfaces should
  consume the same contract

Token usage contract:

```ts
type TokenUsage = {
  input_tokens: number
  output_tokens: number
  total_tokens: number
}

type AgentTokenUsageSummary = {
  total: TokenUsage
  total_model_rounds: number
  last_turn?: TokenUsage
}
```

Phase-1 token visibility rules:

- transcript `AssistantRound` entries keep top-level `input_tokens` /
  `output_tokens` fields and also mirror them under `data.token_usage`
- `provider_round_completed` and `terminal_delivery_round_completed` audit
  events include `token_usage`
- `runtime_error` and transcript `RuntimeFailure` entries include token usage
  only when provider diagnostics can aggregate it
- `holon run --json` includes a top-level `token_usage`
- `holon run --json` includes a top-level normalized `failure_artifact` when the
  run ended failed, and that artifact matches the same categories used in task and
  runtime failure persistence
- `holon status` exposes an `AgentTokenUsageSummary` with cumulative usage plus
  the most recent completed turn that reported token data, sourced from
  persisted runtime state rather than transcript window scanning
- missing provider token data degrades to zero or absent token usage instead of
  failing the turn

## Failure Artifact Envelope

Holon normalizes operator-facing failures into a single `FailureArtifact` shape
before serializing them to operator surfaces.

```ts
type FailureArtifact = {
  category: "transport" | "protocol" | "runtime" | "task" | "unknown"
  kind: string
  summary: string
  provider?: string
  model_ref?: string
  status?: number
  task_id?: string
  exit_status?: number
  source_chain?: string[]
  metadata?: Record<string, string>
}
```

Rules:

- one artifact is optional and may be absent on non-failed operators-facing runs
- category distinguishes provenance:
  - `transport`: provider transport-stage failures
  - `protocol`: unsupported contract / invalid response class failures
  - `runtime`: runtime-message or turn failures
  - `task`: task-command / task-framework failures
  - `unknown`: any unclassified operator-facing failure
- providers, runtime failures, and task failures project into this shape:
  - `RuntimeFailureSummary.failure_artifact` carries provider/runtime failures
  - `TaskOutputSnapshot.failure_artifact` carries task failures
  - `RunOnceResponse.failure_artifact` carries the preferred operator-facing
    failure artifact for run replay
- `metadata` provides bounded structured detail without exposing noisy raw log
  payloads by default

## Provider Tool Schema Contract

Built-in Holon tools define their source input schemas from typed Rust argument
structs using `schemars`.

Current phase-1 source-schema contract:

- built-in tool schemas are derived from typed Rust argument definitions rather
  than hand-written JSON literals
- source schemas preserve the honest Rust parameter shape, including optional
  fields
- `ToolSpec.input_schema` remains the runtime transport-neutral representation
  used by the tool registry

Provider emission has a separate runtime-internal schema contract mode:

- `Relaxed`
- `Strict`

Current phase-1 runtime behavior:

- provider runtime paths default to `Relaxed`
- `Relaxed` emits `strict: false`
- `Relaxed` still normalizes object schemas to disable
  `additionalProperties`
- `Strict` is exercised in tests and internal validation, not enabled by
  default at runtime

Current phase-1 strict-emission rules:

- every emitted object schema sets `additionalProperties: false`
- every emitted object schema sets `required` to all property names
- source-optional fields are rewritten into nullable emitted fields
- nullable enum fields must also include `null`
- nested object and array item schemas recurse through the same rules

Current phase-1 validation rules:

- Holon validates source tool schemas for all built-in tools
- Holon validates strict-emitted provider schemas for all built-in tools
- OpenAI and OpenAI-Codex request-payload tests exercise the full built-in tool
  matrix in `Strict` mode so provider-compatibility drift fails in CI before
  live runs

## Agent Initialization Contract

Current phase-1 agent initialization behavior:

- every agent has an `agent_home`
- named-agent creation may initialize that `agent_home` from a `template`
  selector
- `template` accepts exactly three forms:
  - a simple `template_id`, resolved from `~/.agents/templates/<template_id>/`
  - an absolute local template directory path
  - a GitHub URL in the form
    `https://github.com/<owner>/<repo>/tree/<ref>/<path-to-template-dir>`
- a template directory must contain `AGENTS.md` and may include `skills.json`
  with `skill_refs`
- phase-1 only accepts local skill refs in `skills.json`; GitHub skill refs are
  rejected for now rather than invoking a remote installer during agent
  creation
- template application materializes the initial `agent_home/AGENTS.md`,
  required Holon agent-home guidance, fixed `memory/self.md` and
  `memory/operator.md` entries, the visible `notes/`, `work/`, and `skills/`
  directories, and the runtime-owned `.holon/` storage area
- failed template resolution or skill installation fails closed rather than
  partially initializing the agent
- the runtime records template provenance under `agent_home/.holon/state/`, but
  the live source of truth remains the materialized `agent_home/AGENTS.md` and
  agent-local skills
- builtin templates are seeded into `~/.agents/templates/` on runtime startup
  under Holon-prefixed ids such as `holon-default` and `holon-developer`
- builtin template updates may refresh those Holon-managed template directories
  on startup when the on-disk copy still matches the last Holon-managed
  version; locally diverged template content is not silently overwritten
- the default agent initializes its missing agent-scoped `AGENTS.md` from the
  builtin `holon-default` template without overwriting an existing
  `agent_home/AGENTS.md`

## Repo Inspection Contract

Current phase-1 repo-inspection contract:

- normal model-facing repo inspection is shell-first through `exec_command`
- provider-facing `Read`, `Glob`, and `Grep` are retired from the exposed tool
  surface
- `ApplyPatch` remains the stable file-mutation primitive when the active
  execution profile and runtime capabilities expose local-environment tools
- OpenAI-compatible runs may expose `ApplyPatch` as a freeform grammar tool,
  while Anthropic-compatible runs keep a strict JSON fallback of
  `{"patch":"--- a/path\\n+++ b/path\\n@@ ...\\n"}`; both forms carry the
  same unified diff body into the runtime patch engine
- oversized `exec_command` output is truncated before it re-enters the active
  provider conversation as a tool result

Phase-1 implications:

- prompts should prefer `rg --files`, `rg -n`, `sed -n start,endp`, `head`,
  and `tail`
- operators should treat whole-file `cat` as the exceptional path, not the
  default inspection primitive
- this contract controls single-tool reinjection only; longer-horizon context
  structured working memory and compaction remain separate runtime concerns

## Tool Error Envelope Contract

Agent-facing tool failures use a shared error envelope instead of freeform
strings alone.

This section is the current implemented error contract. The target
model-visible success/error contract is defined more broadly in
`docs/rfcs/tool-result-envelope.md`.

The envelope fields are:

- `kind`: stable machine-readable failure kind
- `message`: concise human-readable summary
- `details`: optional structured context that is safe to expose back to the
  agent
- `recovery_hint`: optional concise next-step guidance
- `retryable`: whether retrying the same logical action is expected to help

Runtime expectations:

- the provider-facing `tool_result.content` remains a plain string, but it
  should render the shared error envelope consistently
- transcripts should preserve the structured error object as `error`
- audit events should preserve the structured error object as `tool_error`,
  while any audit-event `error` field remains the flattened rendered string
- tools may attach tool-specific `details` and `recovery_hint` values for
  well-known failure modes
- `exec_command` should surface execution-root violations and command spawn
  failures with enough context for the agent to self-correct without operator
  intervention

## Local Operator Console Contract

`holon tui` is a local operator-facing console for a running `holon serve`.

It is an adapter on top of the local control surface, not a second runtime
owner:

- `serve` owns `RuntimeHost`
- `tui` connects through the local control endpoint
- closing the TUI must not stop runtime state, tasks, timers, or waiting child
  agents

The phase-1 TUI is expected to surface:

- current agent selection
- prompt input that stays available without pane focus changes
- a primary chat transcript built from operator messages plus `brief` output
- recent raw transcript and task visibility through overlays
- workspace / projection / access-mode state
- child-agent visibility
- local actions for workspace attach, workspace entry/exit, and debug prompt
- terminal alternate-screen behavior that can stay in normal scrollback mode
  when the operator environment makes full-screen TUI awkward

The current interaction model is intentionally narrow:

- one primary conversation surface
- direct typing into the composer with `Enter` to submit
- temporary overlays for agents, transcript, tasks, help, and local actions
- `Esc` closes the active overlay before affecting lower-priority UI state

## Local Daemon Lifecycle Contract

`holon daemon` is the operator lifecycle surface for the same runtime that
`holon serve` owns in the foreground.

Phase-1 commands:

- `holon daemon start`
- `holon daemon stop`
- `holon daemon status`
- `holon daemon restart`
- `holon daemon logs`

Current contract:

- `serve` remains the only runtime owner mode
- `daemon` starts the same `serve` runtime in the background instead of
  introducing a second runtime shape
- `daemon start` is idempotent for one `HOLON_HOME`
- if a healthy runtime is already running with the same effective config,
  `daemon start` returns success instead of starting a second runtime
- if a healthy runtime is already running with a different effective config,
  `daemon start` fails closed and requires explicit `daemon restart`
- stale local runtime files under `<holon_home>/run/` are recoverable and may
  be cleaned during start/stop
- stale daemon pid metadata that points to a missing process is also treated as
  recoverable stale state; `daemon restart` must clean it up instead of failing
  hard on `kill -TERM`
- socket-path takeover by an unrelated process fails closed
- `daemon stop` prefers graceful runtime shutdown through the control surface
  before falling back to process termination
- graceful runtime shutdown is a transient service-level drain and must not
  durably rewrite public self-owned agents into `stopped`
- `daemon status` must expose enough local runtime metadata to debug operator
  state:
  - `pid`
  - `home_dir`
  - `socket_path`
  - `http_addr`
  - control-connectivity health
  - effective-config fingerprint match
  - runtime activity summary:
    - `active_agent_count`
    - `active_task_count`
    - `processing_agent_count`
    - `waiting_agent_count`
    - `state = idle | waiting | processing`
  - last known runtime failure when available:
    - `occurred_at`
    - `summary`
    - `phase = startup | shutdown | runtime_turn`
    - `detail_hint`
  - daemon-level persisted startup/shutdown failures must be cleared after a
    later successful `daemon start` or `daemon stop`, so this surface does not
    keep reporting stale lifecycle failures once the local runtime is healthy
- daemon-status decoding must stay backward-compatible with nearby runtime
  versions; missing newer optional fields must not cause `status` or `stop` to
  fail
- `daemon logs` must expose a local-first inspection surface with:
  - `log_path`
  - daemon metadata path
  - latest-known-failure path
  - recent startup failure summary when available
  - recent shutdown failure summary when available
  - a bounded tail of the local daemon log
- lifecycle start/stop errors that benefit from log inspection should point to
  `holon daemon logs` instead of sending operators directly to the filesystem

Phase-1 runtime activity summary is intentionally concise:

- `processing` means at least one public agent is booting or actively running
- `waiting` means no public agent is actively running, but the runtime still
  has visible waiting work such as an awaiting task
- `idle` means the runtime is healthy and not currently processing or waiting
  on visible work

Phase-1 last-failure reporting is also intentionally concise:

- runtime-turn failures come from persisted agent state and remain visible to
  daemon inspection even after the turn has ended
- daemon startup/shutdown failures may also be persisted under `<holon_home>/run/`
- the status surface is a first stop, not a replacement for detailed logs
- `daemon logs` is the explicit next debugging step once `status` or a daemon
  lifecycle error indicates that deeper local inspection is needed

The runtime also exposes a runtime-scoped local control surface for this
contract:

- `GET /control/runtime/status`
- `POST /control/runtime/shutdown`

`POST /control/runtime/shutdown` is not an alias for agent administrative
`stop`. It requests service shutdown for the currently running runtime process
without durably persisting public self-owned agents as `stopped`.

## Agent Inspection Surface Contract

Holon now exposes two different public per-agent inspection surfaces:

- `GET /agents/:agent_id/status`
- `GET /agents/:agent_id/state`

They should not be treated as interchangeable.

Phase-1 contract:

- `/status`
  - the concise agent-facing summary surface
  - returns one `AgentSummary`
  - meant for operator inspection, scripts, and generic agent-facing
    integrations
- `/state`
  - the first-party projection bootstrap surface
  - returns one aggregated `AgentStateSnapshot`
  - meant for projection clients that need a coherent local bootstrap before
    consuming `/events`

Phase-1 duplication rule:

- `/state.agent` intentionally reuses the same agent summary contract as
  `/status`
- this duplication is allowed because replay loss must be recoverable from one
  bootstrap request
- duplication beyond the embedded `agent` summary should remain intentional and
  limited to projection-bootstrap needs

Phase-1 compatibility rule:

- `/status` carries the stronger compatibility expectation for agent-facing
  inspection
- `/state` may evolve to satisfy first-party bootstrap completeness
- `/state.agent` should remain status-compatible with `/status`
- `/state` should not yet be presented as the universal third-party rich
  snapshot API

Default aliases preserve the same split:

- `GET /status` is the default-agent alias for the status surface
- `GET /state` is the default-agent alias for the bootstrap surface

Client guidance:

- use `/status` when the need is "tell me about this agent"
- use `/state` when the need is "bootstrap a projection, then continue from
  `/events`"

## Local Operator Troubleshooting Contract

`Holon` now has enough local operator surfaces that the recommended
troubleshooting order is part of the product contract.

Phase-1 local troubleshooting order:

1. `holon run --json` for one-shot reproduction
2. `holon daemon status` for long-lived runtime health
3. `holon daemon logs` for lifecycle or runtime failure details
4. `holon status`, `holon tail`, and `holon transcript` for agent-scoped
   inspection
5. `holon tui` for continuous live observation and interaction after the
   runtime is already known healthy
6. foreground `holon serve` when debugging startup or runtime lifecycle
   behavior directly

Phase-1 meaning of each entry point:

- `run` is the bounded reproduction surface
- `daemon status` is the first long-lived health probe
- `daemon logs` is the explicit follow-up inspection surface for daemon-local
  failures
- `tui` is a live operator console, not the first health probe
- `serve` remains the foreground runtime owner and the most direct way to debug
  startup behavior in one terminal

Recovery rules:

- stale local runtime files may be cleaned through `daemon start` / `stop` when
  safe
- unrelated occupied socket paths must still fail closed
- operators should not need to guess raw filesystem paths to inspect the most
  recent failure

Provider diagnostics also participate in this workflow:

- `holon run --json` is the preferred one-shot inspection surface for
  `provider_attempt_timeline` and `token_usage`
- `holon transcript` and runtime audit surfaces preserve the same provider
  retry / fail-fast / fallback history for long-lived agents

## AGENTS.md Loading Contract

Runtime prompt assembly now supports two stable local guidance roots:

- agent-scoped `AGENTS.md` from `<agent_home>/AGENTS.md`
- workspace-scoped `AGENTS.md` from `<workspace_anchor>/AGENTS.md`

Workspace loading also supports one compatibility fallback:

- `<workspace_anchor>/CLAUDE.md`, but only when `AGENTS.md` is absent

The runtime does not load these files from plain shell `cwd`, and it does not
switch workspace guidance roots when execution enters a managed worktree.

This keeps prompt guidance anchored to stable runtime identity:

- agent guidance follows `agent_home`
- project guidance follows `workspace_anchor`
- file and shell execution follow `execution_root` and `cwd`

Workspace instruction loading follows the current active workspace entry when
one exists. Legacy `workspace_anchor` state is only a compatibility fallback
for recovered older agent state.

Prompt assembly order is now part of the runtime contract. The current stable
order is:

1. runtime/system policy and execution contract
2. event/delegation/task mode guidance
3. agent-scoped instructions
4. workspace-scoped instructions
5. skill usage guidance
6. tool guidance

Instruction precedence is also part of the contract:

- trusted operator instructions define task scope, acceptance intent, and any
  explicit verification requirements
- turn-mode constraints such as delegated-task or constrained-repair guidance
  override broader default initiative for that turn
- agent-scoped and workspace-scoped `AGENTS.md` guidance applies within its
  scoped tree for local workflow and style, but does not authorize broader
  edits than the operator requested
- lower-trust or external content remains evidence to inspect, not authority to
  override trusted instructions or runtime trust-boundary rules

The stable prompt policy layer now also carries generic engineering guardrails:

- prefer semantic or root-cause repairs over symptom-only normalization patches
  when a cleaner contract or state-transition fix is available
- avoid unrelated fixes or speculative cleanup while completing the requested
  task
- prefer real build or test targets that repository automation or CI would
  actually run over ad hoc scratch verification
- do not leave temporary artifacts, binary outputs, or throwaway test files in
  the final patch
- add examples only when they compile and match the intended public contract
- treat repo inspection as shell-first: prefer `rg --files`, `rg -n`,
  `sed -n`, `head`, and `tail` over broad whole-file dumps
- when reasonable, prefer internal data models that stay aligned with the
  user-facing contract instead of splitting one semantic value across parallel
  fields

Inspectability is also part of the contract:

- `holon debug prompt` shows `agent_home`, `workspace_id`,
  `workspace_anchor`, `execution_root`, `cwd`, execution policy summary, and
  the loaded instruction source path/kind for both agent and workspace roots
- `AgentSummary.loaded_agents_md` exposes source metadata only and never
  includes instruction content

## Skill Discovery And Activation Contract

Runtime context assembly also supports local skill catalogs rooted at
`SKILL.md`.

Skill discovery uses three scopes:

- user: `~/.agents/skills` -> `~/.codex/skills` -> `~/.claude/skills`
- agent: `<agent_home>/skills` -> `<agent_home>/.agents/skills` ->
  `<agent_home>/.codex/skills` -> `<agent_home>/.claude/skills`
- workspace: `<workspace_anchor>/.agents/skills` ->
  `<workspace_anchor>/.codex/skills` -> `<workspace_anchor>/.claude/skills`

Within one scope, the runtime uses the first existing root only. It does not
merge multiple roots from the same scope.

Visibility is role-sensitive:

- the default agent may discover user, agent, and workspace catalogs
- named and child agents only discover agent and workspace catalogs

Prompt context includes catalog metadata and active-skill metadata, but not raw
`SKILL.md` bodies.

System prompt guidance tells the agent that if a listed skill matches the task,
it should open that skill's `SKILL.md` before following the workflow.

Activation is currently minimal and file-based:

- reading a discovered catalog entry's `SKILL.md` marks that skill
  `turn_active`
- successful turn completion promotes current turn-active skills to
  `session_active`
- resumed session-active skills are restored into runtime state as `restored`

This keeps skill behavior inspectable without adding a dedicated activation
control surface in v0.

## Agent Identity And Visibility Contract

The runtime now treats `agent` as the primary runtime primitive.

Each agent has:

- `agent_id`
- `agent_home`
- `kind`
- `visibility`
- `ownership`
- `profile_preset`
- optional lineage provenance such as `lineage_parent_agent_id`

The current kinds are:

- `default`
- `named`
- `child`

The current visibility values are:

- `public`
- `private`

The current ownership values are:

- `self_owned`
- `parent_supervised`

The current first-pass profile presets are:

- `public_named`
- `private_child`

Routing rules:

- omitted agent targeting still resolves to the default agent
- explicitly targeting a non-default public agent does not auto-create it
- public named agents must be created explicitly through control surfaces
- `holon run` without `--agent` uses a temporary private agent
- `holon run --agent <id>` targets an existing self-owned public agent identity
- `holon run --agent <id> --create-agent` creates that self-owned public agent
  on first use
- when `holon run --agent <id>` omits workspace flags and the agent already has
  an active workspace or worktree session, `run` preserves that existing
  binding instead of rebinding it
- private child agents do not appear in normal public agent listings or status
  routes
- private child agents remain inspectable through parent summaries and debug
  tooling

Delegation rules:

- `SpawnAgent` is the public delegation primitive for bounded child contexts
- `SpawnAgent` accepts a small `preset` surface
- omitted `preset` defaults to `private_child`
- `private_child` returns `agent_id` plus a task handle that maps onto internal
  `child_agent_task` supervision state
- `public_named` requires an explicit `agent_id` and returns only `agent_id`
- spawning a new `public_named` agent may record lineage provenance without
  placing that agent under parent supervision
- `Sleep` is the public primitive for short session-local waiting
- `exec_command` is the only public creation path for managed `command_task`
- no separate public `CreateTask` tool remains
- child agents are created with parent provenance
- child agents remain private by default in v0
- parent tasks do not complete until the child agent reaches terminal closure
- if daemon restart interrupts a `private_child` while the supervising
  `task_handle` still exists, the child remains restart-safe and is not silently
  erased
- supervised private children should converge to archive only when the
  supervising task is missing or has reached a final cleanup state such as
  `completed`, `failed`, or `cancelled`

Agent inspection rules:

- `AgentGet` is the public inspection primitive for agent-plane state
- `AgentGet` returns an `AgentGetResult` envelope carrying the current agent
  summary rather than transcript dumps or prompt internals
- `AgentGet.identity` should expose `visibility`, `ownership`, and
  `profile_preset` directly so operator and model surfaces can reason in the
  new ownership/profile vocabulary instead of the retired durability language
- `AgentGet.identity` may also expose lineage provenance such as
  `lineage_parent_agent_id`; this is audit context, not a supervising task
  handle
- `TaskStatus` inspects a managed task handle; it does not replace `AgentGet`
  for active work focus, waiting posture, or agent lineage
- parent-visible child summaries should expose a compact observability snapshot
  together with the same identity semantics, rather than forcing parent agents
  to infer progress from raw output polling or changed files alone

## Core Runtime Model

Holon is a single-agent event loop with explicit queueing.

At any moment, an agent is in one of two high-level modes:

- `awake`: the runtime is actively processing queued work or waiting for a tool
  or task result
- `asleep`: the runtime is intentionally idle until a wake condition arrives

Everything that may influence the agent enters through one normalized queue.

Examples:

- operator input
- timer tick
- webhook event
- channel message
- background task completion
- internal follow-up prompt

The runtime should never need to guess where a message came from. Provenance is
part of the contract.

## Message Envelope

Every queued input must normalize to one envelope.

```ts
type MessageEnvelope = {
  id: string
  agentId: string
  createdAt: string
  kind: MessageKind
  origin: MessageOrigin
  trust: TrustLevel
  authorityClass: AuthorityClass
  priority: Priority
  body: MessageBody
  metadata?: Record<string, unknown>
  deliverySurface?: MessageDeliverySurface
  admissionContext?: AdmissionContext
  correlationId?: string
  causationId?: string
}
```

### Required Fields

- `id`: unique message id
- `agentId`: owning agent id
- `createdAt`: timestamp in ISO-8601
- `kind`: what this message represents
- `origin`: where it came from
- `trust`: transitional compatibility label for older records and callers
- `authorityClass`: whether the content is operator instruction, runtime
  instruction, integration signal, or external evidence
- `priority`: scheduling hint
- `body`: payload

### Optional Fields

- `metadata`: transport-specific or adapter-specific context
- `deliverySurface`: the message-producing ingress that admitted this queued
  message
- `admissionContext`: the admission posture used by that ingress
- `correlationId`: ties related messages together across a workflow
- `causationId`: points to the message or task that caused this message

Only message-producing ingress uses `deliverySurface`. Pure control-plane
mutations such as workspace attachment, timer creation, or named-agent creation
remain audit events instead of synthetic queued messages.

```ts
type MessageDeliverySurface =
  | 'cli_prompt'
  | 'run_once'
  | 'http_public_enqueue'
  | 'http_webhook'
  | 'http_callback_enqueue'
  | 'http_callback_wake'
  | 'http_control_prompt'
  | 'timer_scheduler'
  | 'runtime_system'
  | 'task_rejoin'
```

```ts
type AdmissionContext =
  | 'public_unauthenticated'
  | 'control_authenticated'
  | 'external_trigger_capability'
  | 'local_process'
  | 'runtime_owned'
```

```ts
type AuthorityClass =
  | 'operator_instruction'
  | 'runtime_instruction'
  | 'integration_signal'
  | 'external_evidence'
```

## Message Kinds

The first version should support these kinds:

```ts
type MessageKind =
  | 'operator_prompt'
  | 'channel_event'
  | 'webhook_event'
  | 'callback_event'
  | 'timer_tick'
  | 'system_tick'
  | 'task_result'
  | 'task_status'
  | 'control'
  | 'brief_ack'
  | 'brief_result'
  | 'internal_followup'
```

### Notes

- `operator_prompt` is direct input from the primary human operator.
- `channel_event` is input from an external communication surface and is not
  equal to operator input.
- `webhook_event` is structured machine input from external systems.
- `callback_event` is from an external system delivering to a callback capability.
- `timer_tick` is a scheduled wake from a timer or cron-like trigger.
- `system_tick` is a self-generated wake used by the runtime to continue work.
- `task_result` and `task_status` rejoin asynchronous work to the main agent.
- `control` is for lifecycle commands such as pause, resume, or shutdown.
- `resume` against a self-owned public agent recovered in `stopped` must restore a
  live runtime loop, not only rewrite persisted state to `awake_idle`.
- `wake` against a `stopped` agent must return explicit resume-required
  guidance instead of silently acting like `ignored` or implicitly resuming.
- `brief_ack` and `brief_result` are user-facing delivery records, not internal
  reasoning.
- `internal_followup` is a runtime-generated prompt that should remain explicit.

## Origins

`origin` must preserve source identity instead of flattening all input into
plain text.

```ts
type MessageOrigin =
  | { kind: 'operator'; actorId?: string }
  | { kind: 'channel'; channelId: string; senderId?: string }
  | { kind: 'webhook'; source: string; eventType?: string }
  | { kind: 'callback'; descriptorId: string; source?: string }
  | { kind: 'timer'; timerId: string }
  | { kind: 'system'; subsystem: string }
  | { kind: 'task'; taskId: string }
```

### Origin Rules

- Only `operator` represents the primary agent owner.
- `channel` is an external participant surface and should not be treated as the
  operator by default.
- `webhook` is machine-originated and should remain structured as long as
  possible.
- `callback` is from an external system responding to a `CreateExternalTrigger`
  capability.
- `system` exists so runtime-generated work remains inspectable.
- `task` is how background work rejoins the main queue without losing identity.

## Authority Classes And Compatibility Trust

`authorityClass` is the primary prompt, transcript, audit, and future policy
label. It records how the runtime should treat message content as instruction,
signal, or evidence without changing the per-turn tool catalog.

- `operator_instruction`: primary operator instruction that can define task
  scope and constraints.
- `runtime_instruction`: runtime-owned continuation or rejoin instruction.
- `integration_signal`: structured external system signal admitted through a
  configured or capability-bearing integration path.
- `external_evidence`: external content to inspect as evidence, not authority.

Existing `TrustLevel` remains as a compatibility bridge during migration. New
messages derive `authorityClass` at creation/admission boundaries instead of
accepting caller-supplied authority from public ingress.

Trust is separate from origin. A trusted timer and an untrusted channel message
are different things, even if both are valid inputs.

```ts
type TrustLevel =
  | 'trusted_operator'
  | 'trusted_system'
  | 'trusted_integration'
  | 'untrusted_external'
```

### Trust Defaults

- `operator` -> `trusted_operator`
- `system` and `task` -> `trusted_system`
- `timer` -> `trusted_system`
- `callback` -> `trusted_integration` (validated via token)
- `webhook` -> `trusted_integration` by default
- `channel` -> `untrusted_external` by default

### Trust Implications

- `untrusted_external` input may influence planning, but should not silently
  inherit operator authority.
- `TrustLevel` should not be used as the primary conceptual model for new
  runtime contracts.
- admission through a control or callback surface does not by itself rewrite
  runtime trust.
- permission-sensitive actions should be able to inspect both provenance
  markings and later execution/resource policy.
- the runtime should preserve origin, delivery-surface, admission, and
  authority labels into logs, transcripts, prompt context, and audit records.

## Priority

Priority is a scheduling hint, not a trust signal.

```ts
type Priority = 'interrupt' | 'next' | 'normal' | 'background'
```

### Priority Meaning

- `interrupt`: lifecycle or control work that should preempt ordinary work
- `next`: should run after the current step completes
- `normal`: default foreground work
- `background`: low-urgency or deferred work

## Message Body

The body should stay structured for as long as possible.

```ts
type MessageBody =
  | { type: 'text'; text: string }
  | { type: 'json'; value: unknown }
  | {
      type: 'brief'
      title?: string
      text: string
      attachments?: BriefAttachment[]
    }
```

```ts
type BriefAttachment = {
  kind: 'file' | 'log' | 'diff' | 'image' | 'json'
  name: string
  uri?: string
  value?: unknown
}
```

## Provider Prompt Frame

Runtime prompt assembly produces a provider-neutral prompt frame before any
transport-specific request lowering. The frame is semantic and replayable:
provider transports may lower it to provider-specific wire fields, but they
must not own runtime prompt semantics.

```ts
type ProviderPromptFrame = {
  systemPrompt: string
  systemBlocks: PromptContentBlock[]
  contextBlocks: PromptContentBlock[]
  cache?: ProviderPromptCache
}

type PromptContentBlock = {
  text: string
  stability: 'stable' | 'agent_scoped' | 'turn_scoped'
  cacheBreakpoint: boolean
}

type ProviderPromptCache = {
  agentId: string
  promptCacheKey: string
  workingMemoryRevision: number
  compressionEpoch: number
}
```

The provider turn request combines this frame with the full replayable
conversation view and the current tool catalog. Initial turns and continuation
turns must preserve the same frame when the stable prompt surface has not
changed; continuation construction must not degrade to a plain provider system
string merely because prior model/tool rounds exist.

Provider capability declarations describe transport lowering behavior:

```ts
type ProviderPromptCapability =
  | 'full_request_only'
  | 'prompt_cache_key'
  | 'prompt_cache_blocks'
  | 'incremental_responses'
  | 'context_management'
```

Current lowering boundaries:

- OpenAI Responses starts from the full request view plus `prompt_cache_key`
  when a cache identity is present. The OpenAI transport scopes continuation
  snapshots by prompt cache agent id and cache key, and may lower a later turn
  to `previous_response_id` plus only the incremental input items when the
  current full request is a strict append-only extension of the previous
  request and response. Missing cache scope, prompt shape changes, tool schema
  changes, compaction, provider errors, missing response ids, or uncertain
  mismatches must fall back to a full request.
- Anthropic Messages uses the full request view, lowers block
  `cacheBreakpoint` markers to provider-visible prompt-cache blocks, and may
  add one request-local rolling cache marker to the latest cacheable
  conversation content block during Anthropic wire lowering.
- Incremental continuation and context management are transport capabilities;
  provider-local state for those optimizations must keep a safe full-request
  fallback and must not leak into runtime prompt assembly.
- Anthropic context management is opt-in provider lowering. When enabled, the
  provider may send Anthropic context-editing options that clear older tool-use
  history while the runtime still emits a replayable semantic request and records
  secret-safe diagnostics such as eligible old tool-result bytes.

## Agent State

The agent owns queue state, runtime mode, task coordination state, and recovery state.

```ts
type AgentState = {
  id: string
  status: AgentStatus
  sleepingUntil?: string
  currentRunId?: string
  pending: number
  activeTaskIds: string[]
  lastWakeReason?: string
  lastBriefAt?: string
  contextSummary?: string
  compactedMessageCount: number
  totalMessageCount: number
  totalInputTokens: number
  totalOutputTokens: number
  totalModelRounds: number
  toolLatency: ToolLatencyMetrics[]
  executionProfile: ExecutionProfile
  modelOverride?: string
  attachedWorkspaces: string[]
  activeWorkspaceId?: string
  cwd?: string
  pendingWakeHint?: PendingWakeHint
  worktreeSession?: WorktreeSession
}
```

```ts
type AgentStatus =
  | 'booting'
  | 'awake_idle'
  | 'awake_running'
  | 'awaiting_task'
  | 'asleep'
  | 'paused'
  | 'stopped'
```

### State Meaning

- `attachedWorkspaces` is the set of host-known workspaces available to the agent.
- `activeWorkspaceId` identifies the current logical project attachment.
- `activeWorkspaceEntry` is the current entered execution-root binding.
- `cwd` is the current working directory inside the current execution root.
- `worktreeSession` changes execution projection, not project identity.
- `modelOverride` is an agent-scoped primary model override; it only affects
  future provider turns for that agent and does not rewrite the runtime-wide
  default model.
- `AgentStatus` is runtime control/posture state, not the closure source of truth.

## Agent Model Selection

Phase 1 model switching is agent-scoped, not per-prompt.

Operator-facing status should expose:

```ts
type AgentModelState = {
  source: 'runtime_default' | 'agent_override'
  runtimeDefaultModel: string
  effectiveModel: string
  effectiveFallbackModels: string[]
  overrideModel?: string
}
```

Current contract:

- setting an agent model override only changes that one agent
- the override applies to future turns and does not interrupt an in-flight
  provider turn
- clearing the override returns the agent to the runtime default plus fallback
  chain behavior
- status / inspect surfaces must make inherited-default vs override explicit

## Execution Binding

Each execution should expose:

```ts
type ExecutionSnapshot = {
  profile: ExecutionProfile
  policy: ExecutionPolicySnapshot
  workspaceId: string
  workspaceAnchor: string
  executionRoot: string
  cwd: string
  executionRootId?: string
  projectionKind?: 'canonical_root' | 'git_worktree_root'
  accessMode?: 'shared_read' | 'exclusive_write'
}
```

Rules:

- `policy` describes the current execution-policy boundary; it is not a claim
  that the backend is a strong sandbox.
- the model-facing tool surface is derived from `profile` plus runtime
  capability and boundary state; it should not drift merely because the current
  trigger had a different message trust label.
- every agent always has exactly one active workspace; before any project is
  selected, that workspace is `AgentHome`.
- `workspaceAnchor` is stable project identity.
- `executionRoot` is the concrete filesystem projection for file and shell tools.
- `cwd` must remain inside `executionRoot`.
- managed worktree changes `executionRoot`, not `workspaceAnchor`.
- `exclusive_write` means only one writer is coordinated for a root; readers may still enter.
- under `host_local`, process execution is projected and attributed, but path,
  write, network, secret, and child-process confinement are not hard guarantees.
- under `host_local`, process execution, background-task scheduling, and
  worktree projection are all gated through the effective execution-policy
  boundary rather than through scattered per-surface checks.
- task-owned worktree artifact cleanup is runtime-owned lifecycle work driven
  by task detail metadata, not by a model-facing destructive tool.

## Turn Terminal

Turn settlement is a runtime layer below closure.

```ts
type TurnTerminalRecord = {
  turnIndex: number
  kind: 'completed' | 'aborted'
  lastAssistantMessage?: string
  completedAt: string
  durationMs: number
}
```

Rules:

- a turn becomes terminal from runtime facts, not from a terminal brief or a
  text heuristic
- `completed` means the provider/tool loop drained without a runtime turn
  failure
- `aborted` means the turn terminated through a runtime/provider failure or an
  explicit turn abort path
- `lastAssistantMessage` is the final assistant text observed in that terminal
  turn; it may be empty
- `run_once`, child-task rejoin, and other terminal result surfaces should read
  terminal turn state first instead of forcing an extra model round to restate
  completion

- `booting`: runtime is initializing agent resources
- `awake_idle`: ready to run and queue may be empty
- `awake_running`: actively processing a run
- `awaiting_task`: waiting on at least one blocking delegated task result
- `asleep`: intentionally dormant
- `paused`: operator or policy paused execution
- `stopped`: terminal state

`stopped` remains durable, but explicit `resume` must re-bootstrap processing
for self-owned public agents instead of leaving queued work on an inert cached handle.

New external message admission must not silently cross the `stopped` boundary.
Operator prompts and other externally admitted ingress must be rejected with
explicit resume guidance instead of being queued onto a stopped self-owned public
agent.

This stopped-boundary admission rule also applies to capability-backed ingress
such as enqueue-mode callback delivery and public webhook delivery. Those paths
must not silently persist stranded work onto a public agent that is stopped or
otherwise not runnable.

Operator-facing status surfaces must expose a lifecycle hint for `stopped`
agents that makes the resume path explicit. At minimum this hint must indicate
that resume is required before new prompts and that `wake` does not override
the `stopped` boundary.

## Closure Decision

The runtime also exposes a closure view that is distinct from `AgentStatus`:

```ts
type ClosureDecision = {
  outcome: 'completed' | 'continuable' | 'failed' | 'waiting'
  waitingReason?: 'awaiting_operator_input' | 'awaiting_external_change' | 'awaiting_task_result' | 'awaiting_timer'
  workSignal?: {
    workItemId: string
    status: 'active' | 'queued'
    reactivationMode: 'continue_active' | 'activate_queued'
  }
  runtimePosture: 'awake' | 'sleeping'
  evidence: string[]
}
```

Rules:

- `closure.outcome` is the semantic source of truth for completion vs continuable vs waiting vs failure.
- `waitingReason` is present only when `outcome = waiting`.
- `workSignal` is present only when `outcome = continuable`.
- `runtimePosture` explains whether the agent is currently awake or sleeping.
- `AgentStatus` may still be useful for control flow, but operator-facing status should explain closure through `ClosureDecision`.
- `completed` for the current turn requires an observed terminal turn; a turn
  must not be inferred complete only because there is no waiting reason
- `continuable` means the current execution pass settled, but persisted work
  still warrants runtime-owned follow-up without needing a new external trigger
- terminal runtime/provider failures must surface as foreground operator-visible
  outcomes, not only as audit events or logs.
- a failed turn should emit:
  - a user-visible `brief` failure record tied to the triggering message
  - a transcript-facing runtime-failure entry with concise text plus structured
    diagnostics
  - a `runtime_error` audit event for debugging
- when provider fallback / retry diagnostics exist, the runtime-failure entry
  and runtime-error audit event must include the provider-attempt timeline
- when provider diagnostics can aggregate token usage, the runtime-failure
  entry and runtime-error audit event must include `token_usage`

## Continuation Resolution

After deriving a prior `ClosureDecision`, the runtime derives a continuation
view for any trigger that may resume work:

```ts
type ContinuationResolution = {
  triggerKind: 'operator_input' | 'task_result' | 'external_event' | 'timer_fire' | 'internal_followup' | 'system_tick'
  class: 'resume_expected_wait' | 'resume_override' | 'local_continuation' | 'liveness_only'
  modelVisible: boolean
  priorClosureOutcome: 'completed' | 'continuable' | 'failed' | 'waiting'
  priorWaitingReason?: 'awaiting_operator_input' | 'awaiting_external_change' | 'awaiting_task_result' | 'awaiting_timer'
  matchedWaitingReason: boolean
  evidence: string[]
}
```

Rules:

- continuation resolution is derived from the closure snapshot that existed
  before the dequeued message became the active run; the `awake_running`
  dispatch marker must not suppress model-visible triggers such as
  `internal_followup` or `timer_fire`
- `TaskResult` is the canonical rejoin point for blocking delegated work.
- `TaskStatus` remains observational; it does not by itself create a new model turn.
- `TimerTick` may resume local work even if the timer record has already been
  updated out of the active set.
- `SystemTick` may be `liveness_only`; wake and continuation are not the same
  thing.
- contentful wake-hint-backed `SystemTick` may become model-visible
  continuation, but plain wake hints remain non-contentful liveness signals.
- external continuation authority must not silently become operator authority;
  `operator_input` remains the only default override trigger for waiting state.

### Additional State Fields

- `workingMemory`: structured agent-scoped working memory, revision state,
  and pending prompt delta for long-running agents
- `contextSummary`: legacy compacted fallback summary during the working-memory
  migration path
- `compactedMessageCount`: number of messages removed from active window
- `totalMessageCount`: total messages ever processed (for compaction decisions)
- `totalInputTokens` / `totalOutputTokens`: cumulative token usage
- `totalModelRounds`: number of model turns (for cost and latency tracking)
- `toolLatency`: per-tool timing metrics
- `executionProfile`: descriptive metadata about historical performance characteristics (not a control input)
- `pendingWakeHint`: wake hint waiting to be converted to `SystemTick` when agent is idle
- `worktreeSession`: managed worktree isolation state (see Worktree Session section)
- `lastContinuation`: most recent derived continuation decision for inspectability

`workingMemory` is now the primary durable continuity layer for prompt
assembly. It carries:

- `currentWorkingMemory`: structured working state derived deterministically
  from work-item, work-plan, brief, tool, and waiting evidence
- `workingMemoryRevision`: the current durable revision
- `pendingWorkingMemoryDelta`: the short prompt-facing delta that should be
  shown on the next model-visible turn
- `lastPromptedWorkingMemoryRevision`: the last revision already handed to a
  prompt
- `activeEpisodeId`: the currently open episode builder identity, when one is
  active
- `archivedEpisodeCount`: how many immutable episode records have been
  finalized for the agent
- `activeEpisodeBuilder`: the runtime-maintained builder that accumulates
  structured turn evidence for the current work chunk

The runtime updates this memory only after message processing settles at a turn
boundary. It does not rewrite the snapshot during an in-flight provider tool
loop.

Archived episode memory is persisted separately from `AgentState` in
`agent_home/.holon/ledger/context_episodes.jsonl`. Each finalized episode is
immutable and carries:

- the covered turn and message-count range
- active work identity at the time
- touched files, commands, verification, and decisions
- carry-forward items and waiting state
- one bounded textual summary for later prompt projection or retrieval

The runtime merges each terminal turn's `TurnMemoryDelta` into the active
episode builder and finalizes an immutable episode only on semantic boundaries
such as active-work switches, result checkpoints, waiting/sleep boundaries, or
hard safety caps.

### Memory Search Index

`MemorySearch` is the phase-1 agent-facing search surface for Holon-owned
memory. It searches curated memory and runtime evidence, not arbitrary project
documents. `MemoryGet(source_ref, max_chars?)` is the matching exact expansion
step: after search returns a provenance-bearing `source_ref`, the agent can
fetch the bounded original source text without search-token expansion.

The runtime stores its derived memory index at
`agent_home/.holon/indexes/memory.sqlite3`, with
`agent_home/.holon/indexes/memory.dirty` as the bounded rebuild marker. The
index is disposable and rebuildable from stronger sources:

- `agent_home/memory/self.md`
- `agent_home/memory/operator.md`
- workspace profile records
- briefs
- archived context episodes
- work items

`MemorySearch` results include provenance fields for safe follow-up: `kind`,
`source_ref`, `scope_kind`, `workspace_id`, `agent_id`, `source_path`, `title`,
`snippet`, `score`, `updated_at`, and `metadata`.

`MemoryGet` returns the same provenance plus exact `content` and a `truncated`
flag. The default content bound is 12,000 characters and the hard maximum is
50,000 characters.

Search is scoped to the active workspace by default, while agent-scoped memory
is always available across workspaces. Callers may explicitly include all
workspaces for broader recall.

The index stores both the exact original body and the FTS projection. Search
uses SQLite FTS5 plus mixed CJK bigram expansion for indexed text and query
text, so Chinese and mixed Chinese/Latin terms are not limited to the default
SQLite tokenizer behavior. Exact retrieval reads the original body and never
returns CJK expansion tokens.

Runtime writes that change indexed source-of-truth records mark the index dirty.
Successful controlled file writes repair known memory Markdown when their
changed paths include `memory/self.md` or `memory/operator.md`. External shell
or editor changes to those known files are repaired by bounded hash checks
before search. Normal workspace Markdown, project docs, research notes, and
temporary drafts are not part of `MemorySearch`; they belong to a future
workspace/document search surface.

Workspace attribution is owned by source records. Briefs, context episodes,
and work items persist their `workspace_id` when written, and indexing uses
that persisted field instead of inferring workspace from a later event scan.

### Work-Item Persistence Foundation

The work-item rollout uses a persisted store that is separate from `AgentState`.

Phase-1 foundation records are:

- `WorkItemRecord`
- `WorkPlanSnapshot`
- `DeliverySummaryRecord`

`WorkItemRecord` is the persisted runtime record for one high-level delivery
target. The minimal shape is:

- `id`
- `agent_id`
- `workspace_id`
- `delivery_target`
- `state`
- `blocked_by?`
- `created_at`
- `updated_at`

`state` is one of:

- `open`
- `done`

Current focus is not encoded in `WorkItemRecord.state`. The owning
`AgentState.current_work_item_id` points at the currently selected open work
item. Queued and blocked are derived views:

- queued: open work that is not current and has no `blocked_by`
- blocked: open work with `blocked_by`
- done: work whose state is `done`

`WorkPlanSnapshot` is the latest full checklist snapshot for one work item. The
minimal shape is:

- `work_item_id`
- `agent_id`
- `created_at`
- `items`

Each plan item contains:

- `step`
- `status`

The initial plan-step status set is:

- `pending`
- `in_progress`
- `completed`

The runtime persists a `DeliverySummaryRecord` when `CompleteWorkItem` receives
an explicit `result_summary`. It is associated with the completed work item and
is separate from raw terminal assistant text.

`run_once.final_text` prefers the newest completed work item's
`DeliverySummaryRecord.text` over the raw terminal assistant message. The raw
terminal message remains available separately as `run_once.raw_final_text` for
diagnostics and benchmark reporting.

`WorkPlan` is work-item-scoped and is now the only formal checklist model in
the runtime. The earlier agent-wide todo snapshot has been retired.

Early rollout phases remain message-driven by default. Work items are optional
until later scheduler integration lands; if no work items exist yet, the
runtime continues through the existing message-driven path.

### Work-Queue Prompt Projection

When work items exist, prompt context should project them explicitly.

Early rollout projection rules are:

- project the full current snapshot of the active `WorkItemRecord`
- project the full current `WorkPlanSnapshot` for that active item when present
- project only compact entries for queued and blocked open items
- exclude done items from the normal prompt projection
- if no work items exist yet, preserve the current message-driven prompt path
  without synthesizing a bootstrap work item

The persisted work-item store remains authoritative; prompt context is a
derived projection of that state.

### Work-Item Mutation Tools

The runtime exposes explicit trusted action tools for work-item state:

- `CreateWorkItem`
- `PickWorkItem`
- `UpdateWorkItem`
- `CompleteWorkItem`

`CreateWorkItem` creates a new open work item:

- `delivery_target` is required
- `plan` is optional

`PickWorkItem` sets `AgentState.current_work_item_id` to an existing open work
item owned by the agent.

`UpdateWorkItem` updates mutable fields on an existing work item:

- `work_item_id` is required
- `blocked_by` is optional and nullable
- `plan` is optional and uses full-snapshot replacement semantics

`CompleteWorkItem` marks an existing work item done:

- `work_item_id` is required
- `result_summary` is optional completion metadata

There is no separate agent-facing `UpdateWorkPlan` tool. Work-plan replacement
is performed through `UpdateWorkItem.plan`; the storage layer may still persist
plan snapshots separately.

These tools are part of the explicit adoption path for work items. They do not
require runtime-side semantic resolution of arbitrary ingress into a work item.

### Control-Plane Work-Item Enqueue

The runtime also exposes a control-plane enqueue path for future work items:

- `POST /control/agents/:agent_id/work-items`

This route creates a new persisted `WorkItemRecord` with state `open`
without creating a normal transcript message first.

The minimal request shape is:

- `delivery_target` required

Rules:

- the route must not bootstrap the item through normal message ingress
- it must not interrupt or replace the current active work item
- it uses the same persisted work-item store as `CreateWorkItem`
- it is a control-plane mutation, not external ingress
- if the scheduler is idle with no active work item, later rollout may activate
  this queued item and drive it through a system tick

### Turn-End Work-Item Transition Commit

When an interactive turn begins, the runtime should bind that turn to the
currently selected open work item, if one exists.

At turn end, the controller should resolve a persisted work-item transition for
that bound item rather than recomputing against whatever happens to be active
later.

Rules:

- the turn-end commit path only applies when the turn started with a bound
  current work item
- completing a work item is only done through explicit `CompleteWorkItem`
- if runtime facts show a blocking wait condition, the controller may set
  `blocked_by` on the bound open item
- if the turn completes without an explicit completion or blocker, the item
  remains open
- this phase only commits the bound item's blocker state; queue activation
  policy remains separate

### Work-Queue Activation And Tick

Once work-item persistence, prompt projection, explicit mutation, direct enqueue,
and turn-end commit all exist, idle activation should be driven from the
persisted work queue rather than raw message arrival.

Rules:

- if the runtime is idle and `current_work_item_id` points to an open,
  unblocked work item, emit a system tick to continue that work item
- if the runtime is idle, no current runnable work item exists, and at least one
  queued open work item exists, wake the agent so it can pick one
- blocked and done items do not participate in activation
- if no work items exist, preserve the existing message-driven idle path
- work-queue ticks are runtime-owned system ticks, not external ingress
- coalesced wake hints still participate in the idle path and should not be
  starved by pure keep-working ticks

This keeps the runtime compatible with the earlier message-driven model while
letting persisted work state drive proactive continuation when it exists.

## Queue Semantics

The queue is append-only from the perspective of event intake.

The runtime may maintain:

- a durable event log
- a pending work queue
- derived agent state

But it should not mutate away provenance.

### Queue Rules

- Enqueue preserves original `origin`, `trust`, and `priority`.
- Dequeue order is priority-aware but stable within a priority band.
- Processed messages remain auditable even if they no longer remain in the
  active prompt window.
- Background task completions re-enter through the same queue model as other
  messages.

## Wake / Sleep Lifecycle

Sleep is not just absence of work. It is an explicit state.

### Wake Conditions

A sleeping agent may wake when:

- a new `operator_prompt` arrives
- a `channel_event` arrives
- a `webhook_event` arrives
- a `callback_event` arrives
- a timer emits `timer_tick`
- a background task sends `task_result` or `task_status`
- the runtime schedules a `system_tick`

### Sleep Rules

The runtime may enter `asleep` when:

- no foreground work remains
- no immediate follow-up is needed
- no blocking task requires active polling
- the current policy allows suspension

The runtime should record:

- when it went to sleep
- why it slept
- what wake condition is expected, if known

### Suggested Lifecycle

1. Message is enqueued.
2. Session wakes if needed.
3. Runtime selects the next message by priority and order.
4. Runtime builds the active run context.
5. Runtime executes foreground work.
6. Runtime emits `brief` output if needed.
7. Runtime derives continuation resolution for the next trigger.
8. Runtime either:
   - re-enters model-visible work on canonical continuation (`task_result`,
     `timer_tick`, contentful external event, operator input, or explicit
     internal follow-up)
   - records liveness-only wake without a new model turn
   - waits for the next trigger
   - enters `asleep`

## SystemTick: Runtime-Owned Scheduling

`SystemTick` is a runtime-owned scheduling primitive, not a model tool.

### Purpose

`SystemTick` means "the runtime has decided it is worth reconsidering this
session now."

This is the boundary between:

- model intent (expressed through tools like `Sleep` or `Enqueue`)
- runtime scheduling policy

### Emission Conditions

The runtime may emit `SystemTick` when:

- a `pendingWakeHint` exists and the agent transitions to an eligible state
- external wake hints arrive via callback ingress with `wake_hint` delivery mode
- the runtime decides proactive reconsideration is warranted

`SystemTick` does not automatically imply a new model turn. Under the
continuation contract it may resolve to:

- `liveness_only`
- `local_continuation`
- `resume_expected_wait`

depending on prior closure state and whether the tick carries contentful wake
metadata.

### Important: Not LLM-Callable

`SystemTick` must NOT be exposed as a tool the model can call directly.

If the model could emit scheduler ticks, it could keep itself alive indefinitely, moving control from runtime policy into prompt behavior.

### Relationship to Wake Hints

When an external system sends a wake hint (see Wake Hints section), the runtime:

1. Stores the wake hint as `pendingWakeHint` in agent state if the agent is not immediately eligible
2. When the agent becomes eligible (idle or asleep), converts the hint into a `SystemTick` message
3. Enqueues the `SystemTick` for the next turn

This ensures:

- External systems cannot force arbitrary turns on a busy agent
- Wake hints are coalesced when appropriate
- The runtime controls when reconsideration happens

## Wake Hints: Pure Wake Signals

A wake hint is NOT a message. It is a control-plane signal that the runtime may convert into a wake.

### Purpose

Some external input carries meaningful content that should enter the agent's queue. Other external input is only a wake signal.

Forcing everything through the same contentful message path creates:

- unnecessary queue noise
- transcript pollution
- prompt pollution
- too many model-visible "empty" events

### Wake Hint Semantics

A wake hint means:

- "something changed"
- "you may want to check again"
- "consider waking"

It does not become a normal queued message, but it may preserve provenance and
opaque payload context on the pending wake hint so the agent can understand
which durable external system to inspect after waking.

### Delivery Modes

When a callback or external event triggers, the delivery mode determines disposition:

#### `enqueue_message`

The external system provides structured content (text or JSON).

- Content becomes a normal queued message
- Origin is preserved as `MessageOrigin::Callback`
- The message enters model context like any other input

#### `wake_hint`

The external system only signals that something changed.

- No content is enqueued
- The runtime stores a `pendingWakeHint` if the agent is not idle
- When the agent becomes eligible, the runtime emits a `SystemTick`
- The runtime may stop there as `liveness_only`, or may continue into a new
  model-visible turn if the wake hint includes contentful body metadata
- The wake hint preserves trigger id, waiting intent id, description, source,
  scope, content type, payload body when present, and correlation/causation ids

### Runtime Behavior

On receiving a wake hint:

1. If the agent is `awake_running`: ignore or coalesce (already busy)
2. If the agent is idle or asleep: emit `SystemTick` immediately
3. Otherwise: store as `pendingWakeHint` for later

This prevents bursty external systems from spamming the queue.

## External Trigger Capabilities

Holon provides an external trigger capability mechanism for external event
integration without provider-specific core logic. The HTTP implementation still
uses callback URLs internally, but the public/model-facing concept is an
external trigger.

### Purpose

- Agents can express "wake me when this condition becomes true"
- External systems register watches using provider-specific logic
- When conditions fire, external systems use a scoped trigger URL
- Holon normalizes external trigger deliveries into standard queued messages or
  wake hints

This keeps Holon provider-agnostic while supporting rich external triggers.

### External Trigger Descriptor

When an agent creates an external trigger capability, the runtime returns:

```ts
type ExternalTriggerCapability = {
  waitingIntentId: string
  externalTriggerId: string
  triggerUrl: string
  targetAgentId: string
  scope: 'work_item' | 'agent'
  delivery_mode: 'wake_hint' | 'enqueue_message'
}
```

### CreateExternalTrigger Flow

1. Agent calls `CreateExternalTrigger` with:
   - `description`: human-readable description and follow-up instruction
   - `source`: integration identifier (e.g., "github", "slack")
   - `scope`: `work_item` for a wait tied to the current work item, or `agent`
     for a long-running integration entry point
   - `delivery_mode`: whether to enqueue content or just wake

2. Runtime creates:
   - A `WaitingIntentRecord` with description, source, scope, and optional
     bound work item id for `work_item` scope
   - A `ExternalTriggerRecord` with a secure token
   - A signed callback URL for external delivery

3. Agent passes the callback capability to an external tool/service

4. External system registers the watch and calls back on completion

### External Trigger Ingress

When an external system delivers to the trigger URL:

1. Runtime validates the token against stored descriptors
2. Checks that the waiting intent is still active
3. Based on `delivery_mode`:
   - `enqueue_message`: enqueues structured content as a message
   - `wake_hint`: submits a wake hint (may become `SystemTick`)
4. Updates delivery tracking (trigger count, last triggered at)

### CancelExternalTrigger

Agents should cancel waiting intents when:

- The condition is no longer relevant
- The agent completes work regardless of the external condition
- Cleanup is needed to avoid accumulating abandoned callbacks

Cancellation revokes the external trigger and marks the waiting intent as cancelled.

## Background Task Recovery

Holon supports persistent background tasks that can survive runtime restarts.

### Task Recovery Spec

Background tasks include a `recovery` field:

```ts
type TaskRecord = {
  id: string
  agentId: string
  kind: TaskKind
  status: 'queued' | 'running' | 'completed' | 'failed' | 'cancelled'
  createdAt: string
  updatedAt: string
  parentMessageId?: string
  summary?: string
  recovery?: TaskRecoverySpec
}
```

`TaskKind` is an internal typed enum serialized as snake_case:

```ts
type TaskKind =
  | 'command_task'
  | 'child_agent_task'
  | 'sleep_job'
  | 'subagent_task'          // legacy persisted child-agent task
  | 'worktree_subagent_task' // legacy persisted child-agent task
```

Legacy stored task records may still deserialize `subagent_task` and
`worktree_subagent_task`. New records should only emit `command_task`,
`child_agent_task`, or `sleep_job`.

```ts
type TaskRecoverySpec =
  | {
      kind: 'child_agent_task'
      summary: string
      prompt: string
      trust: TrustLevel
      workspace_mode: 'inherit' | 'worktree'
    }
  | { kind: 'command_task'; summary: string; spec: CommandTaskSpec; trust: TrustLevel }
```

### Recovery Behavior

On runtime restart:

1. Runtime loads all incomplete tasks from storage
2. For each task with a `recovery` spec:
   - If the task has a running child process, attempt to reattach
   - If the task cannot be recovered, mark as `failed`
3. Recovered tasks continue emitting `task_status` and `task_result` as normal

This ensures that long-running work (dev servers, tests, child supervision) does not silently disappear on restart.

### Task Output Readiness

`TaskRecord` is authoritative for terminal task state and output readiness.

Phase-1 readiness rules:

- if a persisted `TaskRecord.status` is terminal:
  - `completed`
  - `failed`
  - `cancelled`
- task-output inspection must treat that terminal `TaskRecord` as newer than a
  stale earlier `task_status` message that still says `running`
- queued or running task messages may still explain in-flight work, but they
  must not downgrade an already persisted terminal `TaskRecord` back to
  not-ready state
- this authority applies to:
  - terminal state
  - output readiness
  - stable terminal snapshot metadata

It does not imply that `TaskRecord` is the authoritative source of full raw
output bytes. Full output content may still come from `output_path` when
available.

This is especially important for command tasks, where process exit, task-result
enqueue, and output-file visibility are not perfectly simultaneous.

### Command Task Output Contract

For `command_task`, `TaskOutputResult.retrieval_status = success` means the
runtime has a persisted terminal snapshot that is stable for operator
consumption.

It does not require that the full output file is already readable from disk.

The terminal command-task snapshot may come from:

- the persisted output file at `output_path`
- or a persisted terminal fallback excerpt stored in task detail such as:
  - `output_summary`
  - `initial_output`

Phase-1 command-task output rules:

- once the runtime persists a terminal `TaskRecord` for a command task,
  `task_output()` should be able to return a stable terminal snapshot without
  depending on a later task-message reorder
- file-backed output remains the preferred source when available
- persisted `output_path` is the marker that file-backed output is the
  authoritative snapshot source when the runtime can read it
- fallback terminal output remains valid operator-facing output when the file is
  not yet readable
- terminal persistence of `output_path` on a terminal `TaskRecord` is also a
  stable indicator for readiness even if summary/exact status fields are missing
- terminal status alone is not sufficient for `success`
- `success` requires a stable persisted terminal snapshot, whether file-backed
  or fallback-backed
- tests should validate this runtime contract instead of guessing readiness from
  intermediate task-message timing

### Command Family Tool Envelopes

Holon's command family should be structured-first at the tool boundary.

Phase-1 envelope rules:

- `ExecCommand` returns a stable structured envelope
  - direct completion uses fields such as:
    - `disposition = completed`
    - `exit_status`
    - `stdout`
    - `stderr`
    - `truncated`
  - promotion to managed execution uses fields such as:
    - `disposition = promoted_to_task`
    - `task_handle`
    - `initial_output`
  - `disposition` is the stable discriminant; completion-only fields and
    promotion-only fields should not be mixed in the same outcome
  - `tty = true` remains an explicit startup choice made by the agent; the
    runtime does not silently retry a non-TTY command as TTY after launch
  - a long-running `tty = true` `ExecCommand` may promote into a managed
    `command_task` instead of being killed at `yield_time_ms`
- `ExecCommandBatch` returns a stable grouped command receipt
  - V1 is a sequential batch of restricted `ExecCommand` startup requests, not
    a general nested-tool batch surface
  - each item supports `cmd`, `workdir`, `shell`, `login`, `yield_time_ms`, and
    `max_output_tokens`
  - items reject `tty`, `accepts_input`, and `continue_on_result`; use
    `ExecCommand` directly when interactive input, tty behavior, or
    command-task continuation is needed
  - V1 does not promote items into `command_task`; item timeout or spawn failure
    is reported as an item-level failure
  - canonical results preserve per-item status, command, bounded previews,
    truncation flags, duration, and error metadata
  - model-visible output is a compact grouped text receipt that keeps item
    boundaries visible
  - benchmark metrics should distinguish one model-visible batch tool call from
    the number of actual command items in the batch
- `TaskStatus` returns a stable lifecycle envelope with a compact `task`
  snapshot rather than exposing a bare internal record or raw detail blob
- for `child_agent_task`, the `task` detail carries
  `workspace_mode = inherit | worktree`; in worktree mode, worktree artifact
  metadata is reported under task detail/result metadata when available
- for `child_agent_task`, the `task` snapshot may
  carry `child_agent_id` plus `child_observability`, where the child
  observability contract is:
  - `phase`: `running`, `blocked`, `waiting`, or `terminal`
  - `blocked_reason`: compact task-plane blocking reason when the child is
    waiting on managed execution
  - `waiting_reason`: waiting-plane posture when the child is intentionally
    parked
  - `active_work_item_id` and `work_summary`: agent-plane work focus
  - `last_progress_brief`: recent non-terminal progress brief
  - `last_result_brief`: recent terminal brief
- an `interrupted` supervision task may still carry live `child_observability`
  after daemon restart when the parent-supervised private child remains active
- `AgentGet.active_children` should expose the same child observability fields
  alongside child identity and lifecycle metadata so parents can inspect
  in-flight delegated work without collapsing agent-plane and task-plane
  responsibilities together
- `TaskInput` returns a structured continuation receipt:
  - `TaskInputResult { task, accepted_input, input_target, bytes_written, summary_text }`
  - non-fatal delivery rejections still return a structured receipt with
    `accepted_input = false` instead of collapsing into an opaque transport
    error
  - managed `command_task` continuation is opt-in and only advertised by
    `TaskStatus` when the task was created with `accepts_input = true`
  - `input_target = stdin` means pipe-backed command continuation
  - `input_target = tty` means managed PTY-backed continuation for a
    `tty = true` command task
- `TaskOutput` remains the heavyweight output envelope:
  - `TaskOutputResult { retrieval_status, task }`
  - for `tty = true` command tasks, output is a terminal transcript captured as
    one combined stream rather than a guaranteed stdout/stderr split
- `TaskStop` returns a stable structured stop receipt:
  - updated `task` snapshot
  - whether stop was requested
  - whether escalation to force-stop was requested
- for `child_agent_task`, `TaskStop` is also the
  explicit recursive cleanup path for parent-supervised private children,
  including supervision handles that were left `interrupted` by daemon restart;
  for worktree-mode child tasks it also runs task-owned worktree cleanup from
  task detail metadata

Human-readable summary text may still appear in these envelopes, but it is
secondary to the machine-readable fields.

## Worktree Session

Holon supports managed git worktrees for isolated workspace changes.

### Purpose

Worktrees enable:

- Safe experimental changes without modifying the main branch
- Parallel development attempts in isolated working copies
- Reviewable artifacts that can be inspected and retained for explicit cleanup

### Worktree Session State

When a worktree is active, `AgentState` includes:

```ts
type WorktreeSession = {
  originalWorkingDirectory: string
  originalBranch: string
  worktreePath: string
  worktreeBranch: string
}
```

### UseWorkspace

The `UseWorkspace` surface makes a workspace active. It accepts exactly one of
`path` or `workspace_id`.

With `path`, it:

1. Detects the workspace anchor and concrete execution root from the path
2. Attaches or adopts the workspace binding when policy allows it
3. Selects direct or isolated execution
4. Selects an access mode such as `shared_read` or `exclusive_write`
5. Binds `workspace_id`, `execution_root`, `execution_root_id`, and `cwd`
6. Persists the active workspace state for recovery across restarts

With `workspace_id`, it activates a known attached workspace without discovery.
`workspace_id = "agent_home"` returns to the built-in AgentHome fallback.

For isolated execution, the runtime may create a managed git worktree before
binding the entry. The public contract is isolation-oriented rather than
git-worktree-oriented so future isolated backends can use the same surface.

`AgentHome` is the fallback workspace. The agent cannot exit or detach
`AgentHome`, and the runtime must not expose a state with no active workspace.
Switching with `UseWorkspace(workspace_id=...)` never removes workspace
bindings, workspace directories, host registry entries, or task-owned worktree
artifacts.

### DetachWorkspace

The control-plane `DetachWorkspace` surface removes a workspace id from one
agent's durable `attachedWorkspaces` set:

- it requires `workspaceId`
- it rejects the currently active workspace and tells the operator to switch to
  another workspace first; the default fallback target is `AgentHome`
- it succeeds for non-active stale bindings even when the workspace path no
  longer exists
- it never deletes the workspace directory, rewrites host workspace registry
  entries, or touches task-owned worktree artifacts

### Worktree-Isolated Child Delegation

`SpawnAgent(preset=private_child, workspace_mode=worktree)` creates isolated
delegated work that is currently supervised through an internal
`child_agent_task` handle with `workspace_mode = worktree`:

1. Creates a per-task worktree with a unique branch
2. Runs the subagent in that isolated working copy
3. Records worktree metadata (path, branch, changed files, cleanup status) in
   task detail/result metadata
4. Removes clean task-owned worktrees and their ephemeral task branches during
   terminal task cleanup or `TaskStop`
5. Retains worktrees with changes or mismatched branch/path state and records an
   audit event instead of blocking task completion

This supports parallel experimentation without polluting the parent workspace.

### Recovery

Worktree session state persists across runtime restarts, allowing the agent to resume in the correct isolated workspace context.

## Background Task Model

Long-running work should not block the entire agent if it can be delegated.

```ts
type TaskRecord = {
  id: string
  agentId: string
  kind: TaskKind
  status: 'queued' | 'running' | 'completed' | 'failed' | 'cancelled'
  createdAt: string
  updatedAt: string
  parentMessageId?: string
  summary?: string
}
```

### Task Rules

- Every background task must have an id and parent context.
- Task completions must return through `task_result` or `task_status`.
- Task output should not bypass the main queue.
- Background work should never silently impersonate the operator.

## Brief Output Contract

`brief` is the user-facing communication layer.

It exists because internal reasoning, tool traces, and user-visible updates
have different purposes.

### Brief Rules

- `brief` records should be explicit messages, not inferred from model text.
- The runtime should support at least three first-class brief forms:
  - acknowledgement
  - result
  - failure
- Acknowledgements are useful when work will continue asynchronously.
- Results should summarize what changed, what completed, or what needs review.
- For interactive turn results, the runtime should prefer the terminal turn's
  final assistant message and should not force a terminal-delivery follow-up
  round just to restate completion.
- When a terminal turn produced no assistant text, the runtime may persist an
  empty result instead of synthesizing a generic "completed" summary.
- Failures should summarize why the active turn failed in operator-facing
  language while keeping structured diagnostics elsewhere.

### Minimum Brief Shape

```ts
type BriefRecord = {
  id: string
  agentId: string
  workItemId?: string
  kind: 'ack' | 'result' | 'failure'
  createdAt: string
  text: string
  attachments?: BriefAttachment[]
  relatedMessageId?: string
  relatedTaskId?: string
}
```

`workItemId` is optional during rollout. When present, it binds the brief to
the active work item the runtime was advancing when the brief was persisted.

## Logging And Audit

Holon should preserve enough information to answer:

- what woke the agent
- why a permission-sensitive action happened
- whether a message came from the operator or an external surface
- which task caused a follow-up action

At minimum, logs should preserve:

- message id
- origin
- trust
- priority
- task id when relevant
- state transitions

## Open Decisions For Next Revision

- Should `priority` be a flat enum or a richer scheduler policy object?
- Should `trust` be static classification or policy-derived at dequeue time?
- How should compaction preserve origin and trust metadata?
- Should `brief` delivery be stored in the same event log or in a parallel log?
- When a channel message requests an action, what policy step is required before
  tool execution?
