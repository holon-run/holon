---
title: RFC: PresentationItem Model and Level-Aware Renderer
date: 2026-05-23
status: draft
---

# RFC: PresentationItem Model and Level-Aware Renderer

## Summary

This RFC defines a typed `PresentationItem` model that replaces the current
event-classification-based TUI rendering pipeline. The same item carries
complete data and renders differently by display level.

Current state: raw runtime events → `OperatorEventCategory` + `OperatorVisibility`
→ per-level event-name whitelist → `ConversationCell::SystemNotice`.

Proposed state: raw runtime events → **Reducer** → `PresentationItem` → **Renderer**
→ `ConversationCell`, where the Reducer applies merging/coalescing rules and the
Renderer respects the current display level to decide per-item granularity.

This RFC extends and concretizes the presentation pipeline outlined in
[operator-display-levels-and-event-presentation.md](./operator-display-levels-and-event-presentation.md).

## Motivation

### Current Problems

1. **No event merging (P0).** Each raw event produces an independent
   `SystemNotice`. A single `ExecCommand` call produces at least two rows
   (started + finished). The operator sees runtime event plumbing instead of
   human-readable activity items.

2. **Internal vocabulary leaks (P1).** `[task]`, `[work-item]`, `[workspace]`,
   `[external-trigger]` prefixes appear in the main conversation. `System (work)`,
   `System (waiting)`, `System (workspace)` are internal categories, not
   user-facing labels.

3. **Three independent filter functions.** `is_info_event`, `is_verbose_event`,
   `is_debug_event` are maintained as event-name whitelists. Adding a new event
   requires updating three functions. The display logic is distributed and hard
   to test.

4. **Brief rendering lacks visual hierarchy.** Briefs are large markdown blobs
   with coarse speaker labels: `"Holon"` vs `"Holon (failed)"`.

5. **No mutable in-flight activity.** Progress events append new rows instead of
   updating an existing active item. The transcript accumulates noise during
   tool execution.

### Design Goals

- **Unified model.** One `PresentationItem` enum covers all three main display
  levels (`info`, `verbose`, `debug`). No per-level variant types.
- **Data/rendering separation.** The Reducer fills complete data into each item.
  The Renderer decides how much to show based on the current display level.
- **Merge/coalesce.** Related low-level events combine into one item.
  Reducer maintains short-lived reduction state to coalesce adjacent events.
- **Testable.** Each item variant has a predictable `render(level) → Vec<Cell>`
  output, suitable for snapshot testing.

## Non-Goals

- Do not change the raw event stream contract.
- Do not remove or weaken `/events` trace inspection.
- Do not change provider, tool, or work item runtime semantics.
- Do not define a pixel-level visual spec for any single UI implementation.

## Display Levels (Recap)

From the existing RFC:

| Level | Name | Description |
|-------|------|-------------|
| 3 | `info` | Result-oriented. Operator messages, briefs, errors, waiting notices. |
| 4 | `verbose` | Codex-like activity. Assistant progress, tool activity (merged), file changes. |
| 5 | `debug` | Curated internals. Full diffs, full commands, provider telemetry, task lifecycle. |

`trace` (level 6) is reserved for `/events` raw inspection and is not a main
conversation display level.

## The `PresentationItem` Model

### Core Principle

```
                     PresentationItem::FileChange {
                         path: "src/foo.rs",
                         action: Modified,
                         hunks: [HunkSummary { start: 12, count: 5 }, ...],
                         full_diff: Some("@@ -12,5 +12,7 @@\n- old\n+ new\n..."),
                     }

level 3 (info)   →  不渲染 (min_display_level = 4)

level 4 (verbose) →  M  src/foo.rs  (+7, -5)
                     ^ 只用到 path + hunks 摘要

level 5 (debug)  →  M  src/foo.rs  (+7, -5)
                     │  @@ -12,5 +12,7 @@
                     │  - old line
                     │  + new line
                     │  ...
                     ^ 摘要 + full_diff 展开
```

**Data is constant; rendering varies by level.**

### The Enum

```rust
/// A user-facing presentation item.
///
/// Each variant carries complete data and a `min_display_level()`.
/// The Reducer produces items from raw events; the Renderer filters
/// by level and decides per-item display granularity.
#[derive(Debug, Clone)]
pub enum PresentationItem {
    // ── Level 3+ (info): result-oriented ──

    /// Operator message.
    UserMessage {
        created_at: DateTime<Utc>,
        body: String,
        status: Option<OperatorMessageStatus>,
    },

    /// Final brief (result or failure).
    AssistantResult {
        created_at: DateTime<Utc>,
        agent_id: String,
        kind: BriefKind,
        markdown: String,
        /// Link to related task output if available.
        related_task_id: Option<String>,
    },

    /// Error, interruption, or alert.
    SystemAlert {
        title: String,
        body: String,
        kind: AlertKind,   // Error, Warning, Info
    },

    /// Active wait that needs operator attention.
    WaitingNotice {
        description: String,
        source: Option<String>,
        waiting_since: DateTime<Utc>,
    },

    /// Work item created or completed (delta, not full record).
    WorkItemCard {
        action: WorkItemAction,  // Created, Completed, Delegated, ...
        item_id: String,
        summary: String,
    },

    // ── Level 4+ (verbose): Codex-like activity ──

    /// In-flight assistant text.
    AssistantProgress {
        text: String,
        /// Whether this is a stable committed item or a live mutable item.
        state: ItemState,
    },

    /// Group of related actions under one heading (e.g. "Explored", "Fixed").
    ActionGroup {
        heading: String,
        items: Vec<PresentationItem>,
        state: ItemState,
    },

    /// Shell command execution.
    CommandExecuted {
        cmd_preview: String,
        cwd: Option<String>,
        duration: Duration,
        exit_code: i32,
        /// Level 4 summary line.
        stdout_summary: String,
        /// Full stdout, lazy-filled for level 5.
        full_stdout: Option<String>,
        /// Full stderr, lazy-filled for level 5.
        full_stderr: Option<String>,
    },

    /// File read.
    FileRead {
        path: String,
        summary: String,
    },

    /// File change (ApplyPatch, write, delete, rename).
    FileChange {
        path: String,
        action: FileAction,
        hunks: Vec<HunkSummary>,
        /// Full diff, lazy-filled for level 5.
        full_diff: Option<String>,
    },

    /// Plan or roadmap shown to operator.
    PlanShown {
        title: String,
        summary: String,
    },

    /// Work item bookkeeping (picked, enqueued, stale reminder, etc.).
    /// These are verbose lifecycle/activity rows, distinct from info-level
    /// lifecycle cards.
    WorkItemBookkeeping {
        item_id: String,
        transition: WorkItemTransition,
        summary: String,
    },

    // ── Level 5+ (debug): curated internals ──

    /// Provider round telemetry.
    ProviderRound {
        model: String,
        stop_reason: Option<String>,
        tokens: Option<TokenCount>,
        round_number: Option<u64>,
    },

    /// Internal state transition (e.g. closure, continuation).
    InternalTransition {
        what: String,
        from: String,
        to: String,
    },

    /// Task lifecycle event.
    TaskLifecycle {
        task_id: String,
        transition: TaskTransition,
    },

    /// Workspace or worktree change.
    WorkspaceChange {
        path: Option<String>,
        change: String,
    },

    /// Continuation/closure detail.
    ContinuationDetail {
        trigger: String,
        outcome: String,
    },
}

/// Whether the item is stable (committed to transcript) or live (mutating).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Stable,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItemAction {
    Created,
    Completed,
    Delegated,
    DelegationCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    Created,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone)]
pub struct HunkSummary {
    pub start: usize,
    pub count: usize,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTransition {
    Created,
    StatusUpdated,
    ResultReceived,
    ChildSpawned,
    InputDelivered,
    Failed,
}

#[derive(Debug, Clone)]
pub struct TokenCount {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}
```

### `min_display_level`

```rust
impl PresentationItem {
    /// The minimum display level at which this item appears.
    pub fn min_display_level(&self) -> u8 {
        match self {
            Self::UserMessage { .. }
            | Self::AssistantResult { .. }
            | Self::SystemAlert { .. }
            | Self::WaitingNotice { .. }
            | Self::WorkItemCard { .. } => 3,

            Self::AssistantProgress { .. }
            | Self::ActionGroup { .. }
            | Self::CommandExecuted { .. }
            | Self::FileRead { .. }
            | Self::FileChange { .. }
            | Self::PlanShown { .. } => 4,

            Self::ProviderRound { .. }
            | Self::InternalTransition { .. }
            | Self::TaskLifecycle { .. }
            | Self::WorkItemBookkeeping { .. }
            | Self::WorkspaceChange { .. }
            | Self::ContinuationDetail { .. } => 5,
        }
    }

    /// Whether this item is visible at the given display level.
    pub fn is_visible_at(&self, level: u8) -> bool {
        level >= self.min_display_level()
    }
}
```

## The Reducer

### Role

The Reducer consumes `ProjectionEventRecord`s and emits `PresentationItem`s.
It is the "event merging" layer that was missing in the current implementation.

### Reduction State

The Reducer maintains ephemeral reduction state (not persisted):

```rust
pub struct PresentationReducer {
    /// Items queued for rendering (stable, committed to transcript).
    buffer: Vec<PresentationItem>,

    /// The current live item being mutated in place.
    live_item: Option<PresentationItem>,

    /// Recent command/tool lifecycle pairing state.
    pending_action: Option<PendingAction>,
}

enum PendingAction {
    CommandStarted {
        started_at: DateTime<Utc>,
        cmd_preview: String,
        cwd: Option<String>,
    },
    ActionGroupStarted {
        heading: String,
        items: Vec<PresentationItem>,
    },
}
```

### Reduction Rules

| Raw Event Pattern | Reduction |
|---|---|
| `process_execution_requested` → `tool_executed` | Merge into one `CommandExecuted` |
| Multiple sequential `FileRead` events | Merge into one `ActionGroup { heading: "Explored" }` |
| Multiple sequential `FileChange` events | Merge into one `ActionGroup { heading: "Changed" }` |
| `assistant_round_recorded` (with tools) + following tool events | Attach tool events under the assistant round's ActionGroup |
| `task_created` + `task_status_updated` + `task_result_received` (same task_id) | Coalesce into one `TaskLifecycle` with the latest transition |
| `provider_round_completed` + preceding `assistant_round_recorded` | Attach as metadata to the assistant round; standalone only in debug |
| `work_item_written` (non-complete) + subsequent `work_item_written` | Keep only the latest delta |
| `waiting_intent_created` then `waiting_intent_cancelled` (same id) | Drop both unless the wait was long (show as a short-lived notice) |
| Adjacent `AssistantProgress` with same speaker | Coalesce into the latest text (progress is cumulative, not additive) |

### Reducer API

```rust
impl PresentationReducer {
    pub fn new() -> Self;

    /// Feed one event and return any newly-stable items.
    pub fn push_event(
        &mut self,
        event: &ProjectionEventRecord,
        briefs: &[BriefRecord],
    ) -> Vec<PresentationItem>;

    /// Signal end of a batch (e.g. turn end). Returns any remaining live items
    /// committed to stable.
    pub fn flush(&mut self) -> Vec<PresentationItem>;

    /// The current live item, if any (for in-progress display).
    pub fn live_item(&self) -> Option<&PresentationItem>;
}
```

## The Renderer

### Role

The Renderer converts `PresentationItem`s into `ConversationCell`s (or a
future cell type). It is level-aware: the same item renders differently
depending on `display_level`.

### Renderer Signature

```rust
pub trait Renderable {
    /// Minimum display level for this item to appear at all.
    fn min_display_level(&self) -> u8;

    /// Render at the given display level.
    fn render(&self, level: u8) -> Vec<RenderedCell>;
}

pub struct RenderedCell {
    pub role: CellRole,
    pub speaker: String,
    pub body: String,
    /// Whether this cell should replace the previous cell of the same kind
    /// (for live/progress items).
    pub replace_previous: bool,
}

pub enum CellRole {
    UserMessage,
    AssistantResult,
    AssistantProgress,
    Activity,
    Alert,
    Detail,
}
```

### Per-Variant Rendering Behavior

#### AssistantResult

| Level | Behavior |
|---|---|
| info, verbose | Render markdown as-is. Speaker: `"Holon"` for Result, `"Holon · error"` for Failure. |
| debug | Same + show `related_task_id` if present. |

#### CommandExecuted

| Level | Behavior |
|---|---|
| verbose | `✓ cmd_preview (1.2s)` |
| debug | Above line + `cwd: ...` + `exit: 0` + `---stdout---` + full_stdout (truncated) |

#### FileChange

| Level | Behavior |
|---|---|
| verbose | `M  path  (+7, -5)` |
| debug | Above line + `│` prefix + full diff (truncated) |

#### ActionGroup

| Level | Behavior |
|---|---|
| verbose | Heading line + nested items at one indent level |
| debug | Heading line + nested items with full debug detail |

#### ProviderRound

| Level | Behavior |
|---|---|
| debug | `◇ model · stop_reason · N tokens` |
| verbose, info | Hidden (`min_display_level = 5`) |

#### TaskLifecycle

| Level | Behavior |
|---|---|
| debug | `↻ task task_id: created → running` |
| verbose, info | Hidden unless terminal + failed |

### Speaker Names

The current internal category labeling (`System (work)`, `System (waiting)`,
`[task]`, `[work-item]`) is **removed** from level 3–5 rendering.

| CellRole | Speaker (levels 3–5) |
|---|---|
| UserMessage | Operator name |
| AssistantResult | `"Holon"` / `"Holon · error"` |
| AssistantProgress | `"Holon"` |
| Activity | No speaker (activity indicator) |
| Alert | `"⚠"` or no speaker |
| Detail | No speaker (detail row) |

## Pipeline Integration

### Current Pipeline

```
raw events → operator_event.rs → ProjectionEventRecord (with category/visibility)
                                       ↓
                               projection.rs → is_info/verbose/debug_event (whitelist)
                                       ↓
                               chat.rs → ConversationCell
```

### Proposed Pipeline

```
raw events → operator_event.rs → ProjectionEventRecord (unchanged)
                                       ↓
                               PresentationReducer
                                       ↓
                               PresentationItem (typed, merged)
                                       ↓
                               Renderer (level-aware) → RenderedCell
                                       ↓
                               chat.rs → ConversationCell (tui-specific)
```

The `ConversationCell` enum may be replaced or simplified. The `RenderedCell`
carries enough information for both TUI and future non-TUI surfaces.

## Implementation Plan

### Phase 1: Types and Reducer (core model)

1. **Create `src/presentation.rs`** with:
   - `PresentationItem` enum and supporting types
   - `PresentationReducer` with reduction logic
   - `Renderable` trait with `render(level) -> Vec<RenderedCell>`
2. **Add unit tests** for the Reducer with representative event sequences.

### Phase 2: Connect to TUI

1. **Integrate Reducer** into `src/tui/projection.rs` or a new projection module.
2. **Replace `is_info_event` / `is_verbose_event` / `is_debug_event`** with
   `PresentationItem::is_visible_at(level)`.
3. **Replace `conversation_event_body` / `conversation_event_speaker`** with
   per-variant `render()` calls.
4. **Remove `[task]`, `[work-item]`, `[workspace]` prefixes** from level 3–5
   rendering.

### Phase 3: Active/Live Items

1. **Upgrade `ActiveActivity`** to use `PresentationItem::AssistantProgress` /
   `ActionGroup` with `ItemState::Live`.
2. **Coalesce** adjacent progress events into evolving live items.
3. **Separate** running indicators from transcript content.

### Phase 4: Snapshot Tests and Polish

1. **Add snapshot tests** for:
   - A read-only investigation turn at levels 3, 4, 5
   - A code-edit + verify turn
   - A failure + retry turn
   - A waiting/external-trigger scenario
2. **Refine Reducer rules** based on noisy real-world cases.
3. **Remove old filter functions** once the new pipeline is the sole code path.

## File Changes Summary

| File | Action |
|---|---|
| `src/presentation.rs` | **New.** Core model, Reducer, Renderer. |
| `src/tui/presentation_projection.rs` | **New (optional).** TUI projection of PresentationItems. |
| `src/tui/projection.rs` | **Modify.** Wire in Reducer output; deprecate whitelist filters. |
| `src/tui/chat.rs` | **Modify.** Render from `PresentationItem` instead of raw `ProjectionEventRecord`. |
| `src/operator_event.rs` | **Unchanged.** Keeps classification for trace/debug purposes. |

## Compatibility

- Raw event stream stays the same.
- `OperatorEventCategory` and `OperatorVisibility` remain for trace/debug use.
- The three whitelist functions (`is_info_event`, etc.) will be removed only
  after the new pipeline is stable and tested.
- `/events` panel is unaffected.
- Display mode names (`info`, `verbose`, `debug`) stay the same.

## Decisions

### Reducer is in-process, not persisted

The Reducer maintains ephemeral state (current live item, pending action
pairings). This state is not persisted. It recomputes from recent events on
each tick.

*Rationale:* The RFC on operator display levels already decided reduction state
should not be persisted in the first version. Most reductions are local
presentation conveniences.

### One item type, not per-level variants

There is no `Level4Item` vs `Level5Item`. One variant renders differently by
level. This keeps the Reducer simple (no mode awareness) and puts all display
logic in the Renderer.

### Lazy fields for expensive data

`full_stdout`, `full_stderr`, `full_diff` are `Option<String>`, not required.
The Reducer fills them only when the data is available and not prohibitively
large. Level 4 rendering never accesses them, so they are zero-cost when not
needed.

### `RenderedCell` as intermediate type

Rather than directly producing `ConversationCell` (which is TUI-specific),
introduce `RenderedCell` as a surface-neutral intermediate. The TUI `chat.rs`
converts `RenderedCell` to `ConversationCell`. Future non-TUI surfaces can
consume `RenderedCell` directly.

## Risks

- **Reducer complexity.** Event merging rules may have edge cases. Start with
  simple rules (command → one item, adjacent reads → group) and refine.
- **Live item mutation.** Updating an existing cell in ratatui requires care.
  Keep the TUI integration simple in Phase 2 and add live mutation in Phase 3.
- **Performance.** The Reducer runs in the TUI render loop. With O(recent-events)
  work per tick, this should be negligible.
