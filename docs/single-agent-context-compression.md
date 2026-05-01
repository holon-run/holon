# Single-Agent Context Compression For Holon

This note proposes a context compression design for Holon under one explicit
product constraint:

- one durable agent
- one durable session/context
- no operator-facing "start a new session to reset context" escape hatch

That changes the shape of the problem.

For Holon, context compression is not mainly about "how do we summarize old
chat." It is about:

- keeping a single agent alive for a long time
- preserving continuity across coding and event-driven turns
- bounding prompt growth without depending on session resets
- preserving auditability from append-only logs
- keeping the prompt shape cache-friendly when providers support prompt caching

## 1. Current Baseline

Holon already has a good foundation for this problem.

Relevant current behavior:

- `src/storage.rs` keeps durable append-only logs for messages, briefs, tools,
  transcript, tasks, timers, work items, and work plans.
- `src/context.rs` builds model-visible context from recent messages, recent
  briefs, recent tool executions, work queue state, worktree state,
  `context_summary`, and the current input/continuation contract.
- `src/context.rs::maybe_compact_agent()` performs deterministic compaction by
  flattening older messages into a single summary string.
- `src/prompt/mod.rs` already distinguishes `PromptStability::{Stable,
  AgentScoped, TurnScoped}`.
- `src/runtime/operator_dispatch.rs` compacts before prompt assembly on each
  interactive turn.

This is already the right architectural direction:

- durable history is separate from model-visible context
- context is assembled from explicit sections
- the runtime already has session state and append-only evidence

The problem is that the current compaction layer is still too shallow for a
single-agent long-lived session.

## 2. Current Weaknesses

Today Holon's compaction is bounded, deterministic, and easy to test, but it
has four structural weaknesses:

### 2.1 Compaction is message-count based, not budget based

Current triggers are:

- `context_window_messages`
- `compaction_trigger_messages`
- `compaction_keep_recent_messages`

This is too coarse for coding sessions because token pressure often comes from:

- long tool results
- long current input
- rich continuation metadata
- verbose result briefs
- repeated workspace/task metadata

Message count is only a weak proxy.

### 2.2 `context_summary` is one flat string

Current compaction collapses all compacted messages into a single summary text.
That loses important structure:

- what work was active
- what files changed
- what commands were run
- what decisions were made
- what remains open
- what is durable versus provisional

For a one-agent long-lived session, one flat summary eventually becomes both:

- too vague for continuity
- too large to keep rewriting forever

### 2.3 The system rewrites old memory instead of building layers

The current model behaves like:

- keep recent tail
- rewrite older message region into a fresh string

For a short session this is acceptable.

For a long-lived single session, repeated rewrite causes three problems:

- summary drift accumulates
- stable prefix quality degrades
- every compaction boundary can invalidate provider-side prompt caching

### 2.4 `PromptStability` exists, but is not yet fully exploited

Holon already labels sections as:

- `Stable`
- `AgentScoped`
- `TurnScoped`

But today the runtime still renders one full context attachment every turn, and
the session-scoped region is not managed as a true bounded working memory with
stable revision boundaries.

That means Holon has the right vocabulary, but not yet the full lifecycle model
to take advantage of it.

## 3. Design Goal

The design goal should be:

> Keep one agent in one durable session for a long time by using layered,
> append-only, budget-aware memory, where older history becomes structured
> episode memory instead of an ever-rewritten flat summary.

This implies five design rules.

### Rule 1: Durable logs remain append-only

Do not mutate the append-only files under `agent_home/.holon/ledger/`:

- `messages.jsonl`
- `briefs.jsonl`
- `tools.jsonl`
- `transcript.jsonl`

Compression should only change the model-visible projection, not the durable
audit trail.

### Rule 2: Compaction artifacts must also be durable

Do not keep the only compacted memory in `AgentState.context_summary`.

Instead, write compaction artifacts as first-class durable records, then let
`AgentState` point to the active projection.

### Rule 3: Older context should become structured memory, not chat prose

Older turns should be represented as records such as:

- active work snapshot
- work summary snapshot
- scope hints
- completed result
- file/workspace touches
- commands and verification
- unresolved follow-ups

That is much better suited to a coding agent than raw chat-style summarization.

### Rule 4: Working memory should change less often than turn context

Single-session continuity does not mean every prompt region should mutate every
turn.

Holon should keep:

- a stable system prefix
- a relatively stable working memory
- a small rolling turn tail

### Rule 5: Compression should be monotonic

As history ages, it should move through a one-way pipeline:

- hot tail
- warm episode summary
- cold archived episode memory
- optional era-level rollup

It should not bounce between repeated full rewrites.

## 4. Proposed Memory Model

Holon should use four memory layers inside the same agent session.

## 4.1 Layer A: Durable Ledger

This is the existing append-only store:

- messages
- briefs
- tools
- transcript
- tasks
- work items
- work plans

This is the truth source. It is not prompt-bounded. It is not compacted away.

## 4.2 Layer B: Hot Turn Context

This is the smallest and most volatile layer. It should include:

- current input
- continuation context
- active work item
- active work plan
- latest result brief
- most recent message tail
- most recent tool-result tail

This layer should be strictly token-bounded and rebuilt every turn.

## 4.3 Layer C: Session Working Memory

This is the key new layer.

It is not the full history. It is the current durable working state of the
agent in this session.

Suggested fields:

- active delivery target
- active work summary
- scope hints and completion hints
- active workspace/worktree snapshot
- important open threads
- current plan / next checkpoints
- working set files
- important recent decisions
- most recent verified outcome
- active waiting intents or pending external dependencies

This should be small, structured, and updated incrementally after terminal
turns, not rebuilt from scratch on every compaction.

## 4.4 Layer D: Episode Memory

Older history should be grouped into immutable episode summaries.

An episode is not "N messages". It is a meaningful work chunk, for example:

- one active work item or delivery-target phase
- one debug/fix cycle
- one analysis chunk
- one background-task coordination interval
- one wake/resume cycle that materially changed state

Each episode summary should capture:

- episode id
- covered turn range
- covered message range
- active work item id at the time
- delivery target / work summary at the time
- scope hints at the time
- key files touched
- key commands / verification
- outcome
- unresolved items carried forward
- important evidence handles

Old episodes remain immutable once finalized.

## 4.5 Prompt Projection Of Each Memory Layer

These memory layers do not all enter the prompt in the same form.

Holon should distinguish:

- durable canonical state
- model-visible prompt projection

The canonical state is structured and durable. The prompt projection is a
bounded, stable textual rendering of that state.

### Durable ledger projection

The durable ledger does **not** enter the prompt directly as raw logs.

Instead, the ledger is the source for:

- hot turn context
- working memory
- episode summaries

The operator and runtime can inspect the ledger directly through storage and
transcript surfaces, but the model should not receive unbounded raw history by
default.

### Hot turn context projection

Hot turn context is rendered directly into prompt sections because it is
already close to the model's current task.

This should include sections such as:

- `current_input`
- `continuation_context`
- `current_work_item`
- `queued_blocked_work_items`
- `latest_result`
- `recent_messages`
- `recent_tool_executions`

This layer is:

- `TurnScoped`
- rebuilt every turn
- aggressively budgeted

### Working memory projection

Working memory **must** enter the prompt, but not as raw JSON or as a
direct dump of internal runtime state.

It should be rendered as one small, stable `AgentScoped` section with fixed
field order and bounded list sizes.

Suggested section name:

- `working_memory`

Suggested prompt shape:

```text
Working memory:
- Revision: 12
- Delivery target: fix flaky benchmark failure in metrics export
- Work summary: isolate the export boundary and ship the minimal fix
- Scope hints:
  - benchmark passes and output format remains unchanged
  - do not redesign the internal metric model
- Current plan:
  - isolate the failing export path
  - patch the zero-value handling
  - rerun focused verification
- Working set files:
  - src/benchmark/report.rs
  - tests/metrics_export.rs
- Recent decisions:
  - treat missing metrics as zero only at export boundary
  - keep the internal metric model unchanged
- Latest verified result: cargo test --test metrics_export passed
- Pending follow-ups:
  - run the full benchmark suite
- Waiting on:
  - none
```

Important constraints:

- fixed field order
- bounded list lengths
- short sentence fragments, not long prose
- update only on meaningful working-memory revision changes

### Episode memory projection

Episode memory should not be injected as a large archive dump.

Instead, prompt assembly should select only a small number of relevant archived
episodes and render them as compact `AgentScoped` sections or as one grouped
section.

Suggested section name:

- `relevant_episode_memory`

Suggested prompt shape:

```text
Relevant episode memory:
- [episode ep-17][turns 41-54]
  Work target: fix flaky benchmark failure in metrics export
  Files: src/benchmark/report.rs, tests/metrics_export.rs
  Verification: cargo test --test metrics_export passed
  Outcome: focused fix completed
  Carry-forward: run the full benchmark suite before closure
- [episode ep-16][turns 31-40]
  Work target: isolate the benchmark export path
  Outcome: narrowed root cause to export boundary handling
```

This layer is:

- durable and immutable at the storage level
- selected dynamically at prompt-assembly time
- usually stable across nearby turns unless relevance changes

### Active episode projection

The active episode is different from archived episode memory.

It is a runtime builder for the current unfinished work chunk. By default, it
should **not** enter the prompt as a full standalone section.

Instead, its contents should normally be projected indirectly into:

- `working_memory` for stable ongoing state
- hot turn context for the most recent volatile evidence

This avoids:

- duplication with recent messages and tool summaries
- turning the session-scoped prefix into a constantly changing blob
- exposing more raw intermediate state than the model needs

When Holon does need to expose the active episode explicitly, it should do so as
one small checkpoint section rather than as a full dump of the builder.

Suggested section name:

- `active_episode_checkpoint`

Suggested prompt shape:

```text
Active episode checkpoint:
- Started at turn: 41
- Current delivery target: fix flaky benchmark failure in metrics export
- Scope hints:
  - keep output format unchanged
- Working set files:
  - src/benchmark/report.rs
  - tests/metrics_export.rs
- Completed in this episode:
  - isolated export boundary as root cause
  - patched zero-value handling
  - passed focused metrics export test
- Still open:
  - run the full benchmark suite
```

This section should be emitted only when:

- the current episode has become long enough that `working_memory` and
  hot tail no longer communicate the current phase clearly
- the agent resumed from sleep, wake, callback, or task rejoin and needs a
  compact checkpoint
- the current work item is still active but the work chunk has accumulated
  enough internal progress that a handoff-style checkpoint materially helps

Even in those cases, Holon should render only a bounded checkpoint view, not
the full active episode builder state.

## 4.6 Prompt Visibility Rules

Each memory layer should have a default prompt policy.

| Layer | Canonical Store | Prompt Visible | Stability |
| --- | --- | --- | --- |
| Durable ledger | append-only jsonl files | indirect only | n/a |
| Hot turn context | derived each turn | always | `TurnScoped` |
| Working memory | `AgentState.working_memory` | always | `AgentScoped` |
| Active episode builder | runtime-only builder state | checkpoint only when needed | usually `TurnScoped` |
| Episode memory | `context_episodes.jsonl` | selected only | `AgentScoped` |

The core rule is:

- durable state is larger than prompt-visible state
- prompt-visible state is a projection of durable state
- active unfinished work is normally shown through projections, not through a
  full builder dump

## 5. Proposed New Data Structures

Holon does not need a large abstraction rewrite. It needs one new durable memory
record and one richer in-memory projection.

### 5.1 Add `ContextEpisodeRecord`

Suggested new durable file:

- `context_episodes.jsonl`

Suggested record shape:

```rust
pub struct ContextEpisodeRecord {
    pub id: String,
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub start_turn_index: u64,
    pub end_turn_index: u64,
    pub start_message_offset: usize,
    pub end_message_offset: usize,
    pub active_work_item_id: Option<String>,
    pub delivery_target: Option<String>,
    pub work_summary: Option<String>,
    pub scope_hints: Vec<String>,
    pub summary: String,
    pub working_set_files: Vec<String>,
    pub commands: Vec<String>,
    pub verification: Vec<String>,
    pub open_threads: Vec<String>,
    pub archived: bool,
}
```

The key property is durability plus immutability after finalization.

### 5.2 Replace flat `context_summary` with structured `WorkingMemoryState`

`AgentState` should eventually carry something like:

```rust
pub struct WorkingMemoryState {
    pub compression_epoch: u64,
    pub compacted_through_message_count: usize,
    pub hot_tail_message_count: usize,
    pub working_memory_revision: u64,
    pub current_working_memory: WorkingMemorySnapshot,
    pub active_episode_id: Option<String>,
    pub archived_episode_count: usize,
    pub last_compaction_token_estimate: Option<u64>,
}
```

`context_summary: Option<String>` can remain temporarily for migration, but it
should stop being the primary compression artifact.

### 5.3 Add `WorkingMemorySnapshot`

Suggested shape:

```rust
pub struct WorkingMemorySnapshot {
    pub active_work_item_id: Option<String>,
    pub delivery_target: Option<String>,
    pub work_summary: Option<String>,
    pub scope_hints: Vec<String>,
    pub current_plan: Vec<String>,
    pub working_set_files: Vec<String>,
    pub recent_decisions: Vec<String>,
    pub pending_followups: Vec<String>,
    pub waiting_on: Vec<String>,
}
```

This is the main replacement for the current flat compacted summary.

### 5.4 Add `WorkingMemoryDelta`

The runtime also needs a small record that explains what changed between the
last visible working-memory revision and the current one.

Suggested shape:

```rust
pub enum WorkingMemoryUpdateReason {
    TerminalTurnCompleted,
    TaskRejoined,
    WakeResumed,
    ActiveWorkChanged,
    ScopeHintsChanged,
}

pub struct WorkingMemoryDelta {
    pub from_revision: u64,
    pub to_revision: u64,
    pub created_at_turn: u64,
    pub reason: WorkingMemoryUpdateReason,
    pub changed_fields: Vec<String>,
    pub summary_lines: Vec<String>,
}
```

The goal is not to preserve long-term history. The goal is to tell the model,
on the next turn, what changed since the last stable working-memory state it was
shown.

### 5.5 Add `TurnMemoryDelta`

Holon should not extract episode memory directly from free-form prompt text.

Instead, after each terminal turn it should derive one small structured delta
from durable evidence:

```rust
pub struct TurnMemoryDelta {
    pub turn_index: u64,
    pub active_work_changed: bool,
    pub work_plan_changed: bool,
    pub scope_hints_changed: bool,
    pub touched_files: Vec<String>,
    pub commands: Vec<String>,
    pub verification: Vec<String>,
    pub decisions: Vec<String>,
    pub pending_followups: Vec<String>,
    pub waiting_on: Vec<String>,
}
```

This becomes the input to both:

- working memory updates
- active episode accumulation

## 6. Compression Lifecycle

The runtime should update memory in two different moments:

- after a turn completes
- before a prompt is assembled

Those are separate responsibilities.

## 6.1 Post-turn: extract durable memory

After every terminal interactive turn:

1. Read the just-finished result brief, tool execution records, transcript
   entries, and relevant runtime state.
2. Build a `TurnMemoryDelta`.
3. Update `WorkingMemorySnapshot` incrementally.
4. If working memory changed materially, bump its revision and create a
   `WorkingMemoryDelta`.
5. Merge the `TurnMemoryDelta` into the active episode builder.
6. Finalize the active episode when a boundary is crossed.

Episode finalization boundaries should include:

- active work item switch
- delivery target changed materially
- strong scope-hint change
- completed result with a meaningful state transition
- explicit long wait / sleep boundary
- background-task handoff that ends a coherent work chunk

This is where Holon should preserve continuity, not during prompt assembly.

### 6.1.1 How working memory is generated

Working memory should be derived deterministically from existing Holon
state, not free-authored by the model.

Recommended source mapping:

- `active_work_item_id`: active work item id when one exists
- `delivery_target`: active work item delivery target
- `work_summary`: active work item summary
- `scope_hints`: bounded extraction from trusted operator prompts, active work
  progress, and recent result briefs; prefer evidence bound to the current
  active work item and only fall back to legacy unbound records when no
  active-work-bound evidence exists
- `current_plan`: active work plan items that are not completed
- `working_set_files`: recent `ApplyPatch`, relevant
  `read_file`, and file-oriented tool arguments, using the same active-work
  binding preference and legacy-unbound fallback
- `recent_decisions`: bounded extraction from result/failure briefs and
  explicit state transitions, again preferring evidence bound to the current
  active work item
- `pending_followups`: active work item, queued waiting items, and unresolved
  plan entries
- `waiting_on`: waiting intents, task wait state, timer wait state, or explicit
  continuation waiting reasons; when multiple active waits exist, sort them by
  active-work relevance first, then by recency, and only trim after that
  ordering is applied

The first version should remain deterministic and rules-based. LLM-assisted
memory authoring can be layered on later if needed.

### 6.1.2 How the runtime tells the model that memory changed

Updating the durable memory state is not enough. The model only "knows" memory
changed when the next prompt includes both:

- the latest `working_memory` section
- a small `working_memory_delta` section

Suggested prompt shape:

```text
Working memory updated since the last completed turn:
- Revision: 11 -> 12
- Reason: terminal_turn_completed
- Changed fields:
  - working_set_files
  - pending_followups
- Summary:
  - narrowed working set to the export path and its test
  - reduced remaining follow-up to full benchmark validation
```

This section should be:

- `TurnScoped`
- short-lived
- emitted only when the working-memory revision changed

The working memory itself stays `AgentScoped`. The delta is the
explicit handoff signal that tells the model what just changed.

### 6.1.3 Update timing rule

Holon should **not** rewrite working memory during a single provider
tool loop.

Default rule:

- collect evidence during the turn
- commit working memory at the terminal turn boundary
- expose the new snapshot and delta on the next prompt

This avoids:

- prompt-prefix churn
- unstable cache behavior
- moving the model's baseline during one reasoning loop

## 6.2 Pre-turn: assemble a budgeted projection

Before provider prompt assembly:

1. Estimate token cost of each context section.
2. Build the hot turn tail first.
3. Inject `WorkingMemorySnapshot`.
4. Add only the most relevant archived episode summaries.
5. Omit raw older messages when the episode layer already covers them.

This should be a token-budget planner, not a message-count slicer.

### 6.2.1 Prompt assembly order

Prompt assembly should follow a stable order:

1. `Stable` system sections
2. `AgentScoped` working memory
3. `AgentScoped` relevant episode memory
4. `TurnScoped` working memory delta, if any
5. `TurnScoped` hot tail sections

This keeps:

- the stable prefix maximally reusable
- working memory readable and consistent
- new updates visible without forcing a rewrite of the stable memory projection

### 6.2.2 How hot turn context is generated

Hot turn context should remain a direct projection of the latest durable events,
not a second summary layer.

Recommended sources:

- current message for `current_input`
- continuation resolution for `continuation_context`
- active work item and work plan for work coordination sections
- latest result brief for immediate follow-up grounding
- recent messages for short conversation continuity
- recent tool executions for concrete recent evidence

The runtime should trim this layer by value density:

1. keep current input and continuation context
2. keep active work and latest result
3. keep recent message tail
4. keep recent tool results, but prefer compact tool summaries over raw output

Hot turn context is the place for the latest volatile evidence. It should not
become a rolling replacement for working memory.

## 6.3 Compaction order

Compaction should happen in this order:

1. Shrink tool-result verbosity.
2. Shrink recent message tail.
3. Convert warm history into episode summaries.
4. Collapse very old episodes into one era rollup only as a last resort.

This order matters.

For coding agents, old tool output is usually the lowest-value prompt payload.

### 6.3.1 Episode extraction pipeline

Episode memory should be extracted through a deterministic pipeline:

1. `extract_turn_memory_delta(...)`
2. `merge_into_active_episode(...)`
3. `finalize_episode_if_needed(...)`

The key point is that Holon should not summarize old history in one large
retrospective pass. It should accumulate an active episode over time and
finalize it only when a semantic boundary is crossed.

### 6.3.2 Episode source signals

Episode extraction should use existing durable evidence, especially:

- `agent_home/.holon/ledger/messages.jsonl` for operator/task prompts and
  high-level turn anchors
- `agent_home/.holon/ledger/briefs.jsonl` for result/failure outcomes
- `agent_home/.holon/ledger/tools.jsonl` for touched files, commands, and
  verification evidence
- `agent_home/.holon/ledger/transcript.jsonl` for assistant round boundaries,
  continuation prompts, and rejoin/wake signals
- active work item, work plan, and waiting intent state for semantic phase
  changes

This is deliberately different from "take old prompt text and summarize it."

### 6.3.3 Episode builder behavior

Holon should maintain one active episode builder per agent session.

That builder accumulates:

- covered turn range
- active work item snapshot
- delivery target / work summary snapshot
- scope hints snapshot
- touched files
- commands
- verification outcomes
- key decisions
- pending follow-ups
- waiting state
- latest result hint

On finalize, it writes one immutable `ContextEpisodeRecord` and resets the
builder for the next episode.

### 6.3.4 Episode boundary detection

Episode boundaries should be semantic, not purely count-based.

Recommended first-pass boundary reasons:

- active work item switched
- delivery target changed materially
- scope hints changed materially
- meaningful terminal result checkpoint
- entered waiting or sleep state
- task rejoin changed the active work phase
- active work item switched
- worktree entered or exited
- episode exceeded a hard turn or budget cap

This is what makes episode memory useful for one long-lived session: each
episode represents a coherent work chunk, not an arbitrary slice of messages.

### 6.3.5 Episode summary rendering

When an episode is finalized, Holon should store both:

- structured fields for retrieval and future recomposition
- one bounded textual summary for prompt projection

Suggested rendered form:

```text
Episode summary:
- Work target: fix flaky benchmark failure in metrics export
- Scope hints:
  - benchmark passes and output format remains unchanged
  - do not redesign the internal metric model
- Files touched:
  - src/benchmark/report.rs
  - tests/metrics_export.rs
- Commands:
  - cargo test --test metrics_export
- Verification:
  - cargo test --test metrics_export passed
- Key decisions:
  - treat missing metrics as zero only at export boundary
- Outcome:
  - focused fix completed and verified
- Carry-forward:
  - run the full benchmark suite before final closure
```

The structure is canonical. The text is a stable projection of that structure.

## 7. Prompt Topology For A Single-Agent Session

Holon should explicitly render prompt layers like this:

### Stable prefix

- identity
- core contract
- engineering guardrails
- tool guidance
- trust and execution policy rules

This should almost never change.

### Session-scoped memory prefix

- active work snapshot
- active workspace/worktree snapshot
- current working memory snapshot
- selected episode summaries

This should change only on:

- terminal turn completion
- active-work or scope-hint transitions
- explicit compaction epoch changes

### Turn-scoped suffix

- active episode checkpoint, when needed
- continuation context
- current input
- active work item / queue hints
- latest result brief
- latest messages
- latest tool results

This should change every turn.

This is the right shape for a single long-lived session because it keeps the
agent identity continuous while still preserving a stable prompt prefix.

## 8. Cache Friendliness

This design should also improve provider cache reuse.

Holon should be designed so that:

- `Stable` sections stay byte-stable
- `AgentScoped` sections mutate less often than `TurnScoped`
- archived episode summaries are immutable once created
- older memory is added by selection, not by rewriting the same large blob every
  turn

If provider support is added later, Holon should use:

- `agent_id` as the base `prompt_cache_key`
- a `working_memory_revision` or `compression_epoch` to scope cache identity
  when working memory changes materially

Provider-specific future support should look like:

- prompt-cache-key style providers: key the stable session prefix by agent id
  plus working-memory revision
- cache-control style providers: mark stable system blocks and stable
  working-memory blocks explicitly when the API supports it

The important point is not the exact vendor API. The important point is prompt
shape discipline:

- stable prefix
- slow-changing working memory
- small volatile suffix

Current implementation note:

- runtime requests now expose a provider-facing prompt cache identity with:
  `agent_id`, base `prompt_cache_key`, `working_memory_revision`, and
  `compression_epoch`
- the current base `prompt_cache_key` is just `agent_id`, so byte-stable
  prefixes can still reuse provider-side prefix caching instead of rotating the
  top-level key on every memory revision
- `working_memory_revision` and `compression_epoch` are still emitted in prompt
  dumps and runtime events so cache invalidation boundaries stay observable
- cache-control style transports currently place breakpoints at the last
  `Stable` block and the last `AgentScoped` block in the projected prompt
- subagent bootstrap still uses the existing flat prompt path; cache-aware block
  projection currently applies to the main runtime provider turn only

## 9. Retrieval Inside The Same Session

A single durable session will eventually accumulate more episode memory than can
fit in prompt context.

So Holon should not try to keep all archived episodes model-visible all the
time.

Instead:

- the runtime should keep episode metadata indexed locally
- prompt assembly should select only the most relevant archived episodes

Selection signals can start simple:

- same active work item id
- same delivery-target fingerprint
- same worktree/workspace
- same file path overlap
- same task id
- same waiting intent / callback resource
- recency

Practical selection order should be:

1. same active work item or delivery-target fingerprint
2. highest file overlap with current working set
3. same worktree/workspace
4. same task/waiting resource
5. recency tie-break

The renderer should then take only the top few episodes that fit the
working-memory budget, rather than appending every matching episode.

This is still one session. It is just retrieval within the same session's
durable memory.

## 10. What To Stop Doing

This design implies three behavior changes.

### 10.1 Stop using `context_summary` as the main memory layer

It can remain for compatibility during migration, but it should become a debug
projection, not the core memory primitive.

### 10.2 Stop compacting only messages

For Holon, the meaningful unit is not raw messages. It is:

- result briefs
- tool summaries
- work-item transitions
- work-plan state
- work item state
- episode boundaries

### 10.3 Stop triggering compaction purely by message count

The runtime should compact based on estimated section budget and last observed
provider token usage.

## 11. Concrete Rollout Plan

This can be implemented incrementally.

### Phase 1: Structured working memory

- add `WorkingMemorySnapshot`
- update it after terminal turns
- render it in context instead of a flat `context_summary`
- keep current deterministic compaction as a fallback

This is the highest-leverage first step.

### Phase 2: Episode summaries

- add `context_episodes.jsonl`
- finalize immutable episode records at active-work/result/sleep boundaries
- stop repeatedly rewriting one large compacted summary blob

This makes long-lived single-session continuity much stronger.

### Phase 3: Budget-aware prompt assembly

- estimate section token cost
- budget `TurnScoped` vs `AgentScoped` sections separately
- select archived episodes by relevance

This replaces message-count compaction with real prompt planning.

### Phase 4: Cache-aware provider integration

- treat `PromptStability` as a real provider request boundary
- add prompt cache key / cache control support where available
- measure cache hit behavior in benchmarks

Implementation status:

- OpenAI-style transports now send `prompt_cache_key` and surface cached input
  token reads in provider usage telemetry
- Anthropic-style transports now render system/context prompt blocks and mark
  cache-control breakpoints at stable/session-scoped boundaries
- runtime audit events and assistant-round transcript entries now include
  provider cache usage plus the cache identity fields used for the first round

This is valuable, but it should come after the memory model is corrected.

## 12. Success Criteria

This design is successful if Holon can do all of the following without requiring
the operator to create a new session:

- preserve grounded follow-up answers after many coding turns
- remember important file changes and verification results
- resume after sleep/wake boundaries with low confusion
- keep prompt growth bounded for long-lived agents
- avoid rewriting the same compacted blob every few turns
- keep a stable prompt prefix good enough for provider-side prompt caching when
  supported

## 13. Recommendation

For Holon specifically, the right strategy is:

- one durable agent
- one durable append-only ledger
- one bounded hot tail
- one structured working memory
- one immutable episode archive
- optional relevance retrieval over archived episodes

Not:

- one forever-growing raw message stream
- one repeatedly rewritten flat summary blob
- one operator-visible "new session" escape hatch

That keeps Holon aligned with its own runtime direction:

- one agent owns one context
- durable state stays explicit
- compression is a projection problem, not a history-deletion problem
