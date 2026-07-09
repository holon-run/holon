# Canonical Model Ref with Legacy Alias

## Choice

Built-in image generation models that belong to a multi-endpoint provider
(e.g. Volcengine Seedream under the `image-openai` endpoint) are now cataloged
under their **canonical provider id** (`volcengine`) with an explicit `endpoint`
field, instead of under the legacy flattened provider id
(`volcengine-image-openai`).

A legacy alias map (`BuiltInModelCatalog::legacy_aliases`) transparently
resolves old model refs like `volcengine-image-openai/doubao-seedream-5.0-lite`
to the canonical form `volcengine/doubao-seedream-5.0-lite` inside
`resolve_model_route`, so existing user configs continue to work without
manual migration.

## Reason

The catalog, docgen, and settings UI should all present one canonical
provider identity. Keeping the legacy id as a separate catalog entry created
provider sprawl and made the three-column (provider / endpoint / legacy)
identity visible only in docs, not in the runtime model ref itself.

## Preserved boundary

Legacy provider ids remain valid as configuration keys and as model ref input;
they are normalized to canonical form at route resolution time. No user-facing
breaking change; old configs work unchanged.
