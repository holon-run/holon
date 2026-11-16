Validate and record durable metadata for a local image file.

`ViewImage` accepts a workspace-relative or absolute image path plus a required
prompt describing what to inspect. It records provider-neutral evidence such as
media type, byte count, SHA-256, and dimensions when they can be read from the
file header.

Optional `model` selects a route for this call only. Prefer canonical
`provider@endpoint/model`; legacy `provider/model` is also accepted. Existing
endpoint, credential and observation-capability checks still apply. Explicit
selection never changes defaults or silently switches models, and invalid or
unavailable routes fail before inference, even when an observation is cached.

When `model` is omitted, `ViewImage` selects `vision.default` when configured.
Otherwise it auto-discovers an authenticated provider/model that advertises
`image_input` support and whose transport can generate visual observations.
Conversation `model.fallbacks` remain only a compatibility candidate source, not
the primary ViewImage selection mechanism. The result includes structured
selection diagnostics. If no configured model supports image input, the tool
returns a `vision_adapter_unavailable` error with the evaluated candidates.

When the selected vision model is served by a provider whose transport supports
image observation generation (OpenAI-compatible APIs, Anthropic Messages, or
Gemini GenerateContent),
`ViewImage` sends the image and prompt to that model and returns a generated
`visual_observation` alongside the durable image metadata and selection
diagnostics. If the selected model or provider cannot be used for image
observation, the tool returns
`vision_adapter_unavailable` with structured diagnostics. Results report the
requested and resolved route, selection source and contract version. Cached
observations are isolated by route and effective endpoint configuration; a
cache hit preserves the original `generated_by` attribution.
