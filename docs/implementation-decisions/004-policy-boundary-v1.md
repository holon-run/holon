# Policy Boundary V1

Decision:

- preserve `origin`, `delivery_surface`, `admission_context`, and
  `authority_class`
- keep `TrustLevel` as a compatibility bridge while origin validation remains
  separate from authority labels
- record non-message control operations through audit events

Reason:

- provenance vocabulary should stabilize before a full allow/deny matrix
- transport admission and runtime authority are different concerns
- explicit audit provenance is enough for phase 1
