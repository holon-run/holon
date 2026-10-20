# Conversation presentation

Shared presentation contract for Web, iOS and Android. This document owns the
source-icon semantics and turn-header interaction rules; platform design files
own styling and native implementation choices. Track adoption in
[issue #3622](https://github.com/holon-run/holon/issues/3622).

This is a UI contract, not a change to the conversation wire protocol, execution
lifecycle, origin or trust classification. A source icon is not an authority or
success indicator.

## Turn hierarchy

- Keep each executed turn identifiable by a quiet boundary, source, turn index
  and timestamp. Do not merge consecutive background turns into one reply.
- Give operator input and user-facing results visual priority. Put task
  receipts, commands, exit codes and execution activity inside execution details.
- Do not repeat `Turn ended` for normally ended turns. An ended turn does not
  mean the overall work is complete or accepted.
- Show duration as a compact value such as `4m39s`, without a visible `Took`
  prefix. Preserve its meaning in the accessible name.
- Keep meaningful running, waiting, syncing, unavailable and execution-failure
  states visible. A failed task does not by itself make its receiving turn fail.
  Do not claim recovery without evidence.
- A normally ended, collapsed turn with no visible input or reply should occupy
  a compact boundary row, not an empty reply area. Do not compact away missing
  result, truncated input, failure or unavailable notices.
- A reducer-only task receipt without model execution is not a separate
  assistant reply. Preserve any supported original-reply navigation.

## Source icons

Use the structured turn `presentation_class`, not message text, command names
or task status. The localized source name must remain available even when the
visible text is replaced by an icon.

| Class | English name | Semantic shape | Web reference (`lucide-react`) |
| --- | --- | --- | --- |
| `operator` | Operator message | Person | `User` |
| `task` | Task result | Tool/task | `Wrench` |
| `external` | External message | Message bubble | `MessageSquare` |
| `timer` | Scheduled check | Clock | `Clock` |
| `internal` | Background continuation | Branch/continuation | `GitBranch` |
| `system` | System wake | Automation/robot | `Bot` |
| `operational` | Runtime action | Tool/runtime operation | `Wrench` |

iOS and Android may use native symbols or their existing icon library. Match
the meaning and behavior, not identical SVG paths. Reusing the tool shape for
`task` and `operational` is intentional; their accessible names and source
details distinguish them. Do not distinguish these classes only by color.

## Independent header actions

### Source inspection

The source icon opens the canonical trigger message in the inspector (or the
platform's equivalent detail surface), without requiring execution details to
be expanded. For a turn with multiple inputs, use its first non-interjected
input; later inputs and interjections remain available in execution details.
Opening a source must not also toggle execution details.

The visible source describes the turn's structured classification; message
inspection must retain the selected input's actual identity and provenance.
Reuse existing authorized message readers. Do not fabricate full source text
from a truncated preview or infer new access from an icon.

If no inspectable trigger or inspection action is available, show a labeled,
non-interactive source icon rather than a button that does nothing.

### Execution disclosure

Use a single chevron in the header: right-pointing when collapsed (`›`),
down-pointing when expanded (`⌄`). The visible `Execution details` label is
unnecessary. Keep the localized action name, expanded state and controlled
content relationship accessible. The whole control, not just the glyph,
is interactive.

Keep task counts and task failure/cancellation summaries inside expanded
details, not beside the turn's execution status. Required user-facing blockers
and turn failures must remain visible outside folded logs.

This does not change active-turn expansion, manually chosen expansion, lazy
detail loading or deep-link reveal behavior. An activity/input deep link can
still expand execution details to reveal its target.

## Accessibility and platform adaptation

- On Web, use native buttons with keyboard activation and visible focus, a
  localized accessible action name, and a hover tooltip. Mark the decorative
  SVG itself hidden from assistive technology.
- On touch platforms, do not require hover to discover the source or action.
  Provide native accessibility labels/hints and an appropriate explanation
  through the platform's detail or help interaction.
- Apply each platform's standard interactive target sizes independently of
  glyph size; do not copy a small Web glyph as a native hit target.
- Preserve source inspection and disclosure access on narrow and landscape
  layouts. Respect reduced motion for disclosure animation.

## Adoption and verification

Web's reference implementation is
[`ConversationTimeline.tsx`](../../web-gui/app/src/features/agent/ConversationTimeline.tsx)
with duration rendering in
[`TurnElapsedTime.tsx`](../../web-gui/app/src/features/agent/TurnElapsedTime.tsx).
The links from each platform README indicate the shared target, not that the
platform has already implemented it. Keep iOS/Android adoption tracked
separately in #3622.

For each platform, verify source names and shapes, direct source inspection
while collapsed, independent disclosure activation and expanded state, readable
results, compact empty terminal turns, retained failure/missing-result notices,
and narrow-screen plus assistive-technology access. Web regression coverage
lives in `ConversationTimeline.test.tsx` and
`e2e/conversation-presentation.spec.ts`.
