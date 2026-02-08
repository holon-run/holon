# Project Pulse Planning Playbook

Use this playbook to turn `report.json` into project planning and architecture decisions.

## 1. Triage Order

1. CI and review blockers
- If `failing_prs` is non-empty: fix these first.
- If `pr_totals.changes_requested > 0`: clear review feedback before opening more new work.

2. Delivery critical path
- Process `priority:critical` issues as the top queue.
- Keep critical queue short and reviewed daily.

3. Flow health
- Review `stale_issues` and `stalled_prs`.
- Decide for each: close, re-prioritize, split, or assign an explicit owner.

## 2. Parallelization Rules

Run in parallel only when workstreams are independent:
- Lane A: feature delivery (`github-issue-solve`)
- Lane B: PR stabilization (`github-pr-fix`)
- Lane C: quality/security hardening

Do not parallelize two streams that mutate the same PR branch or same high-risk subsystem.

## 3. Suggested Next Actions Template

Based on report signals, generate a compact action list:

- `action`: one of `solve_issue`, `fix_pr`, `review_pr`, `wait`
- `target`: `owner/repo#number`
- `reason`: one-sentence rationale from pulse signal
- `priority`: `critical|high|medium|low`

Example:

```json
[
  {
    "action": "fix_pr",
    "target": "holon-run/holon#585",
    "reason": "Open PR has failing checks and blocks release flow",
    "priority": "critical"
  }
]
```

## 4. Architecture Lens Checklist

Before promoting work to execution, check:
- Does this reduce or increase path fragmentation?
- Does this introduce a second source of truth?
- Does this preserve deterministic publish/sync behavior?
- Can this change be split into an independent milestone?

If answers are unclear, keep action as `wait` and request clarification.
