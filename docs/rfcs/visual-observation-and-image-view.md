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

### Schema Positioning

`visual_observation.v1` is Holon's normalized visual observation format. It is
not a direct implementation of a single external annotation standard.

The schema intentionally aligns with established conventions where they are
useful:

- W3C Web Annotation style region selectors for image regions
- ALTO, hOCR, and PAGE-style OCR/layout records for text and reading evidence
- COCO-style object annotations for detected elements and bounding boxes
- platform accessibility and UI automation tree concepts for roles and element
  semantics

Holon should not store any of those external formats as its canonical durable
history. They are input/source formats that can be mapped into the normalized
observation shape. This keeps the model-visible output compact while preserving
interoperability with OCR, CV, UI automation, and external adapter tools.

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

`prompt` is required in the model-facing schema. It carries the agent's
purpose, focus, and visual question as free text instead of separate fields such
as `purpose`, `focus`, or `observation_policy`. The prompt is part of the
observation cache key, so a generic prompt should be used only when the caller
really wants a broad visual inventory.

CLI or SDK helper surfaces may provide a default prompt for human convenience,
but the model-facing tool contract should require an explicit prompt.

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
charts, and photos, while keeping a stable core. The first implementation
should use one broad `visual_observation.v1` schema with optional sections
rather than separate purpose-specific schemas such as `ui_layout.v1`,
`document_ocr.v1`, or `chart_data.v1`.

The stable core is:

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

Coordinates are image-pixel coordinates in the visual reference's coordinate
space, with origin at the top-left corner.

### Visual Observation Field Semantics

The top-level required fields are:

- `type`: fixed string, `visual_observation`
- `schema`: fixed schema version for the first implementation,
  `visual_observation.v1`
- `visual_reference_id`: durable reference to the source image
- `prompt`: the visual question or focus used to generate this observation
- `generated_by`: provenance for the observation generator
- `summary`: concise visible-evidence summary
- `uncertainties`: explicit caveats, missing evidence, and reliability concerns

The top-level optional fields should default to empty arrays when omitted:

- `ocr`
- `elements`
- `relations`
- `issues`
- `external_sources`

`generated_by` records observation provenance:

```json
{
  "provider": "openai",
  "model": "gpt-5.4-mini",
  "mode": "vision_adapter"
}
```

`mode` may be:

- `vision_adapter`: a configured multimodal provider generated observation for
  a primary model that may not support images
- `primary_native`: the primary multimodal model/provider generated the
  observation
- `external_adapter`: an external OCR, UI automation, browser, Appium, or MCP
  source generated the observation

`summary` should be short and evidence-oriented. It should describe what is
visible, not recommend implementation changes or take over the primary model's
planning.

`ocr` contains text evidence:

```json
{
  "text": "Sign in",
  "bbox": {
    "x": 96,
    "y": 428,
    "width": 180,
    "height": 52
  },
  "confidence": 0.94,
  "source": "vision_adapter"
}
```

`text`, `bbox`, and `confidence` are required for OCR entries. `source` is
optional and may name the adapter or imported source format.

`elements` contains semantic visual objects:

```json
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
  "confidence": 0.86,
  "source": "vision_adapter"
}
```

`id`, `role`, `bbox`, and `confidence` are required for element entries. `text`,
`visual_style`, and `source` are optional.

`role` should be a compact semantic label such as `button`, `input`, `text`,
`heading`, `image`, `icon`, `table`, `chart`, `legend`, `axis`, `cell`,
`container`, or `unknown`. The vocabulary should stay open-ended in v1 because
source adapters and visual domains differ.

`visual_style` captures visible style evidence such as colors, font size,
weight, border color, opacity, or contrast. These values may come from pixel
sampling, a vision model, a DOM/computed-style source, or an accessibility/UI
adapter. Source and confidence should be included when the style is uncertain.

`relations` describes spatial or semantic relationships among elements:

```json
{
  "type": "below",
  "a": "element_1",
  "b": "element_0",
  "confidence": 0.82
}
```

`type`, `a`, `b`, and `confidence` are required. Common relation types include
`above`, `below`, `left_of`, `right_of`, `contains`, `inside`, `overlaps`,
`aligned_left`, `aligned_center`, `aligned_right`, `same_row`, `same_column`,
and `near`.

`issues` contains candidate visual problems discovered during observation:

```json
{
  "type": "possible_overlap",
  "description": "The primary button appears to overlap the sticky footer.",
  "bbox": {
    "x": 860,
    "y": 720,
    "width": 420,
    "height": 88
  },
  "related_elements": ["element_12", "element_19"],
  "confidence": 0.78
}
```

Issues are diagnostic hints, not final conclusions. They are most useful for
bug-finding, review, UI regression, accessibility, chart/document quality, and
visual debugging tasks. If the prompt asks for a broad image description,
`issues` may be empty. If the prompt asks to inspect layout or rendering
problems, the adapter should populate issue candidates when it sees visible
evidence.

Common issue types include `possible_overlap`, `possible_clipping`,
`low_contrast`, `unreadable_text`, `misalignment`, `missing_expected_element`,
`ambiguous_state`, `blurred_or_low_quality`, and `possible_loading_state`.

`uncertainties` is required and may be empty. It should record what the
observation could not establish reliably, such as small text that needs a crop,
ambiguous element identity, low confidence OCR, or missing external UI
structure.

`external_sources` records imported source formats or adapters that contributed
to the observation:

```json
{
  "kind": "browser_accessibility_tree",
  "source_id": "mcp_browser_...",
  "captured_at": "2026-05-25T00:00:00Z"
}
```

Examples include `alto`, `hocr`, `page_xml`, `coco`, `browser_dom`,
`browser_accessibility_tree`, `android_uiautomator`, `appium_page_source`,
`macos_accessibility`, and `windows_uia`.

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
- include the native image payload for the immediate continuation after the
  `ViewImage` tool call
- omit the native image on later replay when the cached observation is
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

The default native image replay policy is:

```text
native_image_replay = immediate_only
```

This lets multimodal primary models inspect the original visual evidence when
the agent explicitly calls `ViewImage`, while keeping long-term replay,
compaction, and model switching observation-first.

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

Provider response validation must keep the observation identity fields
(`type`, `schema`, and non-empty `summary`) strict. Auxiliary observation fields
are best-effort sensor data and must not invalidate an otherwise usable
observation solely because a provider ignored its strict response schema.
Holon normalizes arrays directly, wraps single objects, converts meaningful
strings into text-bearing entries, and ignores null, boolean, numeric, or
otherwise unusable auxiliary values. `uncertainties` similarly accepts a
string, a text-bearing object, or an array containing either form.

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

## Limits

The first implementation should use conservative hard limits. Image input can
inflate substantially when converted to provider-native payloads such as data
URLs, where base64 adds roughly one third to the encoded byte size. Provider
request envelopes, tracing, retries, and in-memory copies add additional
overhead.

Initial limits:

```text
max input file size: 8 MB
max decoded pixels: 16 MP
max encoded image payload before base64: 4 MB
max long edge for adapter/native resized payload: 2048 px
max observation JSON: 6k tokens equivalent
max OCR entries: 200
max elements: 200
max issues: 50
```

The runtime should distinguish input file size from provider payload size. A
large PNG may compress well after resizing, while a smaller file may still
produce an oversized model payload after re-encoding and base64 expansion.

If the original image exceeds the input file limit, `ViewImage` should fail
before reading the full file into memory. If the decoded image or encoded model
payload exceeds limits, Holon should downscale to the configured long edge and
try again. If the resized payload still exceeds the provider payload limit,
`ViewImage` should return a recoverable error that asks the agent to provide a
smaller image or a focused crop in a future tool version.

The first implementation should not expose native original-resolution image
mode. Original-resolution forwarding can be reconsidered after Holon has
provider-specific payload accounting, crop support, and clear cost controls.

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

- Should a future version add crop/focus coordinates to the public `ViewImage`
  input, or should focused inspection remain prompt-only until external
  adapters can provide richer references?
- Should provider selection prefer the current primary model whenever it
  supports image input, or should Holon prefer a cheaper configured vision model
  for structured observation generation while still sending the native image to
  the primary model for immediate continuation?
