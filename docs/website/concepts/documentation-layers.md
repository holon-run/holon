---
title: Documentation layers
summary: How Holon separates user-facing docs, current-contract reference, and maintainer design records — and which layer to read for which question.
order: 30
---

# Documentation layers

Holon's documentation is organized into three layers with clear boundaries.
This page helps you find the right layer for your question.

## Quick guide: which docs should I read?

| I want to... | Start here |
|-------------|-----------|
| Install and run Holon | [Getting started](/getting-started/) |
| Understand core concepts | [Concepts overview](/concepts/) |
| Find a CLI command or config key | [Reference](/reference/) |
| Integrate Holon into another system | [Integration guide](/guides/integration/) |
| Understand a runtime contract | [RFCs](https://github.com/holon-run/holon/tree/main/docs/rfcs) |
| See why a design choice was made | [Implementation decisions](https://github.com/holon-run/holon/tree/main/docs/implementation-decisions) |
| Contribute to the runtime | [Architecture overview](https://github.com/holon-run/holon/blob/main/docs/architecture-overview.md) |

## The three layers

### Layer 1: User-facing website (`docs/website/`)

**Audience:** New users, evaluators, integrators, and operators.

**Goal:** Explain what Holon is, guide first success, and document the current
surface without requiring RFC knowledge.

| Section | Purpose |
|---------|---------|
| Home | Product promise, audiences, brand hierarchy |
| Getting started | Shortest path from install to first agent interaction |
| Concepts | Mental model: agents, work items, tasks, queues, trust boundaries |
| Guides | Task-oriented workflows grouped by user job |
| Reference | Current-contract CLI, config, and HTTP control-plane snapshots |

**Rule:** Website pages explain *what* and *how*. Link to RFCs for *why the
design works that way*.

### Layer 2: Current public contract

**Audience:** Users running, configuring, integrating, or troubleshooting Holon.

**Goal:** Describe only behavior that is current or explicitly marked as changing.

**Includes:**

- `docs/website/reference/` — CLI, configuration, and HTTP control-plane pages
  verified against the compiled runtime
- `README.md` — high-level project entry with install, provider setup, and docs
  navigation
- `docs/runtime-spec.md` — implementation-facing aggregate spec (not the sole
  authority; accepted RFCs and reference pages are more specific)

**Rule:** Reference pages track a specific release version. If behavior is
experimental, the page says so.

### Layer 3: Maintainer design (`docs/rfcs/`, `docs/implementation-decisions/`, `docs/archive/`)

**Audience:** Maintainers and contributors changing runtime behavior.

**Goal:** Preserve architecture contracts, design rationale, and historical
decisions.

| Location | Content type |
|----------|-------------|
| `docs/rfcs/` | Canonical design contracts — one RFC per runtime concept |
| `docs/implementation-decisions/` | ADR-style records — one decision per file |
| `docs/archive/` | Superseded notes and historical design docs |
| `docs/architecture-overview.md` | Short architecture map with RFC reading path |

**Rule:** When a runtime concept changes, update the RFC first. Implementation
decisions capture *why*. Archives preserve history.

## Authority boundaries

When sources conflict, prefer the more specific and current document:

1. **Accepted RFC** (specific domain) over `runtime-spec.md` (aggregate)
2. **Reference page** (current release) over RFC (design intent)
3. **Website concept page** (user explanation) over RFC (design detail)
4. **Implementation decision** (rationale) over archived notes (history)

`docs/runtime-spec.md` is an early aggregate contract, not the single source of
truth. When a domain has a more specific accepted RFC or a current reference
page, prefer that more specific document.

## Cross-layer links

- Website pages link to RFCs as deeper design background — not as required reading.
- The repository `README.md` links to the website as the user-facing entry point.
- The architecture overview (`docs/architecture-overview.md`) points to RFCs for
  detailed contracts.

## When to update which layer

| Change | Primary target | Secondary |
|--------|---------------|-----------|
| New CLI command | Reference page | Guides, getting-started |
| Config key change | Reference page, config schema | Guides that reference it |
| Runtime concept change | RFC | Concepts page |
| New user workflow | Guides | Getting-started |
| Product messaging | Homepage | README |
| Design rationale | Implementation decision | RFC if normative |

## See also

- [Architecture overview](https://github.com/holon-run/holon/blob/main/docs/architecture-overview.md) — repository architecture map
- [RFC index](https://github.com/holon-run/holon/tree/main/docs/rfcs) — all current RFCs
- [Implementation decisions](https://github.com/holon-run/holon/tree/main/docs/implementation-decisions) — design rationale records
