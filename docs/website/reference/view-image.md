---
title: Image observation
summary: Image observation tool inputs, vision model selection, response metadata, and compatibility limits.
order: 44
---

# Image observation

`ViewImage` lets Holon agents inspect local image files through a vision
model. The agent provides an image path and a prompt describing what to
inspect, and the runtime returns a structured visual observation.

## What ViewImage does

When an agent calls ViewImage, the runtime:

1. **Validates the image** — reads the file, checks size limits (max 20 MB,
   50 million pixels), computes SHA-256, detects MIME type and dimensions.
2. **Selects a vision model** — uses the configured `vision.default`
   provider/model, or auto-discovers an authenticated provider that supports
   image input.
3. **Generates an observation** — sends the image and prompt to the vision
   model and returns a structured observation.
4. **Caches the result** — subsequent calls with the same image and prompt
   reuse the cached observation.

## Parameters

| Parameter | Required | Description |
|-----------|----------|-------------|
| `path` | yes | Workspace-relative or absolute image path |
| `prompt` | yes | What to inspect or describe in the image |
| `model` | no | Explicit route ref (`provider@endpoint/model`) for this call |

Supported image formats: PNG, JPEG, GIF, WebP, and other formats the selected
vision model supports.

## What ViewImage returns

The result has two parts:

### Visual reference (durable metadata)

Recorded for every call regardless of vision model availability:

| Field | Description |
|-------|-------------|
| `id` | Stable reference ID |
| `mime` | Media type (e.g., `image/png`) |
| `byte_count` | File size in bytes |
| `sha256` | Content hash |
| `path` | Resolved file path |
| `size` | Image dimensions (width × height) when detectable |

### Visual observation (generated)

When a vision model is available, the observation includes:

| Field | Description |
|-------|-------------|
| `generated_by` | Provider, model, and generation mode |
| `prompt` | The prompt that produced this observation |
| `summary` | Human-readable summary of the observation |
| `ocr` | Extracted text (when applicable) |
| `elements` | Identified visual elements |
| `relations` | Spatial or logical relationships between elements |
| `issues` | Detected problems or anomalies |
| `uncertainties` | Areas where the model is unsure |

## Vision model selection

### Explicit configuration

Set a dedicated vision model:

```bash
holon config set vision.default "anthropic/claude-sonnet-4-6"
```

When `vision.default` is configured, ViewImage uses that model for all image
observations. This is the recommended setup for production use.

### Per-call model selection

Agents can specify `model` (e.g., `"anthropic@direct/claude-sonnet-4-6"`) to
route an individual inspection to a specific model. Explicit per-call selection
never falls back to alternative routes and does not change `vision.default`. If
the requested model cannot observe images, the call returns an explicit error
instead of silently selecting another candidate.

### Auto-discovery

If `vision.default` is not set, ViewImage auto-discovers an available vision
model by scanning configured providers for those that:

- Have valid credentials
- Advertise `image_input` support
- Use a transport that supports image observation generation
  (OpenAI-compatible APIs or Anthropic Messages)

The selection result is returned in the tool response so you can see which
model was chosen.

### When no vision model is available

If no configured model supports image input, ViewImage returns a
`vision_adapter_unavailable` error with the list of evaluated candidates. The
durable visual reference metadata is still recorded.

## Observation caching

ViewImage caches observations by a compound key of image hash + prompt. If the
agent calls ViewImage with the same image and prompt again, the runtime
returns the cached observation without making another model call:

```
ViewImage reused cached visual observation
```

This saves latency and cost during multi-turn sessions where the agent
revisits the same image.

## When agents use ViewImage

Agents call ViewImage when a task involves visual inspection:

- **Code review from screenshots** — inspect UI mockups, error screens, or
  diagrams.
- **Document analysis** — extract text or structure from scanned documents,
  receipts, or whiteboard photos.
- **Troubleshooting** — diagnose visual anomalies in generated output or test
  failures.
- **Data extraction** — pull structured data from charts, tables, or forms
  rendered as images.

The agent decides when ViewImage is relevant based on the task context. The
tool is part of the `LocalEnvironment` capability family and is available to
every agent by default.

## Aggregate image budgets and multimodal preflight

To prevent image-heavy sessions from exhausting provider context limits, Holon
enforces request-level Markdown attachment bounds and multimodal preflight:

- **Attachment limits** — Up to **8 unique images** or **20 MiB** of pre-base64
  file bytes are admitted per request. Per-file limits (20 MB / 50 MP) continue
  to apply.
- **Priority and content deduplication** — Required images reserve capacity
  before optional historical attachments. Identical files under different paths
  are deduplicated by content hash, evaluating at most 16 distinct paths per
  turn. Optional historical images that exceed the budget degrade gracefully
  with text notices.
- **Multimodal context estimation** — Transport preflight counts text and tool
  payloads separately from images, probing dimensions from at most 64 KiB of
  decoded header bytes. Approximate patch/tile heuristics estimate context
  footprint without decoding pixel buffers or fetching remote URLs, verifying
  upstream admission before dispatch.

## CLI verification

ViewImage is a model-facing tool; it is not directly callable from the CLI.
Agents use it through the normal tool-calling mechanism.

## See also

- [Model tool schema inventory](/reference/model-tool-schema-inventory.md) — tool registration and stability
- [Models reference](/reference/models.md) — supported providers and vision model availability
- [Configuration reference](/reference/configuration.md) — `vision.default` and provider setup
