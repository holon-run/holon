---
title: Generate an image
summary: Write a prompt, generate an image, and find the result on disk.
order: 220
---

# Generate an image

Agents can create images from a text prompt with the `GenerateImage` tool. You
describe what you want; the agent calls the tool and saves the result.

## Before you start

- The agent can use `GenerateImage`.
- An image-generation route and its credentials are configured. Follow
  [Configure models](configure-models.md); a conversation model alone may not
  support image generation.
- The runtime can reach the provider and write to the agent's media directory.

## Steps

1. Ask for the image and include the details that matter:

   ```
   Generate a 1024x1024 product shot of a matte black desk lamp on a white
   background. Save it as desk-lamp.png.
   ```

   The agent calls `GenerateImage` with your prompt and supported output
   options. Each call generates one image; request separate calls for distinct
   images.

2. Ask the agent to report the returned file reference. Generated images are
   written under `agent_home/media/generated`; the requested name is not a
   substitute for the actual saved path.

3. Open the image from the response or the client's file browser. If you need
   the agent to check its content, follow
   [Inspect an image](inspect-image.md).

## Confirm it worked

Confirm that the tool succeeded and returned an image reference and metadata,
then open the saved artifact. Check the image against your prompt; a successful
provider call does not guarantee that every requested detail appears.

## Troubleshooting

- **No generation model:** Check the image-generation route and provider
  credentials in [Configure models](configure-models.md).
- **Unsupported option:** Use a size, format, and background supported by the
  configured model. The reference lists the tool's accepted values.
- **Provider or network error:** Read the returned error before retrying. Do
  not report an image as generated without a successful tool result.
- **Cannot open the result:** Check the returned path on the runtime host.
  A remote host's filesystem path does not identify a file on your laptop;
  use the client's artifact viewer or download action.

## Sizes and formats

Size, background, and format options, plus supported models, are in the
[Image generation reference](../reference/image-generation.md).
