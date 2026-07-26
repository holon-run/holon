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

Agent and workspace discovery search these roots in declaration order:

- `skills`
- `.agents/skills`
- `.codex/skills`
- `.claude/skills`

User-global discovery remains limited to the three hidden compatibility roots
and does not search a bare `~/skills` directory.

Discovery searches agent scope and workspace scope separately. Within one
scope, all physically distinct roots are merged. If multiple configured paths
resolve to the same physical directory, discovery keeps the first path in the
order above and scans that directory only once.

## Root Selection

### Agent Scope

Agent-scoped discovery is rooted under `agent_home`.

### Workspace Scope

Workspace-scoped discovery is rooted under `workspace_anchor`.

Skill discovery should not drift with shell cwd.

## Catalog Precedence

When multiple scopes provide a skill with the same declared name, the catalog
keeps the highest-precedence entry:

1. workspace
2. agent
3. user-global

This precedence only selects the effective skill catalog entry. Skill content
does not gain authority over system, developer, or operator instructions.

## Visibility Rules

- agent scope may expose user-level skill libraries to the default agent
- workspace scope may expose project-local skills
- discovery does not imply activation

## Activation Model

Phase-1 activation should be explicit and inspectable. Discovery alone only
means a skill's catalog metadata is available to prompt context; it must not
emit activation events or mark the skill active.

Holon records one user-visible activation event, `skill_activated`, when a
discovered skill is touched in a way that loads or uses the skill:

- which skill ids were activated
- which skill name was activated
- which path triggered activation
- which `SKILL.md` entrypoint the activated skill came from
- whether activation came from agent scope or workspace scope
- why the skill was loaded (`read_skill_md`, `run_skill_script`, or the
  reserved `prompt_injection`)
- the turn index and current run id when available

In the event payload, `path` is the triggering path. For
`load_reason=read_skill_md`, it is the discovered `SKILL.md`. For
`load_reason=run_skill_script`, it is the matching `scripts/*` path when the
runtime can identify one, otherwise the skill's `scripts` directory.
`entrypoint_path` always points at the skill's `SKILL.md`.

The v1 runtime monitors:

- tool reads of a discovered skill's `SKILL.md`
- successful shell commands that reference a discovered skill's `SKILL.md`
- successful shell command-batch items that completed and reference a
  discovered skill's `SKILL.md`
- successful shell commands, including completed command-batch items, that reference
  `scripts/*` under a discovered skill root

Successful turn completion promotes current `turn_active` skills to
`session_active` in state only; it does not emit a second user-visible
promotion event.

## Prompt And Resume Behavior

Activated skills should contribute guidance through normal prompt assembly and
should survive compaction and resume as durable activation records.

## Related Historical Notes

Supersedes and absorbs:

- `docs/archive/skill-discovery-and-activation.md`
