# Canonical Model Catalog with Route Metadata

## Choice

Built-in models are cataloged once under their **canonical provider id**.
Endpoint- and plan-specific availability, limits, capabilities, and preferred
selection are indexed separately by `ModelRouteRef` (`provider@endpoint/model`).
This applies both to legacy image aliases such as Volcengine `image-openai` and to
flattened plan providers such as `volcengine-agent`,
`dashscope-token-plan`, and `xiaomi-token-plan`.

A legacy alias map (`BuiltInModelCatalog::legacy_aliases`) transparently
resolves old model refs like `volcengine-image-openai/doubao-seedream-5.0-lite`
to the canonical form `volcengine/doubao-seedream-5.0-lite`. Catalog
construction also derives aliases for flattened built-in provider ids, while
preserving their exact endpoint as a route. Existing user configs therefore
continue to resolve without manual migration.

The legacy Volcengine image alias resolves to the standard `default` endpoint:
Seedream uses Ark's `/api/v3/images/generations` API, not the Agent Plan
`/api/plan/v3` endpoint. `VOLCENGINE_IMAGE_OPENAI_API_KEY` remains accepted as a
backward-compatible credential fallback for the standard endpoint.

## Reason

The catalog, diagnostics, and settings surfaces should present one canonical
model identity. Keeping plan or endpoint ids as separate catalog providers
duplicates intrinsic model metadata and can silently mix route-specific limits
when the same model is available through multiple endpoints. Separate route
metadata keeps model identity stable while retaining exact transport and plan
contracts.

## Preserved boundary

Legacy provider ids remain valid as configuration keys and as model ref input;
they are normalized to canonical form at route resolution time. Explicit route
selection continues to use `provider@endpoint/model`, and legacy implicit
selection uses the catalog's preferred route for that provider or model. No
user-facing breaking change; old configs and selectable routes work unchanged.
