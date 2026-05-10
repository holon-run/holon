---
title: Documentation layers
summary: How Holon separates product docs, current-contract reference, and maintainer design records.
order: 30
---

# Documentation layers

Holon's documentation is organized into three layers with clear boundaries.
This prevents drift between what users see, what the runtime actually does, and
what maintainers are designing next.

## Layer 1: Product website (`docs/website/`)

**Audience:** New users, evaluators, integrators, and contributors learning Holon.

**Goal:** Explain why Holon exists, guide first success, and expose stable-enough
concepts without overwhelming readers with RFC detail.

**Structure:**

| Section | Purpose |
|---------|---------|
| Home | Product promise, audiences, brand hierarchy, first CTA |
| Getting started | Shortest path from clone to first agent interaction |
| Concepts | Four-object mental model before deep lifecycle vocabulary |
| Guides | Task-oriented workflows grouped by user job |
| Reference | Current-contract CLI, config, and control-plane snapshots |
| Roadmap | User-facing milestones and stability expectations |

**Rule:** Website pages should explain *what* and *how*, not *why the design
works that way*. Link to RFCs for design rationale.

## Layer 2: Current public contract

**Audience:** Users running, configuring, integrating, or troubleshooting Holon.

**Goal:** Describe only behavior that is current or explicitly marked unstable.

**Includes:**

- `docs/website/reference/` — CLI, configuration, and HTTP control-plane pages
- `README.md` — high-level repository entry and contributor orientation
- `docs/runtime-spec.md` — implementation-facing spec (not a user tutorial)

**Rule:** Reference pages should be verified against the compiled runtime
(`holon --help`, `holon config schema`). Mark the version each page was last
checked against. If behavior is experimental, say so.

## Layer 3: Maintainer design (`docs/`, `docs/rfcs/`, `docs/implementation-decisions/`)

**Audience:** Maintainers and contributors changing runtime semantics.

**Goal:** Preserve architecture contracts, design rationale, and historical
decisions. These are the canonical source of truth for runtime behavior.

**Structure:**

| Location | Content type |
|----------|-------------|
| `docs/rfcs/` | Canonical design contracts — one RFC per runtime concept |
| `docs/implementation-decisions/` | ADR-style records — one decision per file |
| `docs/archive/` | Superseded notes, historical design docs |
| `docs/documentation-cleanup-audit.md` | Maintenance audit tracking drift between layers |

**Rule:** When a runtime concept changes, update the RFC first. Implementation
decisions capture *why* a choice was made. Archives preserve history without
cluttering the active surface.

## Cross-layer links

- Website pages link to RFCs as "deeper design background" — not as required
  reading.
- The repository `README.md` links to the website as the product entry point.
- The documentation cleanup audit (`docs/documentation-cleanup-audit.md`)
  tracks stale references and vocabulary drift across layers.

## When to update which layer

| Change | Primary target | Secondary |
|--------|---------------|-----------|
| New CLI command | Reference page | Guides, getting-started |
| Config key change | Reference page, config schema | Reference page in site |
| Runtime concept change | RFC | Concepts page, reference |
| New user workflow | Guides | Getting-started |
| Product messaging | Homepage | README |
| Design rationale | Implementation decision | RFC if normative |
