# First-Class Ollama Provider

## Status

Accepted for phased implementation in GitHub issue #2676.

## Decision

Holon will expose Ollama as a built-in `ollama@default` provider without
coupling agent turns to Ollama's native `/api/chat` protocol.

Phase 1 uses the existing Anthropic Messages transport:

- inference base URL: `http://127.0.0.1:11434`
- messages endpoint: `/v1/messages`
- authentication: none
- catalog policy: discovery only

A same-model bake-off on August 26, 2026 covered non-streaming, streaming,
thinking, a tool-call round trip, and three-turn full history. Anthropic
Messages, OpenAI Responses, and OpenAI Chat Completions each passed all five
cases. Anthropic Messages remains the default because its standard thinking
and tool-result content blocks match Holon's existing full-history agent loop.

The Anthropic transport must therefore honor the endpoint authentication
contract: configured credentials produce an authorization header, while an
explicit `CredentialKind::None` endpoint sends no authorization header.

## Discovery Contract

Ollama discovery is provider-specific control-plane behavior:

1. `GET /api/tags` lists installed models.
2. `POST /api/show` with `{"model":"<name>"}` enriches each model.
3. Results enter the normal remote-discovery cache as
   `BuiltInModelMetadata`.

Phase 1 maps only fields already represented by Holon's catalog:

- advertised context length to `context_window_tokens`
- `vision` to `image_input`
- `thinking` to `supports_reasoning`
- `tools` to the existing tool-call capability projection

Unknown or absent metadata remains conservative. Holon does not infer
capability from model names and does not automatically pull, run, stop, or
delete models.

The default endpoint is loopback-only. A user may explicitly configure a
remote endpoint; doctor should warn for a non-loopback, cleartext,
unauthenticated endpoint rather than silently elevating trust or rejecting an
intentional self-hosted configuration.

## Preserved Boundaries

- Ollama is not inserted into the default or fallback model chain.
- Existing custom OpenAI-compatible Ollama configurations remain valid.
- Provider discovery may be specialized without introducing a general
  arbitrary request-graph framework.
- Transport selection is explicit in endpoint configuration; runtime code does
  not switch transports implicitly per request or capability.

## Deferred

Later phases may add capability evidence improvements, Chat Completions
reasoning compatibility, structured-output capability, embeddings and rerank
classification, lifecycle management, warmup diagnostics, or a native Ollama
transport. Those changes are not part of the Phase 1 provider contract.
