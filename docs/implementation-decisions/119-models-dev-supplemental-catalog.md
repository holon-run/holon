# models.dev Supplemental Catalog

Decision:

- upgrade the weekly models.dev refresh from discovery-only to a
  review-gated supply line: `holon models-dev refresh` drafts new catalog
  entries into the checked-in `models.dev/supplemental_catalog.json`
- auto-draft only for a curated provider allowlist
  (`AUTO_SUPPLEMENT_PROVIDERS`); aggregators and gateways (openrouter,
  huggingface, nvidia, venice, nearai, together, fireworks, chutes, arcee)
  stay out because their catalogs mirror other providers
- a candidate must be missing from the compiled-in catalog, emit text
  output, and carry an upstream release date inside a 120-day window; the
  window gates entry only, retention is sticky (entries stay while they
  exist upstream and are not promoted into the compiled-in catalog)
- the runtime merges the checked-in supplement into the built-in catalog at
  startup (`src/model_catalog/snapshot.rs`): metadata plus a
  default-endpoint route per entry, never overriding compiled-in entries,
  aliases, routes, credentials, or preferred models
- the refresh PR body is generated from `models.dev/refresh-summary.md`
  (drafted/retained/removed/deferred lists); merge review is the admission
  gate
- `holon models-dev` commands are pure data utilities and load config in
  inspection mode, so the refresh workflow runs on credential-less CI
  runners; refresh change detection uses `git status --porcelain` because
  the first refresh run creates previously untracked files

Reason:

Discovery-only refresh produced information but no capability: new models
(for example `xai/grok-4.6`) were visible in the snapshot yet unusable
until someone hand-edited the Rust catalog. Full automation over all mapped
providers would import the entire models.dev archive (870+ models
including dated snapshots and aggregator mirrors). The allowlist plus
recency gate keeps drafts small and current-generational, while sticky
retention keeps the catalog stable across weekly refreshes.

Preserved boundary / tradeoff:

- supplement entries are metadata only; a model still needs provider
  credentials and endpoint configuration to be callable
- curated defaults still belong in the compiled-in catalog; promoting a
  supplement entry to `builtin_snapshot_v1.json` removes it from the
  supplement automatically on the next refresh
- aggregators and unmapped providers remain audit data; adding a provider
  to the allowlist is a code change with review
- upstream metadata corrections ride along automatically with retained
  entries; deliberate Holon-specific overrides must be promoted to the
  compiled-in catalog
