# 050 Anthropic Claude Code Prompt Cache Lowering

Holon uses a provider-lowering strategy named `claude_code_prompt_cache` by
default for Anthropic-compatible prompt-cache behavior. The provider-native
Messages API shape remains available as `messages_native` for endpoints that
need a conservative request shape.

Live cache probes showed that the decisive difference was request shape, not
streaming: a Claude Code-like body with stable prompt material in system blocks,
normal tools, body-level betas, stable metadata, and one rolling message-tail
marker repeatedly produced high cache reads against the same compatible
endpoint where Holon's previous message-heavy shape missed often.

When `HOLON_ANTHROPIC_CACHE_STRATEGY=claude_code_prompt_cache` is enabled, the
Anthropic transport moves the provider prompt frame's context blocks out of the
first conversation message and into cacheable system prefix blocks. The runtime
still builds the same replayable provider turn request. This keeps prompt
semantics provider-neutral while allowing the Anthropic wire shape to match the
cache behavior that the live probes validated.

The runtime default is `claude_code_prompt_cache`. Operators can temporarily opt
out with `HOLON_ANTHROPIC_CACHE_STRATEGY=messages_native` if a compatible
endpoint has request-shape issues. Legacy aliases `current`,
`claude_cli_like`, and `claude-cli-like` remain accepted for existing configs.

Cache lowering and beta injection are separate controls. For the official
Anthropic provider, if no explicit `HOLON_ANTHROPIC_BETAS` value is provided,
the default strategy uses the same betas that the successful live probes used:
`claude-code-20250219,prompt-caching-scope-2026-01-05`. Anthropic-compatible
third-party providers use the same `claude_code_prompt_cache` lowering by
default but do not auto-inject those Claude-specific betas unless the operator
sets `HOLON_ANTHROPIC_BETAS` explicitly.

The Rust `Default` implementation for `AnthropicContextManagementConfig` remains
neutral and does not imply live runtime defaults. Tests and fixtures should set
the strategy they need explicitly; environment/config resolution owns the
operator-facing default.

Diagnostics record the effective strategy, model, betas, and system/message
cache-control counts. These fields are for benchmark analysis and operator
inspection; they should not feed back into runtime scheduling or prompt
assembly.

The lowering preserves prompt stability boundaries. Stable and agent-scoped
system/context blocks remain in the cacheable system prefix, while turn-scoped
context blocks are placed ahead of wire history (see the #3225 update below).
The rolling conversation marker follows the latest cacheable content block,
including Anthropic `tool_result` blocks after tool-only rounds; the runtime
conversation is not mutated.

## Context layout update (2026-09-22, #3175/#3176)

Live probes against real Anthropic-compatible endpoints (dashscope, deepseek,
bigmodel) showed that keeping turn-scoped context in the initial user message
invalidated the conversation history prefix on every turn (a changed
head breaks prefix matching for everything after it). At that time, both cache
strategies shared this context layout:

- Non-TurnScoped context blocks ride the system prefix on both strategies
  (including `messages_native`, which previously relied on upper-layer
  materialization into `conversation[0]` and silently dropped context for
  unmaterialized frames).
- TurnScoped context blocks are re-attached at the conversation tail, inside
  the final user message after the latest tool result (mirroring Claude Code
  system-reminder placement), never carrying `cache_control`. The rolling
  marker stays on the last history block, so per-turn context changes stay
  outside the cached prefix.

## Same-turn continuation layout (2026-09-25, #3225)

The tail layout avoided cross-turn history invalidation, but repeated the
TurnScoped context as uncached input on every provider round within a turn.
Both strategies now strip the materialized head and insert TurnScoped blocks
at the start of wire history, with an explicit breakpoint on the last block
when the four-breakpoint budget permits. The rolling history marker follows
later blocks, allowing subsequent rounds in the *same turn* to reuse the
context. Stable and agent-scoped blocks remain in system; the runtime
conversation remains unchanged. A changed TurnScoped context on the next turn
can invalidate cached history, so real provider usage and net cost across
turns must be checked rather than assuming a global saving.

Diagnostics expose `turn_scoped_context_prefix_blocks`; per-request cache
read/create tokens remain in usage records.
