# Holon Default Agent

This file is the long-lived role contract for the default Holon agent.

Do not repeat:

- system prompt rules
- tool usage instructions
- workspace or project `AGENTS.md` content
- one-off task details

Use this file to capture only durable, agent-specific guidance that should stay
true across turns.

## Role

Fill in:

- what long-lived role this agent serves
- which repos, systems, or workstreams it is responsible for
- what kind of outcomes it is expected to own

## Responsibilities

Fill in:

- the standing responsibilities this agent should carry without being reminded
- the kinds of tasks it should usually handle directly
- the kinds of tasks it should avoid or hand off

## Authority

Fill in:

- what this agent is allowed to do without asking again each time
- what actions still require operator confirmation
- any review, merge, release, or maintenance permissions it has

## Escalation Boundary

Fill in:

- which risks, ambiguities, or irreversible actions must be escalated
- when the agent should stop and ask the operator instead of proceeding

## Operating Conventions

Fill in:

- any durable working style specific to this agent's role
- recurring checklists or decision rules that belong to this agent
- conventions that should persist across sessions

## Agent Home Maintenance

Use `agent_home` for durable agent-local state only.

### Suggested Layout

- `notes/`
  - durable agent-specific notes, background, and role details
- `scripts/`
  - reusable scripts this agent may rely on across multiple tasks
- `refs/`
  - stable references, indexes, and long-lived lookup material
- `state/`
  - light local state or indexes this agent maintains for itself
- `tmp/`
  - short-lived working files and intermediate output that may be cleaned up

Keep here:

- this file
- durable notes about the agent's role or authority
- stable local indexes, references, or maintenance records for this agent

Do not keep here:

- temporary task plans
- short-lived execution notes
- duplicated project documentation
- copies of system or workspace instructions

Only keep content here when it remains useful across tasks or sessions.
