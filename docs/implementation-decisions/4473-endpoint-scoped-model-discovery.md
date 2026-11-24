# Endpoint-scoped model discovery

## Choice

Model discovery is configured with `providers.<id>.discovery` plus optional
`providers.<id>.endpoints.<endpoint_id>.discovery` (`kind` = `openai_models` |
`disabled`, `path` relative to the effective endpoint base URL). The request
always uses the endpoint base URL, credential and headers. Cached results and
discovered models are keyed by `provider + endpoint + model_id`, and the model
catalog's discovered-model map is keyed by `ModelRouteRef` instead of `ModelRef`.

## Reason

Gateway providers (Magpie, LiteLLM, one-api) expose one model list but several
protocol entry points whose bases differ. Deriving the discovery URL only from
the provider-level `base_url` and transport either forced one provider id per
protocol or a hand-written `models.catalog`, fragmenting a single gateway's
model list. Decoupling `discovery` from `transport` keeps `transport` about how
Holon calls the endpoint and `discovery` about where the list lives, and a
route-keyed cache keeps sibling endpoints from overwriting each other.

## Preserved boundaries

- The declared path is appended to the endpoint base path, so the joined URL
  cannot change scheme, host or port; `..`, absolute URLs, query strings and
  percent-escapes are rejected at config load.
- Transport, credential and header selection still come from configuration;
  responses only produce model ids and metadata.
- Selecting a non-default endpoint still uses the existing plan/route mechanism
  (no cross-endpoint smart routing), and discovery failure stays non-blocking.
