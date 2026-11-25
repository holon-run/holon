# Provider Discovery Decoder Selection

Decision:

- expose `decoder` as an independent optional field on `ProviderDiscoveryConfig`
  (`discovery.decoder`) instead of adding a new `ProviderDiscoveryKind` value or a
  built-in `magpie` provider preset.
- the config vocabulary is `ModelDiscoveryDecoder::CONFIG_ALLOWED`, which excludes
  `Ollama`; `as_str`/`parse` mirror `ProviderDiscoveryKind`.
- when `decoder` is absent, `openai_models` keeps the historical
  `OpenAiCompatible` decoder.

Reason:

- discovery is a two-axis concern (`route` + `decoder`); the config previously
  exposed only `route` and hardcoded the decoder, so Magpie and other decoder
  variants were unreachable without code changes.
- a built-in `magpie` preset would invent a default base URL and credentials for
  a user-hosted service, which the issue explicitly forbids.
- `ollama` is decoded by a dedicated refresh path and its payload branch is
  `unreachable!()`; allowing it in config would let operators panic the runtime.

Preserved boundary:

- `disabled` still rejects `path` and now also `decoder`; an unknown decoder
  value, or a decoder set before `kind`/`path`, is a hard config error rather
  than a silent fallback. Transport, catalog policy, and built-in provider
  definitions are unchanged.
