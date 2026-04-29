# External Event Surfaces

Decision:

- implement three minimal external surfaces:
  - generic webhook
  - timer creation
  - remote prompt ingress
- authenticate remote prompt ingress with `HOLON_REMOTE_TOKEN`

Reason:

- these are enough to prove the runtime is truly event-driven
- the remote route is intentionally small and explicit
