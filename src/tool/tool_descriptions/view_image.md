Validate and record durable metadata for a local image file.

`ViewImage` accepts a workspace-relative or absolute image path plus a required
prompt describing what to inspect. It records provider-neutral evidence such as
media type, byte count, SHA-256, and dimensions when they can be read from the
file header.

`ViewImage` selects a configured primary or fallback model that advertises
`image_input` support and includes structured selection diagnostics in the
result. If no configured model supports image input, the tool returns a
`vision_adapter_unavailable` error with the evaluated candidates.

This skeleton does not yet call the selected vision model. Until provider-native
visual observation generation is implemented, successful results return the
image metadata, selected mode/provider/model, and an observation placeholder
explaining that visual observation generation is not implemented yet.
