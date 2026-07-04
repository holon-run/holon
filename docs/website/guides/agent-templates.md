---
title: Agent templates
summary: What agent templates are, how to sync or install them, how to use --template, and how to create custom templates.
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

- **Creating a synced reviewer agent** — `holon agent create reviewer --template holon-reviewer`
- **One-shot tasks with a role** — `holon run --template holon-developer "Fix the null check in handler.rs"`
- **Solving GitHub issues** — `holon solve --template holon-github-solve https://github.com/owner/repo/issues/42`

## Template library and default bootstrap

Holon keeps visible templates in the user template library:

```text
~/.agents/agent_templates/
  .registry.json
  <install_id>/
```

User-authored templates, explicit installs, and remote-source sync results all
use this same root. Remote-source sync is equivalent to a batch install/update
of managed templates into that library. Holon writes `.registry.json` metadata
in the root to track synced remote sources, installed template mappings, and
content hashes.

Template IDs stay local to their source. If a synced remote template conflicts
with an existing local directory, Holon keeps the remote `template_id` in
metadata and installs it under a deterministic local `install_id`, such as
`worker@official`. Re-syncs reuse the recorded install id. If a managed template
has local edits, sync refuses to overwrite it until the operator resolves the
dirty copy.

Holon also carries one hidden built-in `holon-default` template for zero-config
and offline startup. It is not seeded into `~/.agents/agent_templates`, and it
is not shown as a catalog entry. It is used only when creating an agent without
an explicit template selector.

The official template source is the Holon repository. When synced, templates
under its top-level `agent_templates/` directory become normal local catalog
entries from `~/.agents/agent_templates`.

## Using `--template`

### Create an Agent

```bash
holon agent create reviewer --template holon-reviewer
```

This initializes `~/.holon/agents/reviewer/AGENTS.md` from the local
`holon-reviewer` template after that template has been installed or synced. If
the agent home already exists and is non-empty, template initialization refuses
to overwrite it.

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
├── template.toml   # Optional — display metadata and compatibility
└── skills.toml     # Optional — skill references to pre-install
```

### `AGENTS.md`

The agent's role contract. This is the same format as any agent's `AGENTS.md`.
The runtime appends the standard Agent Home guidance
automatically, so your template only needs to define the role-specific content.

### `template.toml`

An optional manifest for template metadata such as display name, summary,
schema, and compatibility. Synced remote templates use it for catalog metadata;
path-based local templates can omit it and fall back to directory/AGENTS.md
metadata.

### `skills.toml`

An optional manifest that lists skills to pre-install when the agent is created:

```toml
[[skills]]
kind = "builtin"
name = "github-issue-solve"

[[skills]]
kind = "builtin"
name = "github-pr-fix"

[[skills]]
kind = "github"
package = "owner/skills@custom-skill"

[[skills]]
kind = "local"
path = "/path/to/custom-skill"
```

Three skill reference kinds are supported:

- **`builtin`** — A skill shipped with Holon (e.g. `ghx`, `github-issue-solve`)
- **`github`** — A skill fetched from a GitHub package reference
- **`local`** — An absolute path to a skill directory on disk

## Creating Custom Templates

Create a directory with an `AGENTS.md`, optional `template.toml`, and optional
`skills.toml`, then use the absolute path as the template selector:

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
tools available from the start. See the [Skills guide](/guides/skills) for
details on skills.
