# Holon Skill-First Architecture

Holon uses a **skill-first** execution model where context collection and publishing are delegated to skills, not the runtime. This provides flexibility and control over the IO workflow.

For release-level compatibility guarantees of `holon run`, see `docs/run-ga-contract.md`.

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
- Skills are resolved and merged with precedence:
  - CLI (`--skill`, `--skills`)
  - Project config
  - Spec metadata
  - Auto-discovered workspace skills
- `--skill`/`--skills` are activation inputs for the current run, not install commands

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
- All executions run in Docker. `run` and `solve --workspace` use the provided workspace directly by default.
- `solve` without `--workspace` prepares an isolated temporary workspace (for example, by cloning the repository).
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
- `--skill <path>`: Add/override active skills for this run (repeatable, highest precedence)
- `--skills <list>`: Comma-separated active skills for this run (highest precedence)
- `--workspace <path>`: Use existing workspace
- `--output <dir>`: Output directory
