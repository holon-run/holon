# DeepSeek Responses Endpoint Dialect

## Choice

Keep `deepseek@default/*` on the existing Anthropic Messages endpoint and add
`deepseek@responses/*` as an explicit opt-in route over the Responses API.

The Responses route uses an endpoint-specific contract:

- streaming Responses transport
- complete conversation history on every request
- no `previous_response_id` continuation
- no provider remote compaction
- no inherited Anthropic-native web-search capability

Plain `reasoning_text` output is stored as `ModelBlock::ReasoningText`. It is
replayed as a Responses reasoning item, but it is excluded from user-visible
assistant text and from transports that cannot preserve that semantic block.

## Reason

DeepSeek's Responses endpoint is wire-compatible with only part of the OpenAI
Responses contract. Treating it as the standard stateful dialect would make
provider restart behavior depend on unavailable remote response state and
would attempt unsupported remote compaction.

Keeping the reasoning block durable preserves the exact history needed after a
tool call or runtime restart without leaking provider reasoning into the
operator-facing result.

## Preserved boundary

This route does not change the legacy DeepSeek default, introduce a model
alias, or change the OpenAI and xAI endpoint contracts. Endpoint dialect is
selected from the resolved canonical `(provider, endpoint)` identity rather
than from the model name.

## Benchmark protocol

Route comparisons use repeated task-level pairs rather than one aggregate run.
Each task/repetition runs both canonical routes with a deterministic seeded
order and isolated branch, worktree, artifact, and agent identities. Raw
per-run results remain authoritative; the harness additionally emits a paired
summary with success, duration, provider-attempt, retry/error, reasoning-token,
and transport-specific cache-token fields. Pair deltas use `runner_b - runner_a`.

The checked-in pilot suite uses `deepseek-v4-flash`, five repetitions, serial
runner execution, cooldown, and disabled provider fallback. Running that paid
suite and using `deepseek-v4-pro` for route confirmation/default-route evidence
remain separate operator-authorized steps. Benchmark infrastructure does not
change `deepseek@default`.
