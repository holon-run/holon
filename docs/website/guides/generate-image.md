---
title: Generate an image
summary: Write a prompt, generate an image, and find the result on disk.
order: 20
---

# Generate an image

Agents can create images from a text prompt with the `GenerateImage` tool. You
describe what you want; the agent calls the tool and saves the result.

## Before you start

- Image generation is configured. If it is not, the tool reports that no model is
  available. See the [Image generation reference](/reference/image-generation.md).

## Steps

1. Ask for the image and include the details that matter:

   ```
   Generate a 1024x1024 product shot of a matte black desk lamp on a white
   background. Save it as desk-lamp.png.
   ```

   The agent turns this into a `GenerateImage` call with a prompt, a size
   (`1024x1024`, `1536x1024`, or `1024x1536`), a background, and an output
   format.

2. Let the agent report where the file landed. Generated images are written under
   the agent's media directory.

## Confirm it worked

The result names the saved file and its format and dimensions. If the tool
returned an error, the message says whether the model or the size was the
problem.

## Sizes and formats

Size, background, and format options, plus supported models, are in the
[Image generation reference](/reference/image-generation.md).
