---
title: RFC: Visual Observation And Image View
date: 2026-05-25
status: draft
---

# RFC: Visual Observation And Image View

## Summary

Holon should introduce a built-in `ViewImage` tool that lets an agent inspect a
local image path while keeping conversation history portable across models.

`ViewImage` should expose one simple model-facing surface:

```json
{
  "path": "/absolute/or/workspace/relative/screenshot.png",
  "prompt": "Inspect the bottom toolbar for overlap and clipping."
}
```

The tool should always create or reuse a durable provider-neutral visual
reference and a structured visual observation. The current model may receive a
native image input when it supports images, but Holon's durable history must not
depend on provider-specific image payloads.

The first implementation should not expose a configurable history policy. The
portable structured observation is the default and only durable policy.

## Problem

Holon already tracks model capabilities such as `image_input`, but the runtime
does not yet have a provider-neutral way to represent images in history.

If Holon simply copies an OpenAI-style `input_image` payload into conversation
history, several runtime guarantees break:

- switching from a multimodal model to a text-only model cannot replay that
  history safely
- context compaction cannot preserve visual evidence without replaying the
  original image
- child agents and fallback providers may lose the basis for prior decisions
- large base64 image payloads can bloat durable ledgers and provider windows
- repeated image replay is costly and can produce inconsistent attention across
  turns

On the other hand, a plain OCR or caption fallback is too weak for many agent
tasks. UI screenshots, charts, diagrams, and product images often require
locations, relationships, colors, and uncertainty, not just prose.

Holon needs a stable visual observation model that works as both:

- a high-fidelity bridge to multimodal providers
- a portable visual memory for text-only providers

## Goals

- expose one simple `ViewImage` tool to ordinary agents
- always produce a durable provider-neutral visual reference
- always produce a portable structured observation by default
- keep provider-native image payloads as request-time lowering, not history
- allow text-only primary models to use a configured multimodal provider as a
  vision adapter
- let multimodal primary models benefit from cached observations instead of
  repeatedly reprocessing images
- make model switching, compaction, replay, and child-agent handoff safe
- keep DOM, accessibility tree, Appium, Playwright, and platform UI capture out
  of Holon core

## Non-goals

- do not make Holon core a browser, Android, macOS, Windows, or Appium
  automation framework
- do not expose separate `ViewImage` and `ObserveImage` tools to ordinary
  agents in the first implementation
- do not preserve base64 image bytes as the canonical durable history format
- do not require the agent to choose a history policy
- do not require users to configure a dedicated vision model in the first
  implementation
- do not promise that structured observation is a lossless replacement for the
  original image

## Design

### One Public Tool

Holon should expose one built-in tool:

```text
ViewImage
```

Input:

```json
{
  "path": "string",
  "prompt": "string"
}
```

`path` is required. It may be absolute or relative to the active workspace root,
subject to the same filesystem trust and workspace boundaries as other local
environment tools.

`prompt` is optional in the JSON schema but should be encouraged in tool
guidance. It carries the agent's purpose, focus, and visual question as free
text instead of separate fields such as `purpose`, `focus`, or
`observation_policy`.

Examples:

```json
{
  "path": "screenshots/login-mobile.png",
  "prompt": "Inspect whether the login button is visible and whether any text is clipped."
}
```

```json
{
  "path": "/tmp/chart.png",
  "prompt": "Extract the visible trend, labels, legend, and any uncertainty in the plotted values."
}
```

### Internal Stages

Although the public surface is one tool, the runtime should implement distinct
internal stages:

1. Resolve and validate the image path.
2. Compute image identity and metadata.
3. Create or reuse a `visual_reference`.
4. Create or reuse a `visual_observation`.
5. Lower the result for the current model and provider.
6. Persist the reference, observation metadata, and tool execution record.

The model should not need to decide whether to "observe" the image. Observation
is part of the tool contract.

### Visual Reference

A visual reference is the durable identity of an image:

```json
{
  "type": "visual_reference",
  "id": "vis_01j...",
  "path": "/workspace/screenshots/login-mobile.png",
  "sha256": "hex...",
  "mime": "image/png",
  "size": {
    "width": 1170,
    "height": 2532
  },
  "created_at": "2026-05-25T00:00:00Z"
}
```

Holon should persist path, hash, mime type, dimensions, and timestamps. It
should not persist provider-native data URLs as the canonical image history.

### Visual Observation

A visual observation is the portable structured result associated with a visual
reference and prompt:

```json
{
  "type": "visual_observation",
  "schema": "visual_observation.v1",
  "visual_reference_id": "vis_01j...",
  "prompt": "Inspect whether the login button is visible and whether any text is clipped.",
  "generated_by": {
    "provider": "openai",
    "model": "gpt-5.4-mini",
    "mode": "vision_adapter"
  },
  "summary": "The mobile login screen shows a centered form. The primary button is visible near the lower third.",
  "ocr": [
    {
      "text": "Sign in",
      "bbox": {
        "x": 96,
        "y": 428,
        "width": 180,
        "height": 52
      },
      "confidence": 0.94
    }
  ],
  "elements": [
    {
      "id": "element_1",
      "role": "button",
      "text": "Continue",
      "bbox": {
        "x": 88,
        "y": 1568,
        "width": 994,
        "height": 104
      },
      "visual_style": {
        "background_color": "#1f6feb",
        "text_color": "#ffffff"
      },
      "confidence": 0.86
    }
  ],
  "relations": [
    {
      "type": "below",
      "a": "element_1",
      "b": "element_0",
      "confidence": 0.82
    }
  ],
  "issues": [
    {
      "type": "possible_clipping",
      "description": "The helper text at the bottom is close to the viewport edge.",
      "bbox": {
        "x": 80,
        "y": 2380,
        "width": 1010,
        "height": 70
      },
      "confidence": 0.62
    }
  ],
  "uncertainties": [
    "Small footer text may require a crop for reliable OCR."
  ]
}
```

The schema should be permissive enough for screenshots, documents, diagrams,
charts, and photos, while keeping a stable core:

- image size and reference
- prompt used for observation
- summary
- OCR spans with bounding boxes
- detected elements with roles, text, bounding boxes, and visual style
- relations among elements
- issue candidates
- uncertainties and limitations

Bounding boxes should use object form instead of positional arrays:

```json
{
  "x": 88,
  "y": 1568,
  "width": 994,
  "height": 104
}
```

This avoids ambiguity between `[x, y, width, height]` and `[x1, y1, x2, y2]`.

### Provider Selection

Holon should automatically discover a vision-capable execution path from the
currently configured provider/model catalog.

Resolution order:

1. If the current primary model supports image input, it is eligible for native
   image lowering.
2. If any configured provider/model supports image input, choose one as a vision
   adapter.
3. If no vision-capable provider is configured, return a clear tool error.

The automatic choice must be inspectable. The `ViewImage` result and tool
execution record should include:

```json
{
  "selected_mode": "native_image_with_observation",
  "vision_provider": "openai",
  "vision_model": "gpt-5.4-mini",
  "selection_reason": "current_primary_model_supports_image_input"
}
```

or:

```json
{
  "selected_mode": "vision_adapter",
  "primary_provider": "deepseek",
  "primary_model": "deepseek-chat",
  "vision_provider": "openai",
  "vision_model": "gpt-5.4-mini",
  "selection_reason": "primary_model_lacks_image_input"
}
```

The first implementation should not require a dedicated `[vision]` config block.
It may use deterministic built-in ordering over configured providers and model
metadata. A later RFC may add override policy if real deployments need it.

### Lowering Policy

Holon's durable history should always be:

```text
visual_reference + visual_observation
```

Provider requests are derived from that durable form.

For a multimodal current model:

- include the structured observation
- include the native image payload when the current step needs direct visual
  inspection
- omit the native image on ordinary replay when the cached observation is
  sufficient

For a text-only current model:

- include the structured observation as ordinary text or structured tool output
- do not include provider-native image payloads

For model switching:

- abandon provider-specific incremental image windows
- rebuild the conversation from Holon's durable transcript
- lower each visual reference according to the new model's capabilities
- if an old visual reference lacks an observation, generate one before replaying
  to a text-only model or return a recoverable error

### Tool Result Shape

The canonical `ViewImage` result should be structured:

```json
{
  "visual_reference": {},
  "observation": {},
  "selected_mode": "native_image_with_observation | vision_adapter",
  "summary_text": "Viewed image screenshots/login-mobile.png and generated visual observation."
}
```

The model-visible rendering may differ by provider:

- OpenAI Responses with native image support may use structured
  `function_call_output.output` content items containing `input_image`.
- Text-only providers should receive a concise textual rendering of the
  `visual_observation`.
- Provider-neutral logs should store the canonical result, not the native image
  payload.

This builds on `tool-result-envelope.md`: canonical runtime result and
model-visible rendering are separate surfaces.

### Prompting The Vision Adapter

When Holon uses a configured multimodal provider as a vision adapter, the
adapter should receive only the image and minimal task-specific prompt. It
should not receive the full agent conversation, repository context, memory, or
reasoning history.

The adapter prompt should instruct the model to:

- describe visible evidence only
- return JSON matching `visual_observation.v1`
- include bounding boxes when location matters
- include uncertainty when evidence is ambiguous
- avoid implementation recommendations
- avoid taking over the main agent's planning

The primary model remains the agent brain. The vision adapter is a sensor.

### External UI Structure

Holon core should not collect DOM, Android hierarchy, macOS accessibility tree,
Windows UI Automation tree, or Appium state directly.

External adapters may attach additional observation sources:

```json
{
  "external_sources": [
    {
      "kind": "browser_accessibility_tree",
      "source_id": "mcp_browser_...",
      "captured_at": "2026-05-25T00:00:00Z"
    }
  ]
}
```

`ViewImage` may consume these sources when available, but platform-specific
capture belongs in adapters, MCP servers, WebMCP bridges, or test frameworks.

## Caching

Holon should cache observations by:

- image sha256
- prompt text
- observation schema version
- adapter provider and model
- optional external source hashes

If the same image and prompt are observed again, Holon should reuse the cached
observation unless explicitly forced by a future administrative API.

Caching should be conservative. If the prompt asks a materially different visual
question, Holon should generate a new observation even when the image hash is
unchanged.

## Failure Modes

`ViewImage` should fail clearly when:

- the path is missing or outside the allowed workspace boundary
- the file is not a supported image
- image dimensions or file size exceed configured safety limits
- no configured provider/model can produce an observation
- the vision adapter returns invalid observation JSON

Failure should be recoverable where possible. The tool should explain what is
missing, for example:

```json
{
  "kind": "vision_adapter_unavailable",
  "message": "ViewImage requires a model with image input support, but no configured provider/model advertises image_input.",
  "hint": "configure an image-capable provider/model or switch the primary model to one that supports image input"
}
```

## Implementation Plan

1. Add provider-neutral visual reference and visual observation types.
2. Extend tool result/model-visible rendering so `ViewImage` can return both
   canonical structured results and provider-native image content items.
3. Add the `ViewImage` tool with `path` and `prompt`.
4. Add automatic vision-capable provider selection from the resolved model
   catalog.
5. Implement OpenAI Responses native image lowering and vision adapter calls.
6. Add text-only lowering that renders `visual_observation.v1` compactly.
7. Store visual reference and observation metadata in durable history/tool
   execution records without storing data URLs as canonical history.
8. Add model-switch replay tests from multimodal to text-only and text-only to
   multimodal.

## Open Questions

- Should `prompt` be required in the first schema, or optional with a generic
  default observation prompt?
- Should the first observation schema specialize by image purpose, or keep one
  broad `visual_observation.v1` with optional sections?
- Should Holon allow a future admin-only `ObserveImageReference` command for
  regenerating observations with a different schema?
- What maximum image size and observation token limits should be enforced in the
  first implementation?
- Should native multimodal replay include the original image by default for the
  immediately following model round, then use observation-only replay afterward?

