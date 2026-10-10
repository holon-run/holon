# Provider Quota Incident Circuit Breaker

Decision:

- key the durable quota incident by the provider-issued `ProviderQuotaIdentity`,
  not by a provider request or message lineage
- keep the incident state in the runtime database so consecutive failures,
  retry time, episode boundaries, and resolution survive turns and restart
- classify cross-turn `RateLimited` fallback as `Deferred`; after the incident
  threshold, park the current WorkItem through the existing `WaitFor` /
  blocked-recheck path instead of adding a provider-specific scheduler
- continue using `ProviderQuotaCoordinator` for in-process request
  coordination; the database incident is the durable circuit-breaker boundary

Reason:

- a message-lineage retry budget resets when an ordinary system tick starts a
  new turn, which permits an unbounded 429 loop
- provider quota exhaustion is shared across fallback attempts and may affect
  multiple turns, while different quota identities must remain isolated
- the existing wait and scheduler contracts already persist blocked WorkItems
  and emit an exact recheck, so reusing them preserves scheduler semantics and
  restart recovery
