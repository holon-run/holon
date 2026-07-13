# OpenRouter model catalog

Holon keeps only OpenRouter's `openrouter/auto` router as a built-in picker
default. The snapshot was verified against the official Models API on
2026-07-13. The former Hunter Alpha and Healer Alpha entries are no longer
returned by that API, while ordinary upstream model availability changes too
frequently to maintain as a second static OpenRouter catalog.

The Auto Router exposes a 2,000,000-token context window and accepts image
input and the `reasoning` parameter. Its selected upstream model and provider
remain dynamic, so Holon leaves the output limit and adjustable reasoning
effort list unset rather than projecting one routed model's limits onto the
aggregate endpoint.

OpenRouter discovery maps the Models API's `context_length`,
`top_provider.max_completion_tokens`, `architecture.input_modalities`,
`supported_parameters`, and `reasoning` object into remote route metadata.
Reasoning is advertised when the endpoint accepts the `reasoning` parameter or
the reasoning object marks it enabled or mandatory. Adjustable effort values
are exposed only when `reasoning_effort` is accepted and the API supplies an
explicit `supported_efforts` list.

Sources:

- OpenRouter Models API:
  `https://openrouter.ai/api/v1/models`
- OpenRouter model routing:
  `https://openrouter.ai/docs/features/model-routing`
- OpenRouter reasoning tokens:
  `https://openrouter.ai/docs/use-cases/reasoning-tokens`
- OpenRouter multimodal overview:
  `https://openrouter.ai/docs/features/multimodal/overview`
