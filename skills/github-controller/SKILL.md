---
name: github-controller
description: Controller skill for proactive GitHub automation. Given a normalized event in /holon/input/context/event.json, decide whether to run issue solve, PR review, PR fix, or no-op, then execute using existing GitHub skills.
---

# GitHub Controller Skill

Use this skill when running in proactive `holon serve` mode.

## Purpose

You are the decision layer. For each incoming GitHub event, decide if action is needed, then execute the action with existing skills.

Input is provided in:

- `/holon/input/context/event.json`: normalized event envelope

## Required behavior

1. Read and parse `/holon/input/context/event.json`.
2. Decide one of:
   - `no-op`
   - `issue-solve`
   - `pr-review`
   - `pr-fix`
3. If action is selected, invoke exactly one of these skills:
   - `github-issue-solve`
   - `github-review`
   - `github-pr-fix`
4. Include a short rationale in `summary.md`:
   - event type
   - decision
   - executed skill or why skipped

## Decision guidance

- For issue events (`github.issue.*` or `github.issue.comment.*`), choose `issue-solve` when implementation work is requested.
- For PR opened/reopened/synchronize, prefer `pr-review`.
- For PR review changes requested or explicit fix requests in comments, prefer `pr-fix`.
- If event is irrelevant, duplicated, or lacks enough context, choose `no-op`.

Do not hard fail only because action is not required. `no-op` is a valid result.

