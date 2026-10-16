---
title: Run a GitHub task with holon solve
summary: Scope an issue, review, or PR fix; run it once; then verify its artifacts and GitHub side effects.
order: 90
---

# Run a GitHub task with holon solve

`holon solve` runs a one-shot agent task against a GitHub issue or pull request.
Choose the goal and publication boundary before starting, then check both the
local output and any GitHub changes. A completed run does not mean a PR merged.

## When to use solve

Use it to implement an issue, review a PR, or address review feedback. For an
ongoing review cycle or CI watch, use a
[long-lived WorkItem](run-long-lived-task.md) with an explicit event subscription.
The solve preset does not keep monitoring a PR after its run ends.

## Configuration

Before starting:

```bash
holon config doctor
holon config models list
gh auth status
```

Configure a usable model and credentials through the
[model setup guide](configure-models.md). Make `GITHUB_TOKEN` or `GH_TOKEN`
available securely when the task needs GitHub access. Confirm that it has only
the permissions required for the requested read or publication operation.
Never echo the token into logs.

Prepare the repository checkout yourself and inspect its current branch and
working tree:

```bash
git status --short --branch
```

Run from the intended checkout or pass its path with `--workspace`. The solve
prompt expects a checkout prepared by the caller; it does not promise to clone
one. Resolve conflicting local changes before authorizing edits.

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

- [CLI reference](../reference/cli.md)
- [Run a long-lived task](run-long-lived-task.md)
- [Troubleshooting](troubleshooting.md)
