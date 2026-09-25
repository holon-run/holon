# Anthropic Turn-Scoped Cache Prefix (#3225)

## Contract

Anthropic-compatible transport strips any materialized prompt context head
before constructing wire messages. Stable and agent-scoped context remains in
system blocks; turn-scoped context is inserted once, at the start of wire
conversation history, before user/assistant/tool continuation. The last
turn-scoped block receives an explicit cache breakpoint only if the system
breakpoints and a reserved rolling history breakpoint leave room within the
provider's four-breakpoint budget. When cache control is disabled, the context
still appears once, without an explicit marker.

All provider rounds in one turn must have identical wire content through this
context boundary, even as later history and its rolling marker advance. The
next turn must use its own context, not a stale context from prior requests.
The runtime's replayable conversation remains unchanged.

## Trade-off and validation

A changed turn-scoped prefix can invalidate the cached history of *previous*
turns. The former tail layout avoided this but repeatedly charged the same
large uncached context on every provider round. Neither layout guarantees a
lower bill for all workloads. Validate the wire prefix in consecutive-round
tests and compare real per-round uncached input, cache creation/read, output,
and net cost across turn boundaries on endpoints that honor the requested
cache behavior. Endpoints with implicit-only caching are a distinct case;
explicit breakpoint assertions do not establish savings there.
