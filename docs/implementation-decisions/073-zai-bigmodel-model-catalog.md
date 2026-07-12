# Z.AI and BigModel model catalogs

Holon keeps separate built-in model lists for the international `zai` provider
and the mainland China `bigmodel` provider.

Both services expose Anthropic-compatible endpoints and share several current
GLM models, but their official model overviews are not identical. Z.AI lists
models such as `glm-4.5-x`, `glm-4.5v`, and `glm-4.6v-flashx`; BigModel instead
lists domestic-only or differently versioned entries such as `glm-4-long`,
`glm-4-flashx-250414`, and `glm-4.1v-thinking-flashx`.

The catalog therefore records only chat-completion models present in each
service's current official overview rather than cloning one provider's list
onto the other. Shared model names remain separate entries so later endpoint
changes do not silently alter the other provider.

Sources:

- Z.AI model overview: `https://docs.z.ai/guides/overview/overview`
- Z.AI GLM-5.2 guide: `https://docs.z.ai/guides/llm/glm-5.2`
- BigModel model overview:
  `https://docs.bigmodel.cn/cn/guide/start/model-overview`
