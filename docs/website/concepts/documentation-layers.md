---
title: Documentation layers
summary: How Holon separates getting started, concepts, guides, reference, and maintainer runtime specs.
order: 20
---

# Documentation layers

Holon's documentation is organized into five distinct layers with explicit
boundaries, audience definitions, and content contracts. This prevents drift
between user-facing mental models, authoritative reference contracts, and
internal runtime specifications.

## The five documentation layers

| Layer | Audience | Primary question answered | What belongs here | What does NOT belong here |
|---|---|---|---|---|
| **Getting started** (`/getting-started/`) | New users & evaluators | *"How do I get my first successful result with Holon?"* | Shortest-path installation, onboarding, first agent interaction, quick verification, next-step branches | Full CLI command trees, complete config catalogs, API endpoints, internal state models |
| **Concepts** (`/concepts/`) | Users & integrators | *"What is Holon's mental model and why does it work this way?"* | Stable objects (Agent, WorkItem, Task, Workspace), trust boundaries, memory, continuity, observable invariants | Step-by-step how-to steps, exact flag/endpoint lists, internal module/struct details, volatile engine mechanics |
| **Guides (How-to)** (`/guides/`) | Practitioners & operators | *"How do I accomplish a specific task X?"* | Task-driven workflows: goal, prerequisites, step-by-step commands, verification, troubleshooting, related links | Full argument reference tables, internal scheduler algorithms, design debates |
| **Reference** (`/reference/`) | Users, integrators & operators | *"What is the exact, authoritative definition of this flag, endpoint, or configuration key?"* | CLI command tree, configuration schema, HTTP control plane endpoints, model catalog, tool schemas, status enums | Narrative tutorials, onboarding paths, architecture rationale, internal state machines |
| **Spec** (`/spec/`) | Maintainers & contributors | *"What is the current internal runtime contract that changes must preserve?"* | Scheduler state machines, execution-root invariants, task lifecycle contracts, internal security boundaries | User onboarding, marketing narrative, quick start workflows |

## Layer guidelines & content contracts

### 1. Getting started — First success

- **Purpose:** Take a new user from zero to a verified running agent in under 15 minutes.
- **Format:** Minimal commands, clear prerequisites, explicit expected output, and pointers to next paths.
- **Rule:** Never dump complete option lists or internal repository layouts into getting started pages. Keep the path uninterrupted.

### 2. Concepts — User-facing mental models

- **Purpose:** Explain Holon's core building blocks and observable semantics so users can predict runtime behavior.
- **Audience:** Regular users and integrators, **not** runtime engine developers.
- **Content principles:**
  - **Audience-first:** Write for the person operating or building on top of Holon.
  - **Stable semantics only:** Focus on durable concepts (e.g. why agents persist, how work items track objectives, what trust boundaries isolate).
  - **Do not disguise implementation details as concepts:** Never burden users with internal Rust module names, struct layouts, database tables, or private queue mechanics just because they exist in code.
  - **Behavioral durability check:** A concept page must remain accurate even if internal queue or storage implementations are refactored.
  - **Link rather than duplicate:** Link to Reference for exact parameters and to Spec or RFCs for maintainer contracts.

### 3. Guides — Task-oriented how-to

- **Purpose:** Solve a concrete user problem ("How do I integrate via HTTP?", "How do I monitor with Prometheus?").
- **Standard guide structure:**
  1. **Goal & context:** What will be achieved and when to use this approach.
  2. **Prerequisites:** Tools, keys, or permissions needed before starting.
  3. **Step-by-step instructions:** Minimal reproducible commands with explanations.
  4. **Verification:** How to confirm the task succeeded.
  5. **Troubleshooting:** Common failure modes and immediate remedies.
  6. **Related reference & concepts:** Direct links to authoritative reference and background concepts.
- **Rule:** Guides provide minimal inline examples. Complete endpoint tables or configuration schemas belong in Reference.

### 4. Reference — Current authoritative contract

- **Purpose:** The single source of truth for syntax, options, endpoints, and schema definitions.
- **Structure:** Standardized tables and sections: Scope, Syntax / Endpoint, Parameters / Payload, Return values, Limits, and Stability.
- **Rule:** Reference pages are authoritative snapshots verified against the compiled binary (`holon --help`, `holon config schema`, route inventory). If an option or endpoint changes, update Reference first.

### 5. Spec — Maintainer runtime contracts

- **Purpose:** Document internal contracts, lifecycle state transitions, and invariants that engine contributors must adhere to.
- **Audience:** Contributors and maintainers working on Holon's codebase.
- **Rule:** Spec pages are normative specifications, not user documentation. User-facing navigation de-emphasizes spec pages so beginners are not confused by internal mechanics.

## Supporting maintainer layers

- **`docs/rfcs/` (Design RFCs):** Canonical design rationale and architectural debates behind major capabilities.
- **`docs/implementation-decisions/` (ADRs):** Lightweight records explaining why a specific technical choice was made when multiple viable options existed.
- **`docs/archive/`:** Preserved historical notes that are no longer active specifications.
- **`docs/website/maintainers/` (Maintainer workflows):** Build, test, and documentation workflows for people changing Holon itself.

## Cross-layer links

A well-structured document links across layers rather than copying content:

```
Getting started ──> Guides (for specific tasks)
      │                │
      ▼                ▼
  Concepts ──────> Reference (for exact parameters/endpoints)
      │
      ▼ (maintainers only)
    Spec ─────────> RFCs / ADRs (for design rationale)
```

## When to update which layer

| Kind of change | Primary documentation target | Cross-references to update |
|---|---|---|
| New CLI command or flag | `reference/cli.md` | Relevant `guides/` or `getting-started/` if part of a core path |
| New HTTP endpoint | `reference/http-control-plane.md` | Relevant how-to in `guides/` |
| New user workflow or task | `guides/<task>.md` | Links to `reference/` and `concepts/` |
| Core object mental model change | `concepts/<model>.md` | `spec/` for engine contracts, `reference/` |
| Internal scheduler or state machine update | `spec/<contract>.md` | Source code tests & maintainer notes |
| Architectural design decision | `docs/rfcs/` or `docs/implementation-decisions/` | Link from `spec/` |
