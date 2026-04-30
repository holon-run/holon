# OpenAI Codex Transport Contract

Decision:

- support `openai-codex/*` through the required Responses streaming transport
  contract instead of the single-body JSON path

Reason:

- the Codex backend requires `stream=true`
- Holon must reconstruct terminal output from streamed events and classify
  explicit terminal failures rather than pretending the provider is available

Endpoint boundary:

- `openai-codex` treats `/backend-api/codex` as the ChatGPT-backed Responses
  API base
- legacy configs that still point at `/backend-api` are normalized before
  constructing `/responses` and `/responses/compact`
