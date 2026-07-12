# 072 Mistral Model Catalog

## Choice

Keep the built-in direct Mistral catalog focused on the current API aliases for:

- Codestral 25.08
- Mistral Large 3
- Mistral Medium 3.5
- Mistral Small 4

Mark the three general-purpose Mistral models as image-input capable. Mark only
Mistral Medium 3.5 as reasoning capable because its current model schema
explicitly declares reasoning output. Do not expose reasoning effort options.

Remove the built-in Devstral 2, Magistral Small 1.2, Mistral Medium 3.1, and
Pixtral Large entries. Their current model records are deprecated, and the
active general-purpose models replace their catalog roles.

## Reason

Mistral's current model records identify Large 3, Medium 3.5, Small 4, and
Codestral 25.08 as active API models. The three general-purpose models accept
image input; Codestral accepts text only. Medium 3.5 explicitly emits reasoning
and text, while the other active entries do not declare reasoning output.

The deprecated entries have documented replacements: Medium 3.5 replaces
Devstral 2, Medium 3.1, and Pixtral Large, while Small 4 replaces Magistral
Small 1.2. Keeping only the current aliases avoids presenting deprecated models
as recommended defaults.

Mistral's model records define context length but do not define a separate
maximum generated-token field. This calibration therefore updates model
identity, context, and capabilities without claiming larger output limits.

## Sources

Verified 2026-07-12:

- <https://github.com/mistralai/platform-docs-public/tree/main/src/schema/models/models>
- <https://github.com/mistralai/platform-docs-public/blob/main/src/schema/models/models/codestral-25-08.ts>
- <https://github.com/mistralai/platform-docs-public/blob/main/src/schema/models/models/mistral-large-3-25-12.ts>
- <https://github.com/mistralai/platform-docs-public/blob/main/src/schema/models/models/mistral-medium-3-5-26-04.ts>
- <https://github.com/mistralai/platform-docs-public/blob/main/src/schema/models/models/mistral-small-4-0-26-03.ts>
- <https://github.com/mistralai/platform-docs-public/blob/main/src/schema/models/models/devstral-2-25-12.ts>
- <https://github.com/mistralai/platform-docs-public/blob/main/src/schema/models/models/magistral-small-1-2-25-09.ts>
- <https://github.com/mistralai/platform-docs-public/blob/main/src/schema/models/models/mistral-medium-3-1-25-08.ts>
- <https://github.com/mistralai/platform-docs-public/blob/main/src/schema/models/models/pixtral-large-24-11.ts>
