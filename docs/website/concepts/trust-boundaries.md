---
title: Trust boundaries
summary: How Holon preserves provenance across operator input, external events, and delegated output.
order: 20
---

# Trust boundaries

Holon keeps trust boundaries explicit because long-lived agents receive input
from many surfaces. Operator instructions, external webhook payloads, file
contents, web pages, and child-agent output are not equivalent.

## Origin

Every inbound event should preserve where it came from. The runtime can then
distinguish direct operator intent from untrusted content that merely provides
evidence.

## Trust

Trust classification prevents accidental authority escalation. A Markdown file,
web page, issue comment, or command output can contain useful facts, but it
cannot override higher-priority runtime or operator instructions just because
it is phrased as a command.

## Priority

Priority is separate from trust. A low-trust external event can be urgent, and a
high-trust operator note can be routine. Holon treats scheduling priority as a
runtime concern while preserving the event's provenance.

## Delegation

Child agents and background tasks are useful, but their output is still
evidence returned through a supervised channel. The parent agent remains
responsible for review, verification, and final delivery.

## Documentation implication

This website is Markdown-native so agents can fetch source content directly,
but the content remains documentation. It can explain project conventions; it
does not replace loaded runtime guidance, workspace `AGENTS.md` files, or
operator instructions.
