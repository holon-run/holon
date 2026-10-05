# xAI Grok Imagine Image Generation

## Choice

`GenerateImage` supports xAI Grok Imagine image models through the existing
OpenAI-compatible `POST /images/generations` path, using an xAI request dialect
selected from the provider id (`xai`) or the `api.x.ai` base-url host.

The xAI dialect maps the tool's OpenAI-shaped `size` to the closest
`aspect_ratio` at `1k` (`1024x1024` -> `1:1`, `1536x1024` -> `3:2`,
`1024x1536` -> `2:3`), always requests `response_format: "b64_json"`, and
rejects OpenAI-only `background` and `output_format` instead of silently
dropping them. Response parsing honors the per-item `mime_type` when the
provider reports it.

The built-in catalog declares `xai/grok-imagine-image`,
`xai/grok-imagine-image-2.0`, and `xai/grok-imagine-image-quality` with only
`image_generation` enabled, so they are selectable through
`image_generation.default` but are never conversation turn candidates.

## Reason

xAI rejects OpenAI's `size` argument (`Argument not supported: size`) and
instead exposes `aspect_ratio` plus `resolution`, so the shared OpenAI request
builder could not be reused unchanged. xAI also returns `data[].b64_json` with
a `mime_type`, so requesting base64 avoids treating a temporary download URL as
a durable artifact and lets the saved file extension match the real bytes.

Unknown models default to `image_generation = false`, so the Imagine models
must be declared in the built-in catalog for explicit route selection to
resolve; they carry no turn or context metadata.

## Preserved boundary

OpenAI and Volcengine image generation keep their existing request shape and
behavior. The dialect is chosen inside the OpenAI-compatible transport from
provider identity only; no new global media abstraction or provider-specific
tool surface is introduced, and `size` mapping stays limited to the three sizes
the tool already exposes.
