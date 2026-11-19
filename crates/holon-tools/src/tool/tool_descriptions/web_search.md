Search the web through Holon's web provider registry and return structured results and citations. Use WebFetch after search when full page content is needed.

- Omit `provider` to use the instance's configured provider and routing mode.
- Use `provider: "auto"` to use the automatic candidate order with the configured mode.
- Use `provider: "aggregate"` to query the same automatic candidates in order for this call only. Results are deduplicated by canonical URL, and querying stops when enough unique results are found or the attempt limit is reached.
- Use a concrete provider ID to query only that provider, with no fallback.

`auto` and `aggregate` are reserved WebSearch argument values. If that ID is also registered, passing it explicitly is an error; rename the configured provider or omit the argument.

Explicit aggregation can send the same query to multiple providers even when the default mode is `single`, increasing disclosure, costs, or latency. Set `web.search.max_provider_attempts` to `1` to cap each call at one attempt. This is not a provider allowlist and does not limit separate calls.
