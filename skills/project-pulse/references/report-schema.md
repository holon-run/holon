# Project Pulse Report Schema

`report.json` contains:

- `metadata`
- `totals`
- `pr_totals`
- `stale_issues`
- `stalled_prs`
- `failing_prs`
- `ready_to_merge_candidates`
- `next_actions`
- `recommendations`

## metadata

- `repo`: `owner/repo`
- `source_of_truth`: always `github`
- `issue_state`: `open` or `all`
- `pr_state`: `open` or `all`
- `fetched_at`: UTC timestamp
- `previous_max_updated`: previous cursor timestamp
- `current_max_updated`: latest `updatedAt` in fetched dataset
- `updated_since_last`: count of issues updated after previous cursor

## totals

- `issues`
- `open`
- `closed`
- `priority_critical`
- `priority_high`
- `priority_medium`
- `priority_low`

## pr_totals

- `prs`
- `open`
- `merged`
- `closed`
- `drafts`
- `failing_checks`
- `changes_requested`

## stale_issues

Array of open issues older than `PULSE_STALE_DAYS`:

- `number`
- `title`
- `updatedAt`
- `url`
- `age_days`
- `priority`

## stalled_prs

Array of open PRs older than `PULSE_STALE_DAYS`:

- `number`
- `title`
- `updatedAt`
- `url`
- `age_days`
- `merge_state`
- `review_decision`

## failing_prs

Array of open PRs with failed status checks:

- `number`
- `title`
- `updatedAt`
- `url`
- `merge_state`

## ready_to_merge_candidates

Array of open PRs that are:

- non-draft
- approved (`reviewDecision=APPROVED`)
- merge-clean (`mergeStateStatus=CLEAN`)
- no failing checks

Fields:

- `number`
- `title`
- `updatedAt`
- `url`

## next_actions

Structured action intents for controller consumption.

Each item contains:

- `action`: `solve_issue` | `fix_pr` | `wait`
- `target`: `owner/repo#number` or `owner/repo`
- `reason`: concise rationale
- `priority`: `critical|high|medium|low`

## recommendations

Array of short action suggestions for controller/PM usage.
