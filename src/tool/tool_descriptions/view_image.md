Validate and record durable metadata for a local image file.

`ViewImage` accepts a workspace-relative or absolute image path plus a required
prompt describing what to inspect. It records provider-neutral evidence such as
media type, byte count, SHA-256, and dimensions when they can be read from the
file header.

`ViewImage` selects a configured primary or fallback model that advertises
`image_input` support and includes structured selection diagnostics in the
result. If no configured model supports image input, the tool returns a
`vision_adapter_unavailable` error with the evaluated candidates.

When the selected vision model is OpenAI-compatible, `ViewImage` sends the image
and prompt to that model and returns a generated `visual_observation` alongside
the durable image metadata and selection diagnostics. If the selected model or
provider cannot be used for image observation, the tool returns
`vision_adapter_unavailable` with structured diagnostics.
