# OpenAI Codex Transport Contract

Decision:

- support `openai-codex/*` through the required Responses streaming transport
  contract instead of the single-body JSON path

Reason:

- the Codex backend requires `stream=true`
- Holon must reconstruct terminal output from streamed events and classify
  explicit terminal failures rather than pretending the provider is available
