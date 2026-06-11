---
design_system: holon-local-web-gui
version: 0.1.0
status: prototype
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
    left_nav_width: "260px"
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

The Web GUI is a local control room for a headless, event-driven runtime. It
should not look like a generic chat app, a metrics-only SaaS dashboard, or a
terminal log viewer. The default experience should explain what is happening,
what needs operator attention, and what has been delivered.

## Product principles

1. **Work-first, not chat-first.** Conversations matter, but WorkItems,
   waits, task results, and final briefs are the durable units operators need
   to understand.
   The agent detail page still needs a plain message composer and recent
   history; the difference is that messages are shown with their WorkItem,
   turn, and brief relationships instead of becoming the only organizing model.
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

For narrower screens, collapse the left nav to icons and make the side panel
an overlay drawer. The prototype can focus on desktop first.

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

## Implementation notes for future agents

- Keep prototype files static until the information architecture stabilizes.
- Do not add a heavy frontend framework solely for the prototype.
- When moving to production UI, preserve this contract as the design source of
  truth or replace it with a deliberately updated contract.
- Production integration should call the existing HTTP control plane and stream
  events through the existing event surfaces rather than inventing a second
  runtime protocol.
