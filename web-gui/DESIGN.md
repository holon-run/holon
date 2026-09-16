---
design_system: holon-local-web-gui
version: 0.3.0
status: standalone-app-contract
audience:
  - local operators
  - power users
  - developers
intent:
  product_mood: "calm local runtime workbench"
  theme_policy: "light-first, dark optional"
  primary_jobs:
    - "Understand what long-lived agents are doing now."
    - "See work, waits, blockers, and results without reading raw logs first."
    - "Escalate from brief state to activity and trace only when needed."
tokens:
  color:
    background: "#f8fafb"
    background_soft: "#e8eef3"
    panel: "#ffffff"
    panel_raised: "#f8fafc"
    panel_subtle: "#f3f6fa"
    border: "#d8e0e7"
    border_muted: "#e6edf2"
    text: "#182230"
    text_muted: "#52647a"
    text_faint: "#73849a"
    primary: "#087ea4"
    primary_strong: "#05617f"
    primary_soft: "#e2f3f8"
    primary_text: "#ffffff"
    success: "#16834f"
    success_soft: "#e8f7ee"
    warning: "#a16207"
    warning_soft: "#fff3d1"
    danger: "#c24158"
    danger_soft: "#fff0f3"
    operator: "#b832c4"
    operator_soft: "#faeafd"
    external: "#b832c4"
    tool: "#475569"
    internal: "#64748b"
  typography:
    ui_family: "Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
    mono_family: "'SFMono-Regular', 'Cascadia Code', 'Roboto Mono', ui-monospace, monospace"
    scale:
      display: "34px/1.05"
      h1: "26px/1.15"
      h2: "20px/1.25"
      h3: "16px/1.35"
      body: "14px/1.55"
      small: "12px/1.45"
      code: "12px/1.5"
  radius:
    xs: "4px"
    sm: "6px"
    md: "8px"
    lg: "8px"
    xl: "12px"
  spacing:
    1: "4px"
    2: "8px"
    3: "12px"
    4: "16px"
    5: "20px"
    6: "24px"
    8: "32px"
    10: "40px"
  shadow:
    panel: "0 12px 32px rgba(16, 24, 40, 0.08)"
    focus: "0 0 0 3px rgba(4, 118, 168, 0.18)"
component_tokens:
  shell:
    max_width: "1440px"
    left_nav_width: "240px"
    detail_width: "360px"
  card:
    background: "{color.panel}"
    border: "1px solid {color.border_muted}"
    radius: "{radius.lg}"
  button:
    height: "36px"
    radius: "{radius.sm}"
    primary_background: "{color.primary}"
    primary_text: "{color.primary_text}"
  status_pill:
    radius: "999px"
    height: "24px"
  density:
    default: "comfortable"
    compact_rows: "44px"
    comfortable_rows: "58px"
---

# Holon Local Web GUI Design Contract

This file defines the initial visual and interaction contract for Holon's
first-party local Web GUI. It is intentionally small enough for coding agents
to read before implementing pages, while still preserving the product shape
that should not be lost across sessions.

The current implementation path is **standalone app first**:

- keep `web-gui/prototype/` as the reviewable static prototype;
- build production UI under `web-gui/app/`;
- call existing local Holon interfaces during development;
- do not add backend API routes in the Web GUI work unless a later task
  explicitly authorizes it;
- do not embed the app into `holon serve` yet.

The Web GUI is a local control room for a headless, event-driven runtime.
The conversation uses a quiet chat reading surface: operator inputs, live
execution, and delivered results. Runtime tools remain available through
progressive disclosure and the object inspector.

## Product principles

1. **Readable work.** Group conversation history by native turn and show
   delivered briefs in full. Keep WorkItems, waits, and execution evidence
   accessible without turning every event into another message bubble.
2. **Progressive disclosure.** The default view is brief and human-readable.
   Verbose activity, tool calls, and debug details are one click deeper.
3. **Local-first trust.** The UI must make local, operator, external, tool, and
   internal origins visually distinguishable without making normal use noisy.
4. **Calm observability.** Use restrained motion, clear status chips, and
   explicit timestamps. Avoid alert colors unless the operator must act.
5. **Agent continuity.** An agent is long-lived. The UI should make current
   focus, queued work, waiting state, and recent completions feel persistent.

## Information architecture

The initial GUI has a global shell and one durable conversation per agent.
Holon should not present multiple threads or sessions for the same agent unless
the runtime model grows that concept later.

The left navigation contains only global surfaces:

- **Dashboard:** the home surface for all agents and their current state.
- **Search:** cross-agent lookup for messages, briefs, WorkItems, tool
  executions, and memory records.
- **Skills and Agent Templates:** browse installed resources, inspect their contents,
  and expand installation or remote-source controls when needed.
- **Settings:** runtime configuration, providers, model defaults, and
  local/remote connection details.

A standalone global Activity page is intentionally out of the first prototype.
The current runtime has event and transcript records, but most useful activity
evidence is still agent-scoped. Dashboard may show high-signal summaries, while
full activity/trace inspection belongs inside the selected agent page.

Agent-scoped surfaces belong inside the selected agent's conversation page or
the on-demand object side panel:

- current WorkItem and work-spine
- queue, waits, blockers, and recent completions
- recent briefs and memory projection
- tool activity and trace/debug evidence

The right side panel is an object inspector, not a permanent agent dashboard.
It opens for concrete objects such as WorkItem detail, diff previews, files,
web pages, memory/source detail, and tool traces. It should be closed by
default so the conversation remains the primary surface.

The dashboard should answer:

- Which agents are active, waiting, or need input?
- What work is currently focused?
- What was recently completed?
- Are there external wakes, failed tasks, or blocked work?
- Which agent should I open next?

The agent detail page should answer:

- What is this agent doing now?
- What is the current WorkItem and plan?
- What does the operator need to say or decide?
- What recent operator messages, agent replies, and work events led here?
- What activity happened behind the brief?
- Can I safely inspect trace/debug details without making them the default?

The bottom-left runtime strip should answer:

- Am I connected to a local or remote runtime?
- Is the connection healthy?
- Which backing store or endpoint am I reading?

The selected workspace belongs in the agent conversation status line, not in
the composer or as a taller standalone card. Workspace name and path are
page-level execution context: they affect how the operator reads every
message, WorkItem, and tool result on the page.

The composer should stay focused on the operator input being sent,
attachments, the next-turn model selector, and send action. Do not show default
authority labels such as `operator trusted` in the normal composer. Display
level belongs in the page top bar, not in the composer. Selecting the model
should open a model/agent-settings side panel that explains the effective
model, source, reasoning effort, and fallbacks.

The current WorkItem summary should have enough horizontal space to show the
objective. It must also have an explicit empty state because an agent can be
ready or waiting without a current WorkItem.

The default agent composer should be a normal message box, not a form for
manual WorkItem creation. The UI sends operator messages; the agent decides
whether to chat, clarify, create a WorkItem, update a WorkItem, wait, or run.
Do not expose internal relationship labels such as "attach to current work" as
primary user actions unless the runtime has a concrete operator-facing API and
the wording is understandable without implementation knowledge.

The default conversation stream should avoid implementation labels. Do not show
RuntimeDb paths or explicit `operator` / `brief` prefixes in Info mode. Role
and provenance can be inferred from layout by default and inspected through
Verbose, Debug, hover details, or context panels. Message meta should
be visually quiet.

## Layout

Use a collapsible three-zone shell:

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ Top bar: current global page or selected agent                            │
├───────────────┬─────────────────────────────────────────────┬────────────┤
│ Left nav      │ Dashboard or agent conversation              │ Side panel  │
│ Dashboard     │ Dashboard lists all agents                   │ WorkItem    │
│ Search        │ Agent page has one conversation per agent     │ Diff/File   │
│ Settings      │                                             │ Web/Trace   │
│ Active agents │                                             │ Memory      │
│ Local/remote  │                                             │             │
└───────────────┴─────────────────────────────────────────────┴────────────┘
```

The navigation is 224px expanded or 72px compact. The inspector defaults to
380px, with a 320px minimum, and reserves at least 640px for the conversation
when displayed beside it. Collapse navigation temporarily before constraining
the inspector's preferred width. When even compact navigation and both panes
cannot fit, display the inspector as a complete view with an explicit return
to the conversation. Do not partially cover the conversation or composer.

### Inspector and file reading

- Overview, Files and Details are stable entry points. Opening an execution
  row selects its existing object renderer and highlights the corresponding
  row; incoming events never replace a file the user is reading.
- Content selection and display mode are independent. Maximize fills the work
  area except for compact desktop navigation; on narrow screens it fills the
  viewport. Restore preserves the preferred inspector width and navigation
  state, subject to the available space.
- Keep maximize/restore controls, separator double-click, Cmd/Ctrl+. and the
  Escape sequence (expanded to normal to closed). The width separator supports
  arrow keys. The covered conversation is inert, remains subscribed to updates,
  and does not acknowledge newly arriving content as read until visible again.
- Closing the inspector restores focus to the invoking control when it still
  exists, with the composer as fallback. Closing file content unmounts previews
  so media does not continue playing in a hidden pane.
- A file browser at least 960px wide can show a collapsible 240px directory
  column beside the preview. Narrower browsers switch between directory and
  preview. Directory navigation uses existing APIs without recursive scans.
- Preserve file selection, directory, filter, sorting, rendered/source mode
  and scroll positions when switching inspector content or size. Keep a bounded
  in-memory history: six file-browser locations, each with up to four previous
  file selections, and up to 32 inspector scroll positions. State belongs to
  the current runtime/auth scope, agent, workspace and execution root; it is
  reset across scope changes and is not persisted to disk.
- File links update the directory context and support returning to the prior
  file or invoking tool. Use the existing Markdown, source, image, media, PDF,
  download, error and truncation renderers. Explicit refresh still reads fresh
  content; UI restoration is not a background file polling mechanism.
- Keep overview information compact: identity/status, current work, workspaces,
  then capabilities and settings. Runtime facts and lifecycle controls are
  disclosures. Workspace names open the browser; full paths remain available
  on expansion. Use dividers rather than nested raised cards.

## Global workspace pages

Dashboard, Search, Skills, Agent Templates, and Settings use the same quiet
surface as the conversation. `PageHeading` provides one 24px page title
(22px on narrow screens), optional supporting text, and wrapping actions.
Content is limited to 1180px, with 36px desktop and 16px mobile side padding.
Body text is 14px, supporting text and form controls 13px. Use neutral surfaces,
light separators, and 8–10px corners; reserve accent colors for actions and state.

- Dashboard retains the agent roster and visible lifecycle/attention state.
  Metrics form one compact strip; task text wraps within each agent card.
- Search keeps the query and filters together and uses a continuous result list.
  Result provenance, source disclosure, and agent navigation remain available.
- Skills use a single list so descriptions have room beside actions. Templates
  retain a responsive card grid. Installation controls and template remote
  sources are explicit disclosures, collapsed initially; closing a disclosure
  retains its unsent form values.
- Settings use a compact runtime summary and one section bar. Arrow keys,
  Home, and End move the selected tab and focus together; Tab enters its panel.
  Configuration forms and error feedback keep their existing save semantics.
- `styles/workspace-pages.css` scopes these rules to `.workspace-page` and is
  loaded after the legacy shared styles. Conversation and inspector rendering
  keep their own density. Grid tracks must shrink to zero rather than letting
  long names or paths force page-level horizontal scrolling.

Verification: real runtime pages inspected at 1440, 1024, 768, and 390px,
including expanded installation/source forms, library filtering and detail
navigation, search results, and Settings tabs. Configuration writes and skill
installation are not performed as part of visual verification.

## Conversation reading contract

Pending operator messages remain user bubbles below the conversation. Pending
task, timer, external, and other background messages share a collapsed
"Pending events" disclosure before the latest turn, with a count and compact
source-labelled rows. Expand a row to read its bounded preview in a scrollable
area. Source comes from canonical metadata, never body text; older daemons
without source metadata use a neutral label. Order events by queue arrival time,
and remove them from this area when assigned to a turn.

Each turn has a status-and-duration disclosure above its result: `Working ·
0:23` while active, then `Completed · Took 1:23` (or stopped/failed/waiting).
The whole row toggles execution details. The timer uses summary-level canonical
start/terminal timing, updates only its own component once per second, and
freezes at execution termination even if the result has not loaded. Cancellation
and setup failures record elapsed processing time rather than a zero placeholder. Old
runtimes without timing still show the status, without an invented duration.
Omit assistant rounds without displayable text (including thinking/tool-only
rounds) from the process, while retaining their separate tool activities and
canonical records. If a later revision adds text, render it normally.
After execution ends, omit the last non-empty assistant activity from the process
only when its full displayed text matches a loaded Brief in the same turn (apart
from outer whitespace and CRLF line endings). Keep it while Briefs are loading,
on Brief load failure, or when the text differs; do not use substring/fuzzy matching or
alter the canonical activity log. Earlier assistant progress, tools, errors,
and waits remain visible. Apply the recent-activity limit after this filtering.
Process content and Briefs share the same left edge and width; no enclosing
process card, left rule, or extra indentation implies a nested result. Activity
uses secondary text and keeps its object-inspector links. Existing automatic
folding waits for a readable Brief and preserves explicit expansion or active
reading of the process.


- Center a column up to 760px wide on a white surface. Use 16px result text,
  14px progress text, and 12–13px metadata. The neutral navigation is 240px
  wide; the object inspector starts closed and becomes an overlay below 1340px.
- Operator inputs use soft gray right-aligned bubbles. Delivered briefs use
  full-width Markdown without card borders. Keep copy and time below the result.
- Place the execution disclosure before the result. Historical turns start
  collapsed and do not fetch activity until opened. System, timer, external,
  and task inputs keep a labeled provenance disclosure instead of impersonating
  an operator message.
- Active execution opens automatically. Show readable assistant progress and
  compact tool rows, initially retaining the latest eight activities plus
  errors/waits. Earlier activity and older server pages are explicit actions.
  Clicking an activity opens the existing inspector directly, including its
  tool-specific rendering for commands, patches, images, and WorkItems. Assistant
  progress also has a keyboard-accessible inspect button. Selecting text or
  opening a Markdown link does not trigger inspection.
- Execution ending alone does not imply result delivery. A turn observed running
  stays open while its result is pending or referenced briefs are still loading.
  Once execution has ended and the brief is readable, collapse the process over
  220ms without waiting for transport settlement. Manual expansion
  wins over automatic folding; focus or selection inside process content keeps
  it open. Respect reduced motion.
- Failure, interruption, fallback, and wait notices remain visible outside the
  fold. A readable brief with `settled=false` does not show a
  misleading waiting banner; this does not change its canonical settlement.
- Follow content growth only while the reader is at the bottom. User scrolling
  upward pauses following and reveals “Back to latest”. Preserve a visible
  content anchor when result hydration or folding changes heights above it.
- Keep the composer aligned with the reading column, with a 24px radius and a
  compact utility row. Below 760px, use icon navigation and narrower gutters.

## Display levels

Display level is a first-class UI concept inherited from the TUI, but in the
GUI it means progressive information disclosure rather than terminal verbosity.

| Level | Default audience | Shows | Hides |
|---|---|---|---|
| Info | most users | current status, final briefs, blockers, operator actions | raw tool logs |
| Verbose | power users | timeline, task lifecycle, child-agent summaries | raw provider/debug payloads |
| Debug | runtime maintainers | tool inputs/outputs, event provenance, state transitions, diagnostic IDs | secrets, full prompt dumps by default |

Rules:

- The default level is **Info**.
- Switching level should not change runtime state.
- Debug must visually mark untrusted external content.
- Raw trace/audit inspection is a separate side-panel or event-inspector surface,
  not a main conversation display level.
- Secrets and capability URLs must never be displayed by default.

## Visual language

Holon should feel like a precise local instrument:

- Light-first workbench surfaces with mineral blue-gray navigation and cyan/teal runtime accents.
- Dark mode remains available for users who prefer a control-room or trace-heavy view.
- Soft panels, subtle borders, compact status chips.
- Monospace only for IDs, commands, paths, and structured traces.
- Use color as reinforcement, not the only signal.
- Prefer explicit labels: `current`, `queued`, `waiting`, `completed`,
  `external`, `operator`, `tool`, `internal`.
- Borrow mature chat density from Codex-style workbenches, but keep Holon visually distinct:
  no warm beige default palette, no generic project-chat semantics, and no file diff as
  the primary runtime evidence.

Avoid:

- Generic blue SaaS dashboards.
- Deep dark pages as the only default experience.
- Chat bubbles as the only organizing primitive.
- Large hero marketing sections inside the product UI.
- Terminal-green hacker aesthetics.
- Hidden background work without visible lifecycle.

## Component guidance

### Runtime status strip

Shows local/remote connection posture, endpoint/backing store, and connection
health. Keep it compact and always visible at the bottom of the left shell.

### Agent card

Required fields:

- agent id / display name
- visibility and profile
- current focus
- lifecycle state
- queued / waiting / running counts
- last operator-visible brief

### Work item card

Required fields:

- objective
- readiness / state
- plan status
- todo progress
- wait reason if blocked
- latest completion or next action

### Timeline event

Every event should preserve origin. Use the origin color tokens:

- operator: purple
- external: magenta
- tool/task: blue
- internal runtime: slate

### Action controls

Primary actions should be explicit and low-risk:

- `Continue`
- `Provide input`
- `Open work item`
- `Inspect activity`
- `Copy local path`

Potentially destructive controls must be visually secondary and require
confirmation:

- `Stop task`
- `Cancel wait`
- `Detach workspace`
- `Shutdown runtime`

## Prototype acceptance criteria

The initial prototype should demonstrate:

- A dashboard focused on the full agent roster.
- Agent cards that open the selected agent's single conversation.
- An agent conversation page with display-level switching.
- WorkItem, queue, waits, memory, and activity surfaces inside the agent page rather than global left navigation.
- A bottom local/remote runtime strip showing local-first connection posture.
- Static sample data that reflects Holon's actual runtime concepts.
- No external network dependencies.

## Standalone app implementation contract

The first production Web GUI is a standalone local web project, not an embedded
server asset. It may run from a normal frontend dev server and connect to a
locally running Holon runtime.

Implementation priorities:

1. **Dashboard.** Show the agent roster, lifecycle/readiness state, active
   workspace context, current WorkItem summary, recent briefs, waits, and
   operator-attention signals.
2. **Agent conversation.** Show one durable conversation per agent with a
   composer, current WorkItem context, display level controls, activity
   evidence, and object inspector entry points.
3. **Runtime adapter.** Use a small client layer that can read existing Holon
   control-plane surfaces when available and fall back to local fixtures during
   UI development.
4. **Search and Settings.** Keep as lightweight shells until Dashboard and
   Agent conversation behavior are stable.

Backend constraints:

- Reuse existing Holon HTTP/control-plane interfaces.
- If the UI needs data that existing interfaces cannot provide, document the
  gap and propose a GitHub issue instead of adding the route in this slice.
- Do not introduce GUI-only runtime semantics that the TUI/runtime model does
  not already support.
- Refer to the TUI when deciding how to expose display level, activity
  evidence, and runtime provenance.

Commit discipline:

- Keep one logical work item per commit.
- The prototype contract, standalone app scaffold, dashboard implementation,
  agent conversation implementation, and API-gap documentation should remain
  separate review slices.

## Implementation notes for future agents

- Keep prototype files static until the information architecture stabilizes.
- Do not add a heavy frontend framework solely for the prototype.
- When moving to production UI, preserve this contract as the design source of
  truth or replace it with a deliberately updated contract.
- Production integration should call the existing HTTP control plane and stream
  events through the existing event surfaces rather than inventing a second
  runtime protocol.
