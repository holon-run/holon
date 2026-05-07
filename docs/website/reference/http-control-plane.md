---
title: HTTP control plane
summary: How to think about Holon's headless integration surface.
order: 20
---

# HTTP control plane

Holon is designed to be headless. HTTP and event-driven integration surfaces
should preserve the same runtime concepts as the CLI: origin, trust, priority,
work items, tasks, queues, wakeups, and user-facing delivery.

## Design goals

- Keep transport details outside the core runtime model.
- Preserve provenance for inbound messages and external events.
- Return structured lifecycle state rather than only streaming text.
- Make wake, sleep, enqueue, and task supervision visible to integrations.
- Keep user-facing output separate from internal traces.

## Integration posture

Treat the HTTP surface as a control plane for runtime state, not a chat-only
endpoint. A good integration should be able to ask:

- What work is active?
- Which tasks are running or waiting?
- What event woke the agent?
- Which output is safe to show to a user?
- Which evidence is internal runtime detail?

The exact API shape should follow the repository runtime specs as they mature.
