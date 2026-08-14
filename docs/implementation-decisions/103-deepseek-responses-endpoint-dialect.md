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
