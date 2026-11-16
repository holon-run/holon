Generate one image from a text prompt using a configured image-generation model.

The runtime saves the generated image under `agent_home/media/generated` and returns
an image reference plus metadata. Do not request multiple images in one call;
call this tool multiple times when distinct images are needed.

Inputs:
- `prompt` (required): detailed image-generation prompt.
- `model` (optional): one-call `provider@endpoint/model` route. Omit to use the
  configured default/automatic selection. An explicit route never falls back
  and does not modify configuration.
- `size` (optional): route-specific dimensions (`WIDTHxHEIGHT`) or named size.
- `aspect_ratio`, `resolution` (optional): native route-specific ratio/resolution.
  Do not combine either with `size`. Query the selected model's generation
  contract before supplying explicit generation parameters.
- `background` (optional): one of `auto`, `transparent`, or `opaque`.
- `output_format` (optional): final delivered `png` (default), `jpeg`, or `webp`.
  The runtime requests a native format when supported and otherwise converts
  locally. JPEG conversion is lossy (quality 90); PNG/WebP conversion is lossless.
- `name` (optional): filename stem used for the saved image.

Prompt-only calls use the selected route's defaults without inventing optional
provider parameters. Explicit unsupported or unconfirmed generation parameters
fail before generation. JPEG cannot preserve transparency; transparent+JPEG is
rejected before generation and transparent source images are never flattened.
Single-frame PNG/JPEG/WebP delivery validates signatures/decoding, with 32 MiB
source/output, 8192-pixel edge, 16,777,216-pixel and 192 MiB decoder-allocation
limits. Conversion preserves display orientation and PNG/WebP alpha.
The original is saved before local conversion. If local delivery fails,
generation has already succeeded: recover the returned original locally rather
than generating again. Results separate selection, requested/provider-effective
parameters, source images, actual dimensions and conversion/encoding policy.
