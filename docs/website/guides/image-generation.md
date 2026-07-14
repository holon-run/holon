---
title: Image Generation guide
summary: Agent tool for generating images from text prompts — model selection, size and format options, output management, and caching.
order: 47
---

# Image Generation Guide

`GenerateImage` lets Holon agents create images from text prompts using a
configured image-generation model. The runtime saves generated images to
`agent_home/media/generated` and returns durable workspace URIs.

## What GenerateImage does

When an agent calls GenerateImage, the runtime:

1. **Validates the prompt and parameters** — ensures the prompt is non-empty
   and any optional parameters use supported values.
2. **Routes to an image-generation model** — uses the configured
   `image_generation.default` provider/model, or auto-discovers the first
   configured turn model that supports `image_generation`.
3. **Saves the image** — writes the generated image bytes under
   `agent_home/media/generated` with a unique filename.
4. **Records durable metadata** — computes SHA-256, detects dimensions and
   MIME type, and returns a `workspace://` URI.

## Parameters

| Parameter | Required | Description |
|-----------|----------|-------------|
| `prompt` | yes | Detailed image-generation prompt |
| `size` | no | One of `1024x1024`, `1536x1024`, or `1024x1536` |
| `background` | no | One of `auto`, `transparent`, or `opaque` |
| `output_format` | no | One of `png`, `jpeg`, or `webp` |
| `name` | no | Filename stem for the saved image |

## Supported sizes

| Size | Ratio | Typical use |
|------|-------|-------------|
| `1024x1024` | 1:1 | Square images, icons, social media posts |
| `1536x1024` | 3:2 | Landscape, banners, hero images |
| `1024x1536` | 2:3 | Portrait, posters, mobile screens |

## What GenerateImage returns

Each generated image includes durable metadata:

| Field | Description |
|-------|-------------|
| `id` | Stable reference ID (e.g., `img_abc123`) |
| `uri` | `workspace://` URI for use in markdown and agent messages |
| `mime` | Media type (e.g., `image/png`) |
| `byte_count` | File size in bytes |
| `sha256` | Content hash |
| `size` | Image dimensions (width × height) when detectable |
| `created_at` | Generation timestamp |

The result also includes provider/model provenance and the original prompt.

## Image routing

### Explicit configuration

Set a dedicated image-generation model:

```bash
holon config set image_generation.default "openai/dall-e-3"
```

When `image_generation.default` is configured, GenerateImage uses that model
for all image generation requests. Use a model route ref that includes the
endpoint for unambiguous routing:

```bash
holon config set image_generation.default "volcengine@image-openai/seedream-4.0"
```

### Auto-discovery

If `image_generation.default` is not set, the runtime selects the first
configured turn model that advertises `image_generation` capability. This
lets agents generate images without dedicated image-generation configuration
when the active conversation model supports it.

### Fallback provider

When a model does not natively support image generation, `GenerateImage`
falls back to the first configured image-generation enabled model. This is
transparent to the agent.

## Supported providers

Image generation is supported through providers whose models advertise the
`image_generation` capability:

| Provider | Model | Notes |
|----------|-------|-------|
| OpenAI | dall-e-3, dall-e-2 | Native image generation |
| Volcengine | seedream-4.0 | Via Volcengine Ark plan endpoint |

> See [Models reference](/reference/models.md) for the current list of
> image-generation capable models.

## Volcengine Seedream setup

To use Volcengine's Seedream model for image generation, configure a
dedicated plan endpoint:

```bash
holon config set providers.volcengine.endpoints.image-openai.transport openai_chat_completions
holon config set providers.volcengine.endpoints.image-openai.base_url "https://ark.cn-beijing.volces.com/api/plan/v3"
holon config set providers.volcengine.plans.image-openai.endpoint image-openai
```

Then set the image generation default:

```bash
holon config set image_generation.default "volcengine@image-openai/seedream-4.0"
```

## Output management

Generated images are saved under `agent_home/media/generated/` with
timestamped filenames. When a `name` is provided, the filename uses that
stem; otherwise it defaults to `generated_<timestamp>`.

The returned `workspace://` URI can be used in agent markdown output for
in-conversation rendering. Images are accessible through the Web GUI file
browser and workspace file API.

## When agents use GenerateImage

Agents call GenerateImage when a task involves visual creation:

- **Data visualization** — generate charts, diagrams, or infographics from
  data descriptions.
- **UI mockups** — create visual representations of interface concepts.
- **Logo and icon design** — generate branding assets or icons.
- **Illustration** — create visual aids for documentation or presentations.

The tool is part of the `LocalEnvironment` capability family and is available
to every agent by default.

## CLI verification

GenerateImage is a model-facing tool; it is not directly callable from the
CLI. Agents use it through the normal tool-calling mechanism. The image
generation model routing can be inspected with:

```bash
holon config get image_generation.default
```

## See also

- [Model tool schema inventory](/reference/model-tool-schema-inventory.md) — tool registration and stability
- [Models reference](/reference/models.md) — supported providers and image generation availability
- [Configuration reference](/reference/configuration.md) — `image_generation.default` and provider setup
- [Web GUI guide](/guides/web-gui.md) — image generation settings in the browser
