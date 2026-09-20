---
title: Inspect an image with a vision tool
summary: Point a vision model at a local image and read back what it sees.
order: 21
---

# Inspect an image with a vision tool

`ViewImage` sends a local image to a vision model and returns an observation. Use
it when a task needs something read from a screenshot, a diagram, or a photo.

## Before you start

- A vision model is configured, or an image-capable model is available. If none
  is, the tool returns `vision_adapter_unavailable`.

## Steps

1. Ask about a specific file, and say what to look for:

   ```
   Open docs/screenshots/error.png and tell me which field is highlighted.
   ```

2. The agent calls `ViewImage` with the file path and a prompt. You get back a
   visual observation plus metadata such as media type, byte size, and
   dimensions.

3. If you ask about the same image and prompt again, the runtime reuses the
   cached observation instead of calling the model again.

## Confirm it worked

The observation answers the question you asked, and the metadata confirms the
tool read the file you meant.

## Limits

Supported formats, model selection, and the full response shape are in the
[Image observation reference](/reference/view-image.md).
