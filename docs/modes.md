# Holon Skill-First Architecture

Holon uses a **skill-first** execution model where context collection and publishing are delegated to skills, not the runtime. This provides flexibility and control over the IO workflow.

## Architecture Overview

### Skill-First IO (Default)
- **Context collection**: Skills (e.g., `github-issue-solve`) are responsible for collecting their own context via tools like `gh` CLI
- **Publishing**: Skills handle publishing directly (e.g., creating PRs, posting comments)
- **Runtime role**: Validates generic contracts (`manifest.json` with `status` and `outcome` fields) without skill-specific logic

### Built-in Skills
- `github-issue-solve`: Solves GitHub issues end-to-end (collect context → implement solution → create PR)
- `github-pr-fix`: Fixes PR review comments (collect PR context → implement fixes → post replies)
- `github-review`: Reviews PRs (collect PR context → analyze code → publish review findings)

## CLI Behavior

### `holon solve <ref>` (recommended)
- Auto-detects reference type (issue vs PR)
- Loads default skill based on reference type:
  - Issue reference → `github-issue-solve`
  - PR reference → `github-pr-fix`
- Skills are loaded from project config (`--skill` flag for overrides)

### `holon run`
- Lower-level entrypoint for running with custom goals/specs
- Always uses skill-first mode
- Skills loaded from project config or `--skill` flag

## Publishing Semantics

### Skill-Driven Publishing
- Skills are responsible for all publishing operations
- Runtime validates success via `manifest.json`:
  - `status: "completed"` - execution finished
  - `outcome: "success"` - execution succeeded
- For issue-solve skills, runtime checks for PR evidence in `manifest.json`:
  - `metadata.pr_number` - created PR number
  - `metadata.pr_url` - created PR URL

### Generic Contract Validation
The runtime validates outputs based on the generic manifest contract, not skill-specific artifacts:
- `manifest.json` (required) - execution status and outcome
- `summary.md` (optional) - human-readable summary
- `diff.patch` (optional) - code changes (applied by skill if needed)

## Workspace and Safety
- All executions run in Docker with a snapshot workspace by default
- Use `--workspace` to point at an existing checkout; otherwise, Holon clones to a temp dir
- Output goes to a temp directory unless `--output` is set
- Artifacts are validated before completion

## Configuration

### Project Config (`/.holon/config.yaml`)
```yaml
skills:
  - github-issue-solve  # Default skill for issues
  - github-pr-fix       # Default skill for PRs
base_image: auto-detect
```

### CLI Flags
- `--skill <path>`: Override skill (repeatable)
- `--skills <list>`: Comma-separated skill paths
- `--workspace <path>`: Use existing workspace
- `--output <dir>`: Output directory

