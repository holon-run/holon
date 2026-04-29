---
title: RFC: Skill Discovery and Activation
date: 2026-04-21
status: accepted
issue:
  - 63
---

# RFC: Skill Discovery and Activation

## Summary

Holon should keep skill discovery separate from skill activation.

Phase-1 skill behavior should be:

- file-based and inspectable
- rooted in stable agent and workspace locations
- explicitly activated rather than auto-selected
- preserved across compaction and resume

## Discovery Roots

Phase-1 compatibility roots should remain:

- `.agents/skills`
- `.codex/skills`
- `.claude/skills`

Discovery should search agent scope and workspace scope separately.

## Root Selection

### Agent Scope

Agent-scoped discovery is rooted under `agent_home`.

### Workspace Scope

Workspace-scoped discovery is rooted under `workspace_anchor`.

Skill discovery should not drift with shell cwd.

## Visibility Rules

- agent scope may expose user-level skill libraries to the default agent
- workspace scope may expose project-local skills
- discovery does not imply activation

## Activation Model

Phase-1 activation should be explicit and inspectable. Holon should record:

- which skill ids were activated
- which source path each one came from
- whether activation came from agent scope or workspace scope

## Prompt And Resume Behavior

Activated skills should contribute guidance through normal prompt assembly and
should survive compaction and resume as durable activation records.

## Related Historical Notes

Supersedes and absorbs:

- `docs/archive/skill-discovery-and-activation.md`
