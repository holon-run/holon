# Arcee model catalog

Holon keeps only `trinity-mini` and `trinity-large-preview` in Arcee's
built-in picker. Arcee's hosted Models API and Models Overview listed those
models when the snapshot was verified on 2026-07-13. Both hosted model pages
publish a 128K context window. `trinity-large-thinking` is not retained because
the current hosted API does not list it.

The public documentation does not publish hosted output-token limits or
portable capability metadata for the two listed models, so Holon does not
infer output limits, reasoning, vision, or parallel tool-call support.

Arcee inference uses the documented OpenAI-compatible
`https://api.arcee.ai/v1` base URL. Model discovery uses the separate
`https://api.arcee.ai/api/v1/models` endpoint, requires the configured
`ARCEE_API_KEY`, and conservatively maps only model identifiers. Known hosted
models retain their documented 128K context window; newly discovered model
identifiers remain otherwise unspecified.

Sources:

- Arcee Models API:
  `https://docs.arcee.ai/api-reference/models`
- Arcee Models Overview:
  `https://docs.arcee.ai/get-started/models-overview`
- Trinity Mini:
  `https://docs.arcee.ai/models/trinity-mini`
- Trinity Large Preview:
  `https://docs.arcee.ai/models/trinity-large-preview`
