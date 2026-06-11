Validate and record durable metadata for a local image file.

`ViewImage` accepts a workspace-relative or absolute image path plus a required
prompt describing what to inspect. It records provider-neutral evidence such as
media type, byte count, SHA-256, and dimensions when they can be read from the
file header.

This skeleton does not yet call a vision model. Until visual observation
generation is implemented, successful results return `status: "unavailable"`
with a structured explanation.
