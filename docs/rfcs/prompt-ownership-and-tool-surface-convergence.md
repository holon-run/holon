---
title: "RFC: Prompt Single-Owner Architecture and Tool Surface Convergence"
date: 2026-11-27
status: draft
---

# RFC: Prompt Single-Owner Architecture and Tool Surface Convergence

## Summary

Holon already separates physical prompt placement (`SharedPrefix` versus
`ScopedSuffix`) from cache invalidation scope (`PromptStability::{Stable,
AgentScoped, TurnScoped}`), isolates runtime clock samples to the conversation
tail, and keeps core runtime contracts harness-owned rather than delegating
behavioral rules to model-specific catalog prompts.

However, model-facing guidance has grown across three overlapping static layers:

1. shared and scoped sections in `crates/holon-context/src/system_prompt.rs`
2. tool-family Markdown manuals in `crates/holon-context/src/tool_guidance/`
3. per-tool schema descriptions in `crates/holon-tools/src/tool/tool_descriptions/`

Together with roughly 46 model-visible built-in tool schemas, these static prose
layers exceed 90 KB of source text (~18.3k prose tokens before JSON schemas and
dynamic context). More importantly, several runtime facts now have multiple
owners that drift or contradict one another, coarse tool-family gates inject
unrelated operational manuals into ordinary coding turns, and low-frequency
discovery or provisioning operations occupy permanent schema slots.

This RFC defines:

- a **five-layer single-owner prompt model (`L1`–`L5`)** where each runtime fact
  has exactly one authoritative home positioned at the earliest decision point
  where the model needs it
- **five hard runtime boundaries** governing what must remain a native built-in
  tool versus what should move to the `holon` CLI plus embedded versioned
  manuals (`holon docs`)
- a **four-tier convergence plan** for the 50 built-in tools
- **prompt-topology snapshot and cache-stability rules** that pair with the
  existing tool schema inventory check

## Motivation and Problem Statement

### 1. Cross-Layer Duplication and Drift

Without a strict single-owner rule across `holon-context` and `holon-tools`,
incremental fixes repeatedly append the same prose to system contracts, tool
guidance, and tool descriptions:

- **Direct contract contradiction:** `runtime_scheduling_contract` in
  `crates/holon-context/src/system_prompt.rs` still states that `WaitFor`
  resolves ownership from an explicit `work_item_id` before the execution-bound
  WorkItem and that `task_result` waits must match the task's owner, whereas
  `resolve_wait_work_item_id` in `crates/holon-tools/src/tool/tools/wait_for.rs`
  and `tool_wait_for.md` prioritize `execution_binding` first (ignoring a
  conflicting explicit selector with receipt disclosure) and allow any WorkItem
  of the same agent to hold the wait without migrating task ownership.
- **Multi-copy prose bloat:**
  - `tool_wait_for.md` (~3.7 KB) and `tool_descriptions/wait_for.md` (~3.8 KB)
    ship near-duplicate multi-paragraph manuals in the same request alongside
    `runtime_scheduling_contract` and `tool_use_contract`.
  - Pending-report continuation rules (`awaiting_final_report` and
    `awaiting_completion_report`) appear four times across `tool_wait_for.md`,
    `wait_for.md`, `tool_work_item_write.md`, and `complete_work_item.md` before
    any deferred state exists.
  - `ripgrep` (`-r` versus `-n`) warnings appear four times across `ExecCommand`
    and `ExecCommandBatch` guidance and descriptions.
  - `holon tail --agent` versus `TaskStatus` versus `GetAgent` observation rules
    appear four times across `tool_use_contract`, `tool_agent_invocation.md`,
    `tool_get_agent.md`, and `tool_task_control.md`.
  - File-reference and Markdown link rules appear across `reporting_contract`,
    `file_reference_contract`, and `tool_workspace.md`.
  - Local App routing and trust rules appear both unconditionally in
    `holon_app_mechanism` and conditionally in `tool_holon_cli.md`.
  - Supervised child deletion policy is split across `delete_agent.md`,
    `invoke_agent.md`, and `tool_holon_cli.md`.

### 2. Coarse Tool-Family Gating ("Collateral Prompt Tax")

- Enabling `ExecCommand` unconditionally injects `tool_holon_cli.md` (~5.0 KB),
  loading documentation search URLs, Local App SDK methods, managed skill CLI
  subcommands, and child-deletion CLI flags into every shell turn.
- Enabling any single member of `CreateAgent | InvokeAgent | SendAgentMessage`
  emits the entire `tool_agent_invocation.md` (~7.8 KB).
- Read-only WorkItem surfaces (`GetWorkItem` / `ListWorkItems`) still trigger
  `tool_work_item_scheduling.md` wording that references write tools.
- `constrained_repair_section` matches English-only substrings (`"fix only"`,
  `"src/"`, `"run exactly:"`), leaving non-English operator instructions
  uncovered while adding brittle special-case branching.

### 3. Governance and Cache-Stability Gaps

- Holon enforces drift checks for CLI flags, OpenAPI, HTTP routes, status enums,
  and `model-tool-schema-inventory.json`, but lacks a checked-in snapshot gate
  for rendered prompt section topology and shared-prefix budgets. Meanwhile,
  unit tests in `crates/holon-context/src/tools/tests.rs` assert exact filler
  sentences, impeding safe consolidation.
- `EffectivePrompt::with_media_tool_defaults` injects live availability
  (`available` versus `unavailable`) into an `AgentScoped` system section and
  recomputes `prompt_cache_key`, allowing transient route-health fluctuations to
  invalidate the agent's cached system prefix.
- Turn-local checkpoint prompts live as inline string constants inside runtime
  turn orchestration rather than auditable prompt assets in `holon-context`.

## Goals

- Preserve Holon's core product invariants: explicit authority and provenance,
  durable WorkItem recovery, non-polling `WaitFor` continuations, accurate
  delegation/reply routing, and self-contained operator briefs.
- Retain the existing [Shared System Prompt Prefix](./shared-system-prompt-prefix.md),
  [Anthropic Turn-Scoped Cache Prefix](./anthropic-turn-scoped-cache-prefix.md),
  and [Runtime Time Context](./runtime-time-context.md) contracts without
  introducing a second cache-key scheme or inline string sentinels.
- Assign every model-facing rule to a single authoritative layer (`L1`–`L5`),
  cutting full-surface static prose by ~65%–70% and narrowing the default
  model-visible tool catalog from ~46 tools toward ~24 cohesive primitives.
- Pair prompt and tool-surface changes with deterministic topology snapshots and
  behavioral regression gates.

## Non-Goals

- Chasing minimal coding-CLI token counts (~500 tokens) by discarding cross-turn
  recovery, authority, or scheduling contracts.
- Moving core turn-coupled or security-sensitive runtime primitives into shell
  CLI wrappers.
- Adopting model-catalog-owned personalities or per-vendor behavioral forks as
  the primary system contract.
- Breaking no-shell (`process_execution_exposed = false`) agent profiles.

## Five-Layer Single-Owner Architecture

Every model-facing fact must reside in exactly one of five layers, ordered by
when the model first needs the information to act safely:

### L1: Tool JSON Schema (`schemars` / Provider `tools[]`)

- **Sole responsibility:** input field names, scalar/enum types, structural
  unions, and required-versus-optional constraints.
- **Excluded:** prose workflow tutorials, positive/negative JSON call examples
  that duplicate schema validation, and cross-tool orchestration policy.

### L2: Single-Tool Descriptions (`crates/holon-tools/src/tool/tool_descriptions/`)

- **Sole responsibility:** what one tool does, its preconditions, key defaults,
  and immediate side-effect boundary.
- **Budget guideline:** ~25–60 tokens for ordinary read/query tools; ~80–180
  tokens for high-consequence lifecycle or mutation tools.
- **Excluded:** multi-tool selection trees, CLI fallback tutorials, and post-call
  continuation instructions that belong in `L4` receipts.

### L3a: Shared System Core (`crates/holon-context/src/system_prompt.rs`)

Consolidate the 17 fixed `SharedPrefix` sections into **7 orthogonal stable
sections** (~1,100–1,350 tokens total):

1. `identity` — neutral, long-lived Holon agent identity.
2. `authority_and_trust` — merges `instruction_precedence`, `trust_boundary`,
   `execution_environment_contract`, and the Local App non-elevation rule; adds
   one reusable rule covering credential minimization and **refusal-as-boundary**
   (treat permission, policy, or tool refusals as explicit boundaries to report,
   never as transient faults to bypass or retry under altered identity).
3. `work_and_verification` — merges `core_contract`, `tool_use_contract`
   (schema-only inputs, evidence-driven minimal calls), `engineering_guardrails`,
   `context_completion`, `exploration_discipline`, and `verification`.
4. `agent_home_boundary` — keeps the boundary between active project workspaces,
   `agent_home/AGENTS.md`, curated `memory/{operator,self}.md`, `tmp/`, and
   runtime-owned `.holon/` ledgers, while moving directory tutorials to `L5` and
   leaving exact truncation counts to dynamic memory sections.
5. `durable_work_tracking` — merges `planning_discipline` and
   `work_item_first_execution` (when *not* to create a WorkItem, reuse of the
   active item, discussion tracking versus implementation authorization, and the
   minimal recovery state across `objective`, `plan.md`, and `todo_list`).
6. `runtime_scheduling_contract` — preserves cross-tool scheduling invariants
   (`focus != execution binding != task owner`, canonical `WaitFor` resolution
   order `execution_binding > explicit > turn_bound > current_focus >
   agent_lifecycle`, same-agent cross-WorkItem `task_result` waits without owner
   migration, `yielded` continuation frames versus `blocked`, and persisting
   successor/recovery state before turn-ending operations).
7. `delivery_contract` — merges `reporting_contract` and
   `file_reference_contract` (brief-centric self-contained outcomes, conclusion
   first, concise prose without rigid line-count tables, and confirmed path/link
   rules aligned with
   [Output Delivery and File Reference Contract](./output-delivery-and-file-reference-contract.md)).

Scoped suffix sections (`workspace_context`, `agent_contract`, `delegated_task`,
`event_turn`, loaded `AGENTS.md` layers, memory slices, `skills_usage_contract`,
and `response_language`) retain their existing placement and precedence.

### L3b: Gated Cross-Tool Decision Cards (`crates/holon-context/src/tool_guidance/`)

- **Sole responsibility:** disambiguating sibling tools when multiple options
  exist in the agent's active catalog (for example `ExecCommand` versus
  `ExecCommandBatch`, `CreateAgent` versus `InvokeAgent` versus
  `SendAgentMessage`, `ApplyPatch` surface syntax for the active provider).
- **Exact-subset gating:** emit clauses only for tools actually present in
  `available_tools`. Read-only WorkItem profiles must not emit write-transition
  prompts; `SendAgentMessage`-only profiles must not emit subagent creation and
  deletion essays; `ApplyPatch` emits only the active surface (`CodexDslFreeform`
  or `UnifiedDiffJson`).

### L4: Structured Tool Receipts and Error Recovery (`ToolResultEnvelope`)

- **Sole responsibility:** obligations and recovery hints that only apply *after*
  a specific tool outcome occurs (building on
  [Tool Result Envelope](./tool-result-envelope.md)):
  - `WaitFor` returning `awaiting_final_report` and `CompleteWorkItem` returning
    `awaiting_completion_report` carry the exact pending-report rules (finish
    authorized verification with ordinary tools, emit non-empty final text, do
    not invoke `WaitFor`, `CompleteWorkItem`, `PickWorkItem`, or `Sleep`) in
    their deferred receipt summary.
  - Ambiguous `SendAgentMessage` replies return eligible `reply_context_id`
    selectors in the error details/hint.
  - Explicit model or search/fetch provider override errors return valid
    configured candidates in the recovery hint so even no-shell agents can
    recover without permanent catalog-listing tools.
  - Non-clean `ApplyPatch` receipts instruct bounded inspection of the affected
    hunk before re-patching.

### L5: Embedded Versioned Manuals (`holon docs`) and CLI Control Surface

- **Sole responsibility:** detailed operational references (`cli`, `skills`,
  `apps`, `agents`, `workspace`, `agent-home`, `models`) compiled directly into
  the `holon` binary and served offline via `holon docs [topic]` (supporting
  both human text and `--output json`).
- **Properties:** version-locked to the running binary, immune to `cwd` drift or
  workspace shadowing, zero network dependency, and zero prompt cost on ordinary
  turns where `holon` administration is unused.

## Native Tools Versus `holon` CLI Boundary

Consolidating manuals into `holon docs` enables retiring low-frequency discovery
and administrative tools in favor of the CLI, subject to **five hard runtime
boundaries**:

1. **In-turn atomic settlement and turn-loop hooks:** operations that settle or
   yield the active turn (`WaitFor`, `CompleteWorkItem`, `PickWorkItem`), open
   pending-report continuations, face open-`ExecutionAttempt` control-HTTP
   locks, invalidate turn-local checkpoint anchors (`ApplyPatch`, `*WorkItem`),
   or participate in provider context-management exclusions (`ApplyPatch`) must
   remain in-process native tools.
2. **Runtime-authenticated identity versus declaration-based CLI provenance:** as
   specified in
   [Agent-Aware CLI Invocation Context](./agent-aware-cli-invocation.md),
   `HOLON_CALLER_*` environment claims are provenance declarations rather than
   unforgeable caller authentication. Operations binding turn-admitted
   `reply_context_id` routes (`SendAgentMessage`), supervised child handles and
   `incarnation` checks (`InvokeAgent`, `DeleteAgent`), or `AuthorityClass`
   caps (`Enqueue`) must remain native tools.
3. **No-shell least-privilege profiles:** agents running with
   `process_execution_exposed = false` lack `ExecCommand`; their required
   continuity, memory, timer, WorkItem, and web capabilities cannot depend on
   spawning `holon` subprocesses.
4. **Prompt-cache economics and round-trip latency:** native tool schemas sit in
   the cacheable request prefix and execute in one round, whereas replacing
   frequent operations with `holon docs` + `holon <subcommand>` adds extra
   model rounds and appends uncached command output into rolling conversation
   history.
5. **Provider-protocol and multimodal lowering:** `ApplyPatch` freeform grammars,
   `ViewImage` image blocks, and native `WebSearch` grounding lower directly into
   provider wire formats that subprocess stdout cannot express.

### Four-Tier Built-In Tool Classification

Applying the five boundaries to the 50 built-in tool names yields four tiers
aligned with [Tool Surface Layering](./tool-surface-layering.md):

| Tier | Built-in tools | Target disposition |
| --- | --- | --- |
| **Tier A: Hidden legacy compatibility (4)** | `Sleep`, `TaskList`, `UseWorkspace`, `ListSubagentCleanup` | Keep hidden from model tool specs; remove residual prompt references. |
| **Tier B: Demote to CLI + `holon docs` + L4 error hints (6)** | `ListModelProviders`, `ListProviderModels`, `ListWebSearchProviders`, `ListWebFetchProviders`, `CreateExternalTrigger`, `CancelExternalTrigger` | Ensure machine-readable CLI parity and L4 candidate hints on invalid route overrides; remove from default model `tools[]`. |
| **Tier C: Profile scoping and same-plane CRUD consolidation** | Authority-expanding (`AttachWorkspace`, `DetachWorkspace`, `RemoveWorktree`), `Timer` family (`Create/List/Get/Edit/CancelTimer`), read-only introspection (`Get/ListWorkItems`, `GetAgent`/`ListSubagents`) | Scope authority-expanding tools to profiles with cross-workspace mandate; omit `Timer` tools on bounded one-shot child profiles; evaluate consolidating fragmented same-plane read/timer CRUD schemas while keeping native support for no-shell profiles. |
| **Tier D: Core native runtime primitives (~18–22)** | `ExecCommand`, `ExecCommandBatch`, `ApplyPatch`, `WaitFor`, `Create/Pick/Update/CompleteWorkItem`, `Create/Invoke/DeleteAgent`, `SendAgentMessage`, `Enqueue`, `ListTasks`/`TaskStatus`/`TaskInput`/`TaskStop`, `GetWorkspaceState`/`SwitchWorkspace`/`CreateWorktree`, `MemorySearch`/`MemoryGet`, `WebSearch`/`WebFetch` (`XSearch`), `ViewImage`/`GenerateImage` (`AdvisoryDecision`) | Retain as first-class native tools with concise `L2` descriptions, `L3b` disambiguation cards, and `L4` stateful receipts. External ecosystem integrations and host ops remain CLI + `SKILL.md` / `holon docs`. |

### External ACP Harness Boundary

Consistent with
[External ACP Agent Harness Integration](./external-acp-agent-harness.md), ACP
delegated turns pass only the task handoff and negotiated role while enforcing
workspace/process policy through `AcpPermissionGate`; Holon native system
sections, tool descriptions, and `holon docs` indices are never injected into an
external harness's prompt.

## Cache Stability, Checkpoint Assets, and Snapshot Governance

1. **Static `media_tool_defaults`:** single-call model override rules belong in
   static `ViewImage` / `GenerateImage` descriptions, and missing-route guidance
   belongs in `L4` failure receipts rather than injecting live route-health
   booleans into `AgentScoped` system sections.
2. **Turn-frozen prompt framing:** all provider rounds within a single active
   turn retain the `ProviderPromptFrame` captured at turn start so mid-turn
   daemon upgrades or config reloads never mutate the prefix mid-loop.
3. **Auditable checkpoint templates:** turn-local full, delta, and resume
   checkpoint prompts ([Turn-Local Context Compaction](./turn-local-context-compaction.md))
   move into dedicated `holon-context` prompt assets covered by snapshot tests.
4. **Canonical prompt-topology snapshots:** add a checked-in prompt topology
   snapshot (`docs/website/reference/prompt-topology-snapshots.json`) verified by
   `make snapshots-check` across representative profiles (`zero-tools`,
   `no-shell-steward`, `coding-shell-patch-dsl`, `coding-shell-patch-json`,
   `delegated-child`, and `full-surface`), recording section order,
   `PromptLayout`, `PromptStability`, character/token budgets, and
   `shared_prefix_fingerprint`.
5. **Retiring `constrained_repair_section`:** fold explicit file-scope and
   exact-verification adherence into `authority_and_trust` and
   `work_and_verification` (supporting all operator languages) and remove the
   English substring heuristic once regression checks pass.

## Migration Plan

### Phase 0: Contract Bugfix and Snapshot Baseline

- Fix `runtime_scheduling_contract` in `crates/holon-context/src/system_prompt.rs`
  to match `WaitFor` execution-bound ownership precedence and same-agent
  cross-WorkItem `task_result` waiting, with unit and runtime contract coverage.
- Add the multi-profile prompt-topology snapshot gate to `make snapshots-check`
  and `make snapshots-refresh`.
- Replace brittle filler-sentence assertions in `crates/holon-context` unit tests
  with structural gating, invariant, and snapshot checks.

### Phase 1A: Core Consolidation, Three-Layer Deduplication, and L4 Receipts

- Merge the 17 shared core sections into the 7 `L3a` sections and staticize
  `media_tool_defaults`.
- Deduplicate `WaitFor`, `WorkItem`, `ExecCommand`/`ExecCommandBatch`,
  `ApplyPatch`/`file_mutation`, `Workspace`, and `Task` guidance across
  `holon-context` and `holon-tools`, ensuring `L4` deferred receipts carry full
  pending-report obligations and fixing read-only WorkItem gating.

### Phase 1B: Embedded `holon docs` and Tier B Tool Demotion (Deferred)

- Ship `holon docs [topic]` backed by compile-time embedded manuals; slim
  `tool_holon_cli` and per-tool-gated `tool_agent_invocation`; extract
  checkpoint prompt templates into `holon-context`.
- Verify CLI JSON parity and `L4` route-override candidate hints, then demote
  the 6 Tier B discovery/trigger tools from the default model tool catalog and
  tighten Tier C profile scoping.

### Phase 2: CRUD Consolidation and Net-Token Evaluation (Future)

- Evaluate same-plane CRUD schema consolidation (`Timer`, read-only WorkItem and
  agent introspection), validate net whole-task input token savings (including
  any on-demand `holon docs` reads and cache hit rates), and retire
  `constrained_repair_section` after benchmark verification.
