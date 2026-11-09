# Agent Status Display Projection

## Status

Accepted for Web, iOS, and Android client alignment. Implementation is tracked
in [#3973](https://github.com/holon-run/holon/issues/3973).

## Context

Holon already separates durable runtime state from derived agent posture in
[Agent State Model And Runtime Projection](./agent-state-model.md). The next
boundary to make explicit is the display contract used by Web, iOS, and Android.
The TUI may reuse the semantic projection without adopting graphical icons.

Without a shared display projection, clients tend to combine low-level
lifecycle, scheduling posture, queue counters, task counters, and waiting
details independently. That produces confusing states such as a single agent
appearing as both `Ready` and `Waiting`, or as both `Stopped` and `Running`.

## Decision

User-facing agent lists should display one primary status per agent. That
status is a display projection, not a new canonical runtime state.

The projection is derived from these inputs:

1. `AgentSchedulingPosture` / `scheduling_posture` as the primary semantic
   source.
2. Compact counters and execution/wait hints as compatibility inputs only when
   the posture is absent or unknown.
3. `AgentStatus` / lifecycle only as a fallback or secondary detail.
4. Waiting conditions and waiting intents as explanatory detail, not as a
   second primary badge.

## Display precedence

Clients must not reconstruct scheduler precedence from counters when a known
`scheduling_posture` is present. A stopped/archived lifecycle or posture takes
precedence over stale activity hints; otherwise map the known posture directly:

| Posture | Display kind | Default English label | Icon meaning | Tone |
| --- | --- | --- | --- | --- |
| `stopped` (legacy `archived`) | `stopped` | Stopped | Circle with cross | muted |
| `waiting_for_operator` | `needs-input` | Needs input | Circle with exclamation | attention |
| `active_turn` | `running` | Running | Activity spinner | accent |
| `has_queued_input`, `has_runnable_work` | `runnable` | Runnable | Circle with right arrow | accent |
| `waiting_for_task`, `waiting_for_external` | `waiting` | Waiting | Clock | muted |
| `blocked` | `blocked` | Blocked | Lock | danger |
| `idle` | `ready` | Ready | Circle with check | muted |
| unavailable / unrecognized | `unknown` | Unknown | Circle with question | muted |

The display kind is distinct from tone: `running` and `runnable`, or `waiting`
and `blocked`, must not collapse merely because a client reuses a color.
Platform-native equivalents of the icon meanings are allowed.

Queued input is already supplied input, not a request for another operator
response. It maps to `runnable`, not `needs-input`. Active background task
counts alone do not prove an active turn or a task-result wait. Neither unread
briefs nor preview text is evidence of scheduling posture.

For older summaries with absent/unknown posture, use these fallbacks in order:
stopped/archived lifecycle; active execution (`current_run_id` or a running
lifecycle); queued input (`runnable`); an explicit wait summary (`waiting`);
idle/ready/asleep lifecycle (`ready`); otherwise `unknown`. A fallback wait count
does not identify an operator wait, so it must not claim `needs-input`.
Normalize legacy hyphen/underscore status spellings at this boundary.

Deletion is a separate, temporary client lifecycle operation: an existing
`Deleting` indicator may replace the status while deletion is in progress.
It must not suggest running work and is not a new scheduler posture.

## UI contract

Agent list rows should:

- show at most one primary status badge;
- prefer posture-derived labels such as `Needs input`, `Running`, `Runnable`,
  `Waiting`, `Blocked`, `Stopped`, or `Ready`;
- put lifecycle, current work state, and waiting/task counters in secondary
  text or tooltips;
- avoid independently rendering lifecycle and attention badges side by side;
- avoid treating waiting as idle. Waiting means the runtime has an explicit
  wake condition or wait intent.

### Presentation primitives

- A primary status icon is required in agent list rows and selected-agent
  headers on all three graphical clients. Use one icon for one display kind,
  not a lifecycle icon plus a competing attention icon.
- Text is auxiliary. A compact row may show only the icon; wider layouts may
  show one localized status label beside it. Do not require a new status line
  below every agent or replace the conversation/brief preview with status.
- Web icon-only rendering must expose the localized label through an accessible
  name and tooltip. Mobile must expose the label to VoiceOver/TalkBack and make
  the same status readable in the agent detail/header; hover is not sufficient.
- Color is supplementary, never the only distinction. Spinner motion is only
  for current `running` activity and must respect reduced-motion preferences.
- Waiting reason, lifecycle, queued inputs, and task counts belong in secondary
  detail. Do not render them as additional competing primary status badges.
- Avatar expression is decorative and may reflect this same projection, but
  must not be the only status indicator or derive a conflicting status.
- Cached/offline summaries must be marked as such and must not animate or
  advertise stale activity as current. When freshness is unavailable, use an
  `unknown` primary status and retain last-known information only as explicitly
  cached secondary detail.

`offline` and `unloaded` compatibility hints mean freshness is unavailable, not
that the scheduler is stopped. They therefore use `unknown` rather than
replaying a cached running/waiting posture. A deleted lifecycle uses the
stopped presentation. A static activity glyph is an acceptable native
equivalent of the running spinner; animation is not required.

Agent detail views may show more state, but should keep these axes visually
separate:

- lifecycle: process/control state;
- scheduling posture: what the agent can do next;
- waiting reason: why the scheduler is paused;
- work/task details: concrete current activity.

## API expectation

`/agents/list` should eventually expose enough compact summary fields for
clients to render the primary display status without issuing one `/state`
request per agent. A detail page may fetch the selected agent's full state.

During migration, clients may compute the display status from the existing
summary fields, but should keep that computation centralized and aligned with
this RFC.

## Cross-client migration and acceptance

Web, iOS, and Android should each centralize projection in one testable helper
and use it for list and header rendering:

- Web: preserve compact icon-only rows while distinguishing runnable from
  ready and blocked from waiting.
- Android: replace independent text/tone derivations with the shared semantic
  mapping and add the primary icon without losing the brief preview.
- iOS: add the same primary icon and accessible label without promoting
  lifecycle or preview heuristics to primary status.

The three test suites must consume the same language-neutral fixture under
`tests/fixtures/agent-status-display.json`. Cover every posture, stopped versus
stale running hints, known idle versus stale counters/run IDs, operator wait
versus queued input, background tasks without a wait, unknown/legacy fallback,
and cached/offline presentation. Assert display kind, icon meaning, and tone;
localized labels may differ while retaining the same meaning.

Run each changed client's existing test/build gates and verify icon-only
accessibility plus list/header consistency. The implementation issue and PR
must record actual outcomes and any platform limitations.

This is a display-only step. It should not change scheduler behavior or the
runtime's canonical posture derivation.
