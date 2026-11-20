---
title: Run or follow GitHub issues and PR fixes with Holon
summary: Choose between a conversational, worktree-isolated workflow in the Web GUI for multi-round issues and PRs or one-shot holon solve runs from CLI and CI, with explicit preflight checks, publication gates, and cleanup.
order: 90
---

# Run or follow GitHub issues and PR fixes with Holon

Use this guide to run a single-pass GitHub task or manage multi-round issue and
pull-request work with explicit scope, repository isolation, and publication
boundaries. A completed task never means a pull request was merged automatically.

## Choose the right workflow

| Need | Recommended path | Why |
|---|---|---|
| Multi-round PR feedback, CI waits, or ongoing follow-up | **Web GUI + long-lived agent** | Keeps `WorkItem` state, isolates work in linked `git worktree`s, and waits on CI/reviews without polling |
| Dedicated automated PR review lifecycle | [Build a local Code Reviewer](build-code-reviewer.md) | Uses the `code-reviewer` contract and event subscriptions end-to-end |
| Bounded one-shot CLI run or CI job | **`holon solve`** | Runs the `github-solver` preset once and writes `run.json`, `summary.md`, and `manifest.json`, then exits |

## Prerequisites

- A running Holon runtime with a working model route ([Configure models](configure-models.md)).
- Authenticated GitHub access (`gh` logged in, or `GITHUB_TOKEN` / `GH_TOKEN` scoped strictly to the needed read or PR actions—never log tokens).
- A clean local repository checkout (or path ready to attach). Resolve conflicting uncommitted edits before authorizing changes.

## Path A: Conversational issue & PR workflow in the Web GUI

For interactive fixes, multi-round review comments, or tasks that wait on CI,
drive the workflow directly in the [Web GUI](use-web-gui.md) instead of running
a chain of CLI diagnostic commands up front.

### 1. Run preflight checks and isolate a worktree in conversation

Open your agent (for example `main`, or a dedicated agent created from
`software-developer` or `code-reviewer`) and give it one scoped instruction:

> "Attach `/path/to/my-repo` as a workspace. Before making changes, verify your model route and `gh auth status`, inspect issue `#42` (or PR `#123`), and create a dedicated linked GitHub worktree branch (for example `fix/issue-42`) from `main`. Keep your changes scoped to this issue, run the affected tests, and pause before pushing or opening a PR—or open a draft PR without merging."

The agent attaches the workspace, confirms GitHub access, creates an isolated
`git worktree` via `CreateWorktree` (leaving your main checkout clean), and
records any standing boundaries in its `AGENTS.md`. See
[Work with workspaces and Git worktrees](work-with-workspaces.md).

### 2. Follow CI, review rounds, and cleanup

After tests pass and you approve opening or updating the PR:

- **Track review & CI across turns:** Ask the agent to track the PR via a
  [long-lived WorkItem](run-long-lived-task.md) or connect GitHub events via
  [Agent Inbox](connect-event-inbox.md) so it wakes only when reviews or CI
  checks arrive.
- **Verify deliverables:** Check the PR URL, head commit SHA, and local test
  output reported in the brief—do not rely on summary prose alone.
- **Clean up after merge:** Once the PR merges, ask the agent to verify the
  branch is merged and safely remove the linked worktree (`RemoveWorktree`).

## Path B: One-shot CLI & CI tasks with `holon solve`

Use `holon solve` when you want a single headless pass from the terminal or CI.
Before running it, prepare your checkout (`git status --short --branch`) and
confirm `holon config doctor` and `gh auth status` pass. Run from the checkout
or pass `--workspace`: `solve` expects an existing local checkout and does not
promise to clone one.

## Target reference formats

Use a full issue or PR URL to make the target kind explicit. These shorthand
forms are also accepted:

```text
owner/repo#42
#42 --repo owner/repo
```

Quote a leading `#` in a shell. Replace example repository names and numbers
with your actual target.

## Quick start

### Solve an issue

```bash
holon solve 'owner/repo#42' --workspace "$PWD" \
  --goal "Implement issue 42, run the relevant tests, and open a PR. Do not merge." \
  --output ./solve-results/issue-42 --json
```

This goal authorizes repository changes and PR publication. For a read-only
first pass, use a goal that explicitly requests investigation only. Inspect
`summary.md`, the diff, and test evidence before accepting an implementation.

### Provide a custom goal

```bash
holon solve '#42' --repo owner/repo --workspace "$PWD" \
  --goal "Investigate the issue and propose a fix. Do not edit, commit, push, or publish." \
  --output ./solve-results/investigation-42 --json
```

A goal should state scope, acceptance checks, and permitted side effects. The
standard solve workflow can commit, push, and create or update a PR when code
changes are required; do not rely on an omitted publication instruction to mean
read-only.

### Review a pull request

```bash
holon solve https://github.com/owner/repo/pull/123 --workspace "$PWD" \
  --goal "Review only: check correctness and test coverage. Do not edit files. Publish one review or PR comment, then stop. Do not approve or merge." \
  --output ./solve-results/review-123 --json
```

Review guardrails tell the agent to check the current head and existing reviews
before publishing, use one publish surface, and verify an ambiguous result
before retrying. Still inspect the resulting review URL and head SHA. If a run
was interrupted after publication, check GitHub before running it again.

### Fix a PR from review feedback

```bash
holon solve https://github.com/owner/repo/pull/123 --workspace "$PWD" \
  --goal "Address the actionable review feedback, run the affected tests, and update this PR. Report each addressed item and any blocker. Do not merge." \
  --output ./solve-results/fix-123 --json
```

Use `--base BRANCH` when the agent needs a base-branch hint. The flag does not
replace checking the PR's actual base and head.

## How solve works

The runtime parses the target, prepares input/output directories, selects the
configured solve template (normally `github-solver`), and runs the scoped prompt.
Use a distinct output directory per run to keep prior artifacts from being
mistaken for new results.

After a run returns, inspect:

| File | What to check |
|---|---|
| `run.json` | Structured run result, including `final_status` |
| `summary.md` | Changes, verification, publication URLs, and remaining blockers |
| `manifest.json` | Outcome metadata and available artifact references |

Early setup failures may leave no complete artifact set. Keep stderr and the
command exit status. The [CLI reference](../reference/cli.md) lists all flags;
this guide does not redefine the output schema.

## Differences from holon run

Use `holon run` for a general headless prompt. Use `holon solve` when the task
starts from a GitHub issue or PR and needs the solve preset's GitHub workflows.
Neither a successful process exit nor a short summary replaces checking the
requested acceptance criteria.

## Scripting with solve

### JSON output for scripting

Capture the process status before inspecting `final_status`. Avoid piping the
command straight into `jq` without preserving its exit code.

### Custom output directory

```bash
holon solve 'owner/repo#42' --workspace "$PWD" \
  --goal "Investigate only; do not edit or publish." \
  --output ./solve-results/inspect-42 --json
cat ./solve-results/inspect-42/summary.md
```

### CI integration sketch

This Bash example stops on an execution error or incomplete run. It does not
push whatever branch happens to be checked out:

```bash
set -euo pipefail
: "${TARGET_REF:?Set the issue or PR reference}"
OUTPUT=$(mktemp -d)

if holon solve "$TARGET_REF" --workspace "$PWD" \
  --goal "Investigate only; do not edit, commit, push, or publish." \
  --output "$OUTPUT" --json >"$OUTPUT/stdout.json"; then
  jq -e '.final_status == "completed"' "$OUTPUT/run.json" >/dev/null
  cat "$OUTPUT/summary.md"
else
  status=$?
  printf 'Solve failed; inspect artifacts in %s\n' "$OUTPUT" >&2
  exit "$status"
fi
```

Archive that directory as a CI artifact even on failure, after checking it for
secrets. Add task-specific acceptance checks; `final_status=completed` only
establishes that the runtime run completed.

### GitHub Actions

The repository's composite action accepts a target `ref` or `trigger: auto` for
supported events. Review the checked-in `action.yml` and your workflow's token
permissions before enabling publication. Keep credentials in the workflow's
secret mechanism. Pin the action revision according to your repository policy.

Do not automatically rerun a publication-capable job after a timeout: first
check whether it already created a commit, PR, review, or comment.

## If the task does not complete

Check model availability, GitHub authentication, the selected checkout, and the
run's stderr. If the agent reached a turn or time limit, inspect artifacts and
external side effects before deciding whether to resume or start another run.
Report an unresolved blocker rather than claiming the issue is fixed.

## See also

- [Build a local Code Reviewer that follows PRs](build-code-reviewer.md)
- [Work with workspaces and Git worktrees](work-with-workspaces.md)
- [Connect external events with Agent Inbox](connect-event-inbox.md)
- [Run a long-lived task](run-long-lived-task.md)
- [CLI reference](../reference/cli.md)
- [Troubleshooting](troubleshooting.md)
