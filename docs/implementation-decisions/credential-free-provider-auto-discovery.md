# Credential-Free Provider Auto Discovery

Decision:

- automatic model discovery and discovery-cache loading are limited to providers
  with a credential requirement, providers declared as `providers.<id>` in
  `config.json`, and providers referenced by a configured model route
  (`model.default`, `model.fallbacks`, `vision.default`,
  `image_generation.default`, vision candidate routes) or a validated model
  override.
- already-polluted discovery-cache entries are filtered on read:
  `RuntimeModelCatalog` omits models whose provider endpoint is not opted in, so
  the file keeps its historical entries but the catalog never serves them.

Reason:

- built-in providers such as `ollama` and `vllm` declare
  `CredentialKind::None`, so `credential_configured` is true without any user
  action. Probing their default loopback ports on every start records whatever
  unrelated service listens there, and the resulting models stayed in
  `model-discovery-cache.json` after that service stopped.
- the config file is the durable, reviewable evidence that the operator actually
  runs the local service; runtime-only state such as an agent model override
  still selects an explicit model route, which resolves without discovered
  metadata.

Preserved boundary:

- providers with a credential requirement keep the historical discovery
  behavior, including `Required`/`Optional` discovery auth handling.
- explicit `holon config models refresh --provider <id>` remains available; it
  writes the cache, but the catalog only serves the entry once the provider is
  opted in as above.
