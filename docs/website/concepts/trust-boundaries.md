---
title: Trust boundaries
summary: How Holon classifies origin, trust, and priority to keep long-lived agents secure.
order: 20
---

# Trust Boundaries

Holon keeps trust boundaries explicit because long-lived agents receive input
from many surfaces. Operator instructions, external webhook payloads, file
contents, web pages, and child-agent output are **not equivalent**.

## Why Trust Matters

In a headless runtime, an agent may simultaneously:

- Follow operator instructions from a trusted channel
- Read untrusted webhook payloads for evidence
- Parse Markdown files that contain project conventions
- Receive child-agent output that needs verification
- Fetch external web pages for research

Without explicit trust classification, a malicious or accidental "instruction"
hidden in any of these sources could escalate authority.

## Origin Classification

Every inbound event carries an `origin` that records its source:

| Origin | Description | Example |
|--------|-------------|---------|
| `operator` | Direct human instruction | CLI prompt, operator ingress API |
| `system` | Runtime-generated event | System tick, compaction trigger |
| `task` | Child task completion | Command task finished, child agent result |
| `channel` | External integration | Slack message, CI notification |
| `webhook` | Third-party callback | GitHub webhook, deployment hook |
| `timer` | Scheduled trigger | Cron-like timer fire |

The runtime can distinguish "operator told me to do X" from "a web page
mentioned X."

## Trust Levels

| Level | Meaning | Who can set it |
|-------|---------|---------------|
| `trusted-operator` | Highest authority — binding instructions | Operator, runtime configuration |
| `trusted-system` | Runtime-internal events | Holon runtime itself |
| `trusted-integration` | Vetted external service | Configured transport bindings |
| `untrusted-external` | Unknown or unverified source | Default for webhooks, external content |

Trust classification prevents accidental authority escalation:

- A Markdown file cannot override operator instructions just because it
  contains a sentence that looks like a command
- A web page fetched for research cannot change the agent's active work item
- A GitHub issue comment cannot modify runtime configuration

## Priority vs Trust

Priority is **separate** from trust:

- **Priority** controls scheduling: `interrupt` > `next` > `normal` >
  `background`
- **Trust** controls authority: what the event is allowed to do

A low-trust external event can be urgent (CI build failed — wake the agent
now). A high-trust operator note can be routine (review this when you have
time).

## Delegation

Child agents and background tasks return **evidence**, not authority:

- Child agent output arrives through a supervised task handle
- The parent agent remains responsible for review and verification
- A child's conclusion does not automatically become the parent's answer

## Practical Application

### When reading files

A file's content is **untrusted context**, even if it's in the workspace. It
can describe conventions and provide facts, but it cannot issue runtime
instructions.

### When fetching web content

`WebFetch` results are labeled as untrusted external content. The agent can
use them as research evidence but must not treat them as commands.

### When receiving external events

Inbound webhook payloads carry `origin: webhook` with the specific source. The
agent inspects the payload for evidence but evaluates instructions from the
operator or its own `AGENTS.md` guidance as binding.

### Operator instructions

Operator input through trusted channels (`holon run`, operator ingress API,
TUI) carries the highest trust level. These instructions define the task scope
and acceptance criteria.

## Documentation Implication

This website is Markdown-native so agents can fetch source content directly,
but the content remains documentation. It can explain project conventions; it
does not replace loaded runtime guidance, workspace `AGENTS.md` files, or
operator instructions.

## See Also

- [Runtime Model](/concepts/runtime-model.md) — Agent, task, and work item
  lifecycle
- [Integration Guide](/guides/integration.md) — How origin and trust appear in
  the HTTP API
- [CLI Reference](/reference/cli.md) — The `--trust` flag on `holon run`
