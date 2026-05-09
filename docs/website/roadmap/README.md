---
title: Roadmap
summary: What you can rely on today, what's coming next, and what's still experimental.
order: 50
---

# Roadmap

Holon is pre-1.0. This page tells you what's stable enough to use, what's
actively changing, and where the project is headed.

## What you can rely on today

These surfaces are stabilizing and unlikely to change without notice:

- **Agent model:** Agents have durable homes, identities, and lifecycles.
  Creating, listing, and inspecting agents works reliably.
- **Work items:** Durable objectives with plans, todo lists, and completion
  tracking are production-grade.
- **Task supervision:** Shell commands and child agent delegation through the
  task handle model are stable.
- **Daemon mode:** `holon daemon start/stop/status/logs` is the recommended way
  to run Holon interactively.
- **Control plane:** `holon serve` exposes a working HTTP API for integration.
- **Trust classification:** Origin and trust levels (`trusted-operator`,
  `trusted-system`, `trusted-integration`, `untrusted-external`) are enforced
  at ingress.

## What's experimental

These surfaces work but may change shape or naming:

- **CLI command surface:** Command names, flags, and subcommand trees may
  reorganize. Use `holon --help` as the authority over any written reference.
- **Configuration schema:** Config keys and credential profiles may evolve.
- **Provider and model configuration:** Provider registration, fallback
  behavior, and model catalog are still being refined.
- **TUI:** The terminal UI is functional but not final in layout or navigation.
- **Skills system:** Skill loading, discovery, and the SKILL.md contract are
  stable in concept but may gain new capabilities.

## What's coming next

The project prioritizes runtime clarity before adapters, plugins, or UI
surfaces. Near-term work:

1. **Stabilize the public API contract** — lock down CLI, config, and HTTP
   surface shapes so integrations can depend on them.
2. **Provider catalog** — first-class support for a verified set of model
   providers with documented credential flows.
3. **Event ingress model** — formalize how webhooks, timers, and external
   triggers enter the runtime queue.
4. **Packaged releases** — provide pre-built binaries and versioned release
   notes so users don't need to build from source.

For the detailed implementation roadmap and current RFCs, see the repository
[docs/rfcs/](https://github.com/holon-run/holon/tree/main/docs/rfcs) and
[docs/next-phase-direction.md](https://github.com/holon-run/holon/blob/main/docs/next-phase-direction.md).

## Non-goals

Holon is *not* focused on these right now:

- UI-first product pages or dashboards.
- A plugin marketplace or extension registry.
- Hidden automation that cannot be inspected as runtime state.
- Documentation that contradicts the repository runtime contracts.

<!-- INDEX:START -->

<!-- INDEX:END -->
