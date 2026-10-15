# Agent message routing context

## Status

Implemented incrementally with the agent-message delivery envelope.

## Contract

Every runtime-created agent-to-agent message may carry a trusted
`routing_context`. The context is runtime metadata, not model-controlled body
content, and is copied into the message lifecycle audit event and durable
delivery record.

The context contains:

- `message_id`: the delivered message identity.
- `sender_agent_id`: the immediate sending agent, when the sender is an agent.
- `recipient_agent_id`: the agent receiving this delivery.
- `reply_to_agent_id`: the trusted agent to use for a direct response.
- `correlation_id`: the delivery correlation, when present.
- `in_reply_to_message_id`: the message being answered or forwarded.
- `original_sender_agent_id` and `original_reply_to_agent_id`: the original
  route preserved across forwarding.

Missing optional values are omitted for compatibility with older persisted
messages. Consumers must not infer routing from the message body or allow body
text to override these fields.

## Forwarding semantics

For a direct delivery, the immediate sender is also the default reply target.
A send to the trusted sender or reply target of the selected incoming message
inherits that route and is treated as a direct response. A send to any other
agent starts a new direct route unless the caller explicitly sets `forward`
on `SendAgentMessage`.

When an agent intentionally forwards an incoming message, the new message
records the relay as `sender_agent_id`, retains the inherited trusted reply
target, and records the incoming message as `in_reply_to_message_id`. The
original sender and reply target are retained separately so a receiver can
distinguish the relay from the route that should receive a response. The
`forward` flag is an intent signal only; it cannot supply or override routing
identities.

`SendAgentMessage.reply_context_id` selects a runtime-provided original message
ID, not a sender, correlation, or authority. Eligible routes are reconstructed
from the live canonical execution and its nonterminal `TurnRecord.input_message_ids`,
using the original durable runtime-owned agent-message envelopes attached to that
turn. A batched follower may retain its original ingress turn assignment:
the existing `idle_input_batch` source ID/reference plus atomic `Interject`
queue consumption and turn input admission prove its attachment. The durable
current execution binding links the turn to its canonical open activation;
an attempt's original turn assignment is not a substitute for that binding.
The anchor's canonical source identity, unchanged optional ingress turn ID,
and `Dequeued` queue claim prove its route even when that ingress ID is absent.
No independent routing ledger is maintained. Recovery of the same live
execution reconstructs the same routes; unknown IDs, other turns, terminal
executions, changed activation, control fences, or owner completion cannot reuse
them. Selection is revalidated in the delivery admission transaction.
For interrupted-execution replay, an original envelope may retain its source
turn assignment. It must be admitted again in the live turn, with canonical
recovery linkage and source-turn admission evidence; replay text alone is not
enough to make a foreign-turn route eligible.
The predecessor must be canonically `Interrupted` with the same agent, source,
and execution binding. If its optional turn link was never persisted, the
original source turn's batch activation, anchor, and trigger prove that admission
instead; the recovery attempt's inherited turn link does not bind the predecessor.

One eligible route keeps implicit replies (an unrouted latest input still stops
implicit inheritance). If multiple routes are eligible, replies or forwarding
without a selector fail with the available IDs, including multiple correlations
from the same sender. A proactive send to an unrelated third party remains a
new direct route, even with a valid selector; `forward=true` requires a unique or
explicitly selected source and preserves the relay/original-reply semantics.
Pure task-result inputs do not create agent-message routes. Existing invocation
durable-reply obligations remain unchanged.

## Projections

The routing context is exposed in model input, message lifecycle events, and
delivery receipts/records. The Web GUI timeline projects sender, recipient,
reply, correlation, and forwarded-route details when the event includes this
metadata. Older events without `routing_context` remain renderable without
inventing route information.

## Low-noise collaboration guidance

Issue #3530 P0 changes runtime-injected collaboration guidance on both sides,
not delivery admission, queue consumption, routing, trust, or task settlement.
Routine progress, compile/retry updates, unchanged test status, and self-resolved
problems stay in observable output or work records. Unsolicited messages are for
final delivery, blockers requiring the recipient's action, coordination-relevant
contract changes, or dependencies becoming ready. Parents avoid acknowledging
pure acknowledgments, repeating status requests, and relaying redundant or
superseded updates.

For progress already available in records, parents prefer a bounded CLI read
when the CLI and command tool are available:

```sh
holon tail --agent child --include-progress --limit 20 \
  --max-events 100 --max-bytes 65536
```

Ask a targeted question only when records cannot supply necessary new
information, decisions, or coordination. Do not poll merely to wait. This is
agent-scoped observation, not result consumption, proof of task completion, or
a replacement for `WaitFor(task_result)`; see
[agent output observation](agent-output-observation.md).

Explicit reply obligations remain authoritative, including the runtime reply
expectation for `InvokeAgent(existing_agent)`: send a durable reply when an answer
is available even if a delayed request is otherwise covered by newer evidence.
Supervised children deliver their concise final report through the existing
task-result channel without also sending the same result with `SendAgentMessage`.
Necessary blockers, coordination updates, and verification evidence are not
discarded. The delegated-task guidance includes this boundary even when the
child has no invocation or messaging tools.

P0 is a prompt-level reduction in unnecessary messages, not a guarantee of model
behavior or fewer activations. Bounded batch consumption and turn-local safe
attachment remain separate P1/P2 stages.
