---
title: Inspect an image with a vision tool
summary: Point a vision model at a local image and read back what it sees.
order: 230
---

# Inspect an image with a vision tool

`ViewImage` sends a local image to a vision model and returns an observation. Use
it when a task needs something read from a screenshot, a diagram, or a photo.

## Before you start

- The image exists on the runtime host and the agent can read it. A path on
  your phone or laptop is not automatically available to a remote runtime.
- The agent can use `ViewImage`.
- A vision route is configured, or an authenticated image-capable model can be
  discovered. Follow [Configure models](configure-models.md).
- You are authorized to send this image to the selected model provider. Check
  screenshots for credentials or private information first.

## Steps

1. Ask about a specific file, and say what to look for:

   ```
   Open docs/screenshots/error.png and tell me which field is highlighted.
   ```

2. The agent calls `ViewImage` with the file path and a prompt. You get back a
   visual observation plus metadata such as media type, byte size, and
   dimensions.

3. Read the observation against your question. The runtime can reuse a cached
   observation for the same image content and prompt; a repeated request does
   not necessarily make another provider call.

## Confirm it worked

Check the returned file metadata against the image you intended to inspect.
Confirm that a visual observation was returned: file metadata alone does not
mean a model examined the image. Treat uncertain readings, especially small
text, as observations to verify against the original.

## Troubleshooting

- **File missing or unreadable:** Check the path on the runtime host and the
  agent's active workspace. Use a confirmed absolute path if the relative path
  resolves to the wrong file.
- **`vision_adapter_unavailable`:** Read the selection diagnostics. Check
  `vision.default`, credentials, image-input capability, and transport support
  through [Configure models](configure-models.md).
- **Provider error:** Check the returned authentication, network, or format
  error before retrying.
- **Observation misses the detail:** Ask a narrower question or supply a
  readable crop. Do not infer text that the image does not show clearly.

## Limits

Supported formats, model selection, and the full response shape are in the
[Image observation reference](../reference/view-image.md).
