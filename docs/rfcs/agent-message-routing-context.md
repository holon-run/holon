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
When an agent forwards an incoming message, the new message records the relay
as `sender_agent_id`, retains the inherited trusted reply target, and records
the incoming message as `in_reply_to_message_id`. The original sender and
reply target are retained separately so a receiver can distinguish the relay
from the route that should receive a response.

## Projections

The routing context is exposed in model input, message lifecycle events, and
delivery receipts/records. The Web GUI timeline projects sender, recipient,
reply, correlation, and forwarded-route details when the event includes this
metadata. Older events without `routing_context` remain renderable without
inventing route information.
