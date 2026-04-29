# Compatible Provider Catalog

Holon now ships built-in provider and model defaults for providers that fit the
runtime's existing transport contracts.

The catalog is intentionally named around compatibility rather than any
upstream source. These entries are Holon runtime defaults: they describe
provider ids, endpoint/auth defaults, and conservative model metadata that the
current runtime can call directly.

The boundary is transport capability. Providers that need native auth flows,
custom per-provider headers, SDK credential chains, or dynamic gateway discovery
stay outside the built-in catalog until Holon has explicit contracts for those
requirements. Operators can still configure those providers manually through
the extensible provider configuration surface.
