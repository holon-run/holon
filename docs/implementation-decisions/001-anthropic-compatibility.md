# Anthropic Compatibility

Decision:

- use `ANTHROPIC_AUTH_TOKEN` with `Authorization: Bearer ...`
- use `ANTHROPIC_BASE_URL` as the runtime endpoint
- always send `anthropic-version: 2023-06-01`

Reason:

- the local environment already provides these values in `~/.claude/settings.json`
- the configured endpoint may be an Anthropic-compatible proxy rather than the
  official Anthropic host
- this keeps live integration tests aligned with the real local setup
