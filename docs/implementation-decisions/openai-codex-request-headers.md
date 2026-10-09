# OpenAI Codex request header alignment

Date: 2026-10-09

## Choice

The `openai-codex` transport sends the same client-identity and negotiation
headers as the official Codex CLI (verified against `rust-v0.160.0`):

- `User-Agent` in the CLI's `{originator}/{version} ({os}; {arch}) {surface}` shape
- `accept: text/event-stream` on streaming `/responses` requests
- `session-id` / `thread-id` / `x-client-request-id` correlation headers on
  conversation traffic
- the legacy `OpenAI-Beta: responses=experimental` header is no longer sent,
  because current CLI releases do not send it on the HTTP path

## Reason

Holon's Codex transport reuses Codex CLI OAuth credentials and sends
`originator: codex_cli_rs`, but its header fingerprint matched no real Codex
CLI release (previously no `User-Agent` at all). Under backend capacity
pressure the edge layer can treat such unmatched fingerprints differently,
which is consistent with the intermittent connection resets observed before
this change.

## Preserved boundary

- `User-Agent` reports the Codex wire version the transport targets
  (`CODEX_CLI_WIRE_VERSION`) with `holon/<version>` as the client surface
  token: aligned in shape, honest about the implementation.
- Holon does not fabricate headers that require real first-party client
  capabilities (attestation, `x-codex-turn-state`, routing hints, request
  compression). Those stay out until the transport implements them.
- `session-id` maps to the Holon agent id and `thread-id` /
  `x-client-request-id` to the provider continuation scope, so correlation
  values remain Holon-owned identities rather than fabricated UUIDs.
