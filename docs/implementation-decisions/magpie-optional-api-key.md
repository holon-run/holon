# Magpie Optional API Key

Decision:

- the built-in `magpie` provider declares an optional `MAGPIE_API_KEY`
  (`credential_envs = ["MAGPIE_API_KEY"]`), so config surfaces report
  `credential_kind = "api_key"` and `api_key_supported = true`, while
  `ModelDiscoveryAuth` stays `Optional`.
- `built_in_provider_allows_anonymous_access` (in
  `crates/holon-domain/src/provider_registry.rs`) marks the providers that may
  skip the credential requirement. Only `magpie` is listed. When the key is
  unset, `provider_has_usable_auth` still returns true and the Anthropic
  transport sends no `Authorization` header instead of failing.
- both checks key on `route_provider`, so a plan alias whose runtime `id` is
  `<provider>-<plan>` keeps the anonymous path.
- a persisted `providers.magpie` entry (for example a custom `base_url`) keeps
  the built-in optional key binding: the default config template preserves the
  built-in auth for anonymous-access providers (mirroring the Copilot
  exception), and materialization falls back to the built-in binding when a
  stored entry carries an effectively empty auth block, which covers entries
  written before that template change.

- declaring an optional key must not opt magpie into automatic discovery:
  `ModelCatalogConfig::provider_auto_discovery_allowed` exempts
  anonymous-access built-ins from the "providers with a required credential keep
  the historical behavior" branch, so magpie stays gated on an explicit
  `providers.magpie` entry, a model route reference, or an explicit refresh.
  Otherwise every gateway start would probe `127.0.0.1:3425` and cache whatever
  unrelated service answers there.

Reason:

- the official Magpie docs state that a loopback gateway accepts any key
  (metering is by the first User-Agent word); only cross-machine "Share on
  local network" access requires the user gateway key. `MAGPIE_API_KEY` is a
  Holon-side convention, not an official Magpie environment variable.
- treating the credential as required would regress the zero-config loopback
  path; treating it as absent (`CredentialKind::None`) hides the API Key input
  and cannot express the cross-machine attribution case. Declaring it optional
  keeps both the anonymous default and the configurable attribution path.

Preserved boundary:

- providers with a required credential keep the historical behavior: a missing
  credential still fails, because they are not in the anonymous-access set.
- when `MAGPIE_API_KEY` is set, the existing API Key provider path injects
  `Authorization: Bearer magpie-<app-id>` unchanged.
- `providers.magpie.auth.source: none` does not disable the built-in optional
  key: an effectively empty auth block falls back to the built-in binding,
  because the config format has no separate presence marker. Operators who need
  the anonymous path simply leave `MAGPIE_API_KEY` unset.
- the docs reference page renders `MAGPIE_API_KEY (optional)` from
  `ProviderDocEntry.credential_optional`, so the generated English page and its
  zh-CN copy stay identical instead of being edited by hand.
