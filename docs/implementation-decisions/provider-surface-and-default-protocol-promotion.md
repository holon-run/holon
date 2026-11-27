# Provider surface and default-protocol promotion

## Choice

- `ProviderDefinition` gained a declarative `surface` field
  (`ProviderSurface::Primary | SubEndpoint { of }`). `materializer` still decides
  whether a definition is materialized; `surface` decides whether the
  materialized entry is a first-class provider or an internal protocol endpoint.
  `AliasOnly` is unchanged and still means "never materialize".
- Visibility is projected by one function, `provider_exposure`
  (`crates/holon-runtime/src/provider/exposure.rs`). The runtime config surface,
  `config providers list`, and onboarding all read it; the onboarding
  `-openai`/`-anthropic` suffix heuristic was removed.
- A sub-endpoint is `Advanced` only when the user configured it, either through
  `providers.<parent>.endpoints.<endpoint>` or the legacy flat
  `providers.<sub-id>` form. Otherwise it is routable but not shown.
- Default protocols follow Anthropic Messages > OpenAI Responses > OpenAI Chat
  Completions. `REVIEWED_DEFAULT_ENDPOINTS` in
  `crates/holon-model-catalog/src/lib.rs` names the reviewed promotions
  (`tencent-tokenhub`, `opencode-go` → `messages`); every other endpoint of
  those providers stays routable.

## Reason

`route_endpoint != "default"` cannot decide visibility: subscription variants
(`dashscope-token-plan`, `volcengine-coding`, `xiaomi-token-plan`) are non-default
endpoints that must stay first-class. Only explicit metadata can separate
"routable" from "shown by default".

Protocol promotion is a pointer change, not a transport change: the model face
(`ModelProviderEntry`, grouped by `(family, endpoint)`) still answers "where can
this model be called", so chat-only routes must stay registered. Promoting the
pointer instead of rewriting definitions keeps `<provider>@default/<model>`
working and makes the change reversible without a cache migration, because
discovery caches are keyed by `(provider, endpoint)`.

## Preserved boundaries

- Tencent TokenHub sub-endpoint routes are registered under their own legacy id
  (`tencent-tokenhub-messages`), so the promotion also runs against the parent
  provider for the provider-level default; per-model promotion for OpenCode Go
  is excluded because its official table documents a native protocol per model.
- Reviewed Messages policies reuse the reviewed per-model chat facts; the
  Responses endpoint keeps its compatibility-mode limitations until it is
  reviewed model by model.
