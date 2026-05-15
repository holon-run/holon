---
title: Agent templates
summary: What agent templates are, available built-in templates, how to use --template, and how to create custom templates.
order: 18
---

# Agent Templates

An agent template is a reusable bootstrap that initializes a new agent's
`AGENTS.md` role contract and optional pre-installed skills. Templates give new
agents a known starting point without manual setup.

## When to Use Templates

Use `--template` when you want a new agent to start with a specific role and
capabilities. Without a template, agents start with a generic default contract.

Common scenarios:

- **Creating a reviewer agent** — `holon agent create reviewer --template holon-reviewer`
- **One-shot tasks with a role** — `holon run --template holon-developer "Fix the null check in handler.rs"`
- **Solving GitHub issues** — `holon solve --template holon-github-solve https://github.com/owner/repo/issues/42`

## Built-in Templates

Holon ships with these built-in templates under `builtin_templates/`:

| Template ID | Purpose | Includes Skills |
|-------------|---------|----------------|
| `holon-default` | Generic agent with fill-in-the-blank role contract | None |
| `holon-developer` | Implementation-focused agent for code changes | None |
| `holon-reviewer` | Review-focused agent for PR inspection | None |
| `holon-release` | Release and delivery agent for versioning/publishing | None |
| `holon-github-solve` | GitHub task agent for issues and PRs | `ghx`, `github-issue-solve`, `github-pr-fix`, `github-review` |

### `holon-default`

The default template. Provides a fill-in-the-blank `AGENTS.md` structure with
sections for Role, Responsibilities, Authority, Escalation Boundary, and
Operating Conventions. Use this as a starting point for custom roles.

### `holon-developer`

Pre-configured for implementation work:

- Turn requirements into concrete code changes
- Run minimal verification that meaningfully checks the change
- Keep edits narrow, explicit, and easy to review
- Report blockers with concrete technical evidence

### `holon-reviewer`

Pre-configured for code review:

- Inspect pull requests for correctness, regressions, and contract drift
- Prioritize concrete findings with clear severity and file references
- Distinguish proven issues from open questions

### `holon-release`

Pre-configured for release work:

- Prepare release changesets, version bumps, and release notes
- Verify release prerequisites before publishing
- Surface irreversible steps before executing them

### `holon-github-solve`

The most feature-rich template. Pre-configures the agent for GitHub workflows
and pre-installs four skills for issue solving, PR fixing, code review, and
GitHub CLI operations.

## Using `--template`

### Create an Agent

```bash
holon agent create reviewer --template holon-reviewer
```

This initializes `~/.holon/agents/reviewer/AGENTS.md` with the reviewer role
contract. If the agent home already exists and is non-empty, template
initialization refuses to overwrite it.

### One-Shot Run

```bash
holon run --template holon-developer "Fix the null check in handler.rs"
```

The agent is created with the developer role contract, executes the prompt,
and is cleaned up after completion.

### Solve a GitHub Issue

```bash
holon solve --template holon-github-solve https://github.com/owner/repo/issues/42
```

The agent starts with GitHub workflow guidance and the four GitHub skills
pre-installed.

## Template Structure

A template consists of a directory containing:

```
my-template/
├── AGENTS.md       # Required — the agent role contract
└── skills.json     # Optional — skill references to pre-install
```

### `AGENTS.md`

The agent's role contract. This is the same format as any agent's `AGENTS.md`.
The runtime appends the standard [Agent Home](#agent-home-section) guidance
automatically, so your template only needs to define the role-specific content.

### `skills.json`

An optional manifest that lists skills to pre-install when the agent is created:

```json
{
  "skill_refs": [
    { "kind": "builtin", "name": "github-issue-solve" },
    { "kind": "builtin", "name": "github-pr-fix" },
    { "kind": "local", "path": "/path/to/custom-skill" },
    { "kind": "github", "package": "owner/repo/skill-name" }
  ]
}
```

Three skill reference kinds are supported:

- **`builtin`** — A skill shipped with Holon (e.g. `ghx`, `github-issue-solve`)
- **`local`** — An absolute path to a skill directory on disk
- **`github`** — A GitHub package reference

## Creating Custom Templates

Create a directory with an `AGENTS.md` and optional `skills.json`, then use the
absolute path as the template selector:

```bash
holon agent create my-agent --template /path/to/my-template
```

You can also host templates on GitHub and reference them by URL:

```bash
holon agent create my-agent --template https://github.com/owner/repo/tree/main/templates/my-template
```

Templates referenced by absolute path or GitHub URL record provenance in the
agent home (`template-provenance.json`), so you can trace back where the
agent's contract came from.

## Templates vs Skills

Templates and skills serve different purposes:

| Feature | Template | Skill |
|---------|----------|-------|
| What it provides | Agent identity and role contract | Reusable task workflow |
| When applied | At agent creation time | Loaded on demand during a task |
| Persistence | Permanent in agent home | Available as long as installed |
| Example | "You are a reviewer" | "Here's how to review a PR" |

Templates often include skill references so that new agents have the right
tools available from the start. See the [Skills guide](/guides/skills/) for
details on skills.
