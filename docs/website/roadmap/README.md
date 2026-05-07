---
title: Roadmap
summary: The order in which Holon is defining its runtime.
order: 50
---

# Roadmap

Holon is early-stage. The roadmap prioritizes runtime clarity before adapters,
plugins, or UI surfaces.

## Current priorities

1. Runtime model and message envelope.
2. Queue, wake, sleep, and task lifecycle.
3. Event ingress and trust classification.
4. Structured user-facing output.
5. Integrations and adapters.

## What this means for the website

The documentation site should mirror that priority:

- Explain the runtime vocabulary before promoting integrations.
- Keep Markdown source readable for humans and agents.
- Link to deeper repository docs and RFCs when a design needs more context.
- Avoid publishing unstable behavior as if it were a settled public contract.

## Near-term documentation work

- Add architecture diagrams once the core envelope and lifecycle names settle.
- Add concrete CLI examples after the command surface stabilizes.
- Add deployment docs once HTTP and worker adapters are ready for external use.
- Add release notes and migration guides when packaged releases become routine.

## Non-goals for now

- UI-first product pages.
- A plugin marketplace.
- Hidden automation that cannot be inspected as runtime state.
- Documentation that contradicts the repository runtime specs.

<!-- INDEX:START -->

<!-- INDEX:END -->
