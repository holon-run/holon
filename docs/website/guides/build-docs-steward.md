---
title: Build a local Docs Steward that keeps docs in sync with code
summary: Create a documentation hygiene agent from the docs-steward template, ask it in conversation to attach your repository, audit drift between code and docs, and submit cited, docs-only PRs in an isolated worktree.
order: 87
---

# Build a local Docs Steward that keeps docs in sync with code

This guide shows how to set up a long-lived documentation maintenance agent from the `docs-steward` template. After creating the agent in the Web GUI, you guide the entire workflow in plain conversation: ask the agent to attach your repository, inspect merged pull requests and CLI command updates for documentation drift, run local validation in an isolated worktree, and submit cited, docs-only PRs without touching product code.

- For reviewing code pull requests instead of documentation, see [Build a local Code Reviewer that follows PRs](./build-code-reviewer.md).
- To explore the template definition and bundled skills, see the [Docs Steward template](../templates/docs-steward/).
- To understand how to isolate agent skills across dedicated roles, read [Organize agent skills across dedicated roles](./organize-agent-skills-and-roles.md).

## Before you start

- A running Holon instance with the [Web GUI](./use-web-gui.md) open (default `http://localhost:8080`) and a [configured model](./configure-models.md).
- The machine running Holon has the GitHub CLI (`gh`) authenticated and your repository's doc build or link-check tooling installed.
- No need to run complex CLI commands manually: attaching workspaces, setting role boundaries in `AGENTS.md`, running diff checks, and creating isolated worktrees can all be delegated to the agent in conversation.

## Steps

### 1. Create the agent from the `docs-steward` template

In the Web GUI, click **New Agent**, choose the **Docs Steward** (`docs-steward`) template, and name your agent (for example, `docs-steward` or `doc-bot`).

The `docs-steward` template preconfigures the agent's role contract (`AGENTS.md`) and bundles the necessary skills:
- `ghx`: GitHub CLI integration for querying merged PRs, reading issues, and opening docs-only pull requests;
- `sview`: structured inspection for codebases, symbol tables, and document trees without full-tree file dumps;
- `agentinbox`: persistent event subscriptions for repository changes and PR events;
- `writing-clearly-and-concisely`, `humanizer`, and `humanizer-zh`: style guidelines for clear technical writing and bilingual synchronization;
- `mermaid-diagrams`: validated diagram generation for architecture and workflow docs.

### 2. Attach your repository and set boundary rules in conversation

Start a conversation with the newly created agent. Ask it to attach your repository and confirm its operational boundaries:

> "Please attach the local repository at `/path/to/my-repo` and confirm your operational boundaries: you may inspect source code and CLI `--help` flags to verify facts, but you must only edit documentation files (`docs/`, `README.md`). Never modify application code, tests, or configuration, and never merge pull requests autonomously."

The agent executes `AttachWorkspace` to bind your repository and records these instructions in its persistent `AGENTS.md`. This guarantees that:
- **Fact-first verification**: Claims must point to concrete code locations, CLI help outputs, or approved RFCs rather than hallucinated behavior.
- **Strict docs-only scope**: Edits remain confined to documentation files; the agent rejects requests to modify business logic to match outdated docs.
- **Human approval boundary**: The agent drafts and submits pull requests for maintainer review, but never merges them autonomously.

### 3. Run a first documentation drift audit

Ask the agent to check recent commits or CLI updates against the existing documentation:

> "Please inspect the last 5 merged pull requests on the `main` branch and the current output of `./my-app --help`. Identify any new flags, modified default parameters, or renamed subcommands that are missing from `docs/` or `README.md`. Output a checklist with file paths and line numbers."

The agent uses `ghx` and `sview` to compare recent code changes with the documentation. Instead of vague summaries, it produces a structured drift report:

```markdown
### Documentation Drift Audit

1. **New `--output-format` flag**:
   - Source: `src/cli/args.rs:48` (added in PR #142).
   - Missing in: `docs/reference/cli.md` (last updated for v1.2).
2. **Changed default timeout**:
   - Source: `src/config/defaults.rs:16` (default changed from 30s to 60s in PR #145).
   - Inconsistent in: `README.md:82` and `docs/guides/getting-started.md:34`.
```

### 4. Create an isolated worktree and submit a docs-only PR

Once you review the audit checklist, instruct the agent to prepare the documentation updates in an isolated Git worktree:

> "Create an isolated worktree on branch `docs/sync-cli-v1-3`, update `docs/reference/cli.md` and `README.md` to reflect these changes, verify local links and doc formatting, and submit a draft PR for review."

The agent:
1. Calls `CreateWorktree` to create a dedicated branch (e.g. `docs/sync-cli-v1-3`) so your working tree remains untouched;
2. Applies targeted patches with exact parameter descriptions and source references;
3. Runs the local documentation build or lint script (such as `npm run check:content` or `mkdocs build`);
4. Opens a pull request via `ghx` with a structured summary linking each change to the originating PR or code line.

You can inspect the generated PR directly on GitHub or in the Web GUI without leaving your current workspace.

### 5. Subscribe to repository PR events for continuous synchronization

To ensure the agent keeps documentation fresh over time, ask it to configure continuous event tracking:

> "Please set up an event subscription using `agentinbox` to track merged pull requests on this repository. When new PRs touching `src/` or `crates/` are merged, wake up and perform a doc drift check."

The agent subscribes to GitHub repository events. When a pull request merges:
- Holon receives the webhook event and wakes the `docs-steward` agent;
- The agent creates a tracking work item, checks whether user-facing contracts or flags changed, and prepares a draft PR or notification brief if updates are needed;
- Between events, the agent stays idle with zero token consumption.

## Verification

- **Workspace boundary**: In the Web GUI, check the agent's active workspace and verify that no application source files (`.rs`, `.go`, `.ts`) have uncommitted modifications.
- **Evidence audit**: Verify that every item in the agent's pull request body links directly to a commit SHA or file line in the target codebase.
- **Clean worktree lifecycle**: Verify that temporary branches and worktrees are safely removed after pull requests are merged.

## If it does not work

- **`gh` authentication failed**: If the agent reports `gh: not logged in`, run `gh auth login` on the host machine running Holon, then ask the agent to retry.
- **Incomplete drift detection**: Ensure the target repository is fully fetched (`git fetch --all`). If recent commits are shallow, ask the agent to run a fetch in its workspace.
- **Formatting or link check errors**: If doc verification fails, ask the agent: *"Inspect the build error log and fix any broken links or broken markdown headers in your branch before submitting the PR."*

## Next steps

- Read [Work with workspaces and Git worktrees](./work-with-workspaces.md) to learn how Holon isolates concurrent agent worktrees.
- Read [Connect external events and webhooks](./connect-external-events.md) to learn more about Holon's event-driven scheduling.
- Read [Manage agent memory](./manage-agent-memory.md) to customize long-term style rules and terminology glossaries for your docs steward.
