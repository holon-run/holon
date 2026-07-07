Generate one image from a text prompt using a configured image-generation model.

The runtime saves the generated image under `agent_home/media/generated` and returns
a `workspace://...` URI plus metadata. Do not request multiple images in one call;
call this tool multiple times when distinct images are needed.

Inputs:
- `prompt` (required): detailed image-generation prompt.
- `size` (optional): one of `1024x1024`, `1536x1024`, or `1024x1536`.
- `background` (optional): one of `auto`, `transparent`, or `opaque`.
- `output_format` (optional): one of `png`, `jpeg`, or `webp`.
- `name` (optional): filename stem used for the saved image.
