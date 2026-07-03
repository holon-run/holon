---
title: RFC: Agent Initialization and Template
date: 2026-04-22
updated: 2026-07-02
status: accepted
issue:
  - 367
---

# RFC: Agent Initialization and Template

## Summary

Holon should define agent initialization as a first-class runtime contract.

The core model is:

- every agent has its own `agent_home`
- `agent_home` is the agent's identity root, not just a storage directory
- every agent may have its own agent-scoped `AGENTS.md`
- every agent may have agent-local skills
- agent creation should prefer a reusable template over passing large inline
  instruction blobs
- a template initializes an agent, but does not remain the live source of truth
- agent templates should be packaged, discovered, and managed with conventions
  parallel to skills

After initialization, the effective source of truth for the agent's role and
long-lived behavioral guidance is the agent's own `agent_home/AGENTS.md`, which
may evolve over time.

## Why

Holon already defines:

- where agent-scoped and workspace-scoped instructions load from
- where user, agent, and workspace skill catalogs are discovered

But it does not yet define the missing step in between:

- when a new agent is created, what initial identity materials does it get
- where role-specific guidance should live
- where agent-specific skills should live
- how a long-lived named agent differs from a temporary child agent at
  initialization time

Without this contract, several needs are awkward or unstable:

- role-specific agents such as reviewer, developer, or release assistants
- agent-specific behavioral constraints that do not belong in workspace
  instructions
- agent-specific skills that should not be global or project-wide
- spawn flows that would otherwise need to pass large instruction payloads

## Scope

This RFC defines:

- the initialization contract for any new agent
- the role of `agent_home` in that contract
- how agent-scoped `AGENTS.md` should be initialized
- how agent-local skills should be initialized
- the role of templates in agent creation
- how initialized guidance may evolve after agent creation
- the target package shape for agent templates
- the local and remote discovery layout for template libraries
- the daemon API capabilities needed before a GUI management surface can be
  built

This RFC does not define:

- runtime-enforced permission or policy fields
- a full structured authorization system
- the exact operator UX for editing templates or `AGENTS.md`
- hierarchical `AGENTS.md` loading inside the workspace tree
- a migration plan for earlier experimental template directories

## Core Model

### `agent_home` is the agent identity root

Every agent has an `agent_home`.

`agent_home` should be treated as the agent's local identity root. It may hold:

- the agent's `AGENTS.md`
- agent-local skill links or entries
- agent-local supporting files that belong to that agent's role or workflow
- durable agent state already managed by the runtime

This applies uniformly to:

- the default agent
- named agents
- child agents

The difference between these agents is lifecycle and retention policy, not
whether they are allowed to have an `agent_home`.

### Agent-scoped `AGENTS.md`

Each agent may have its own `agent_home/AGENTS.md`.

This file is the agent-local durable guidance document for:

- role definition
- responsibilities
- textual permission boundaries
- long-lived collaboration expectations
- agent-specific workflow preferences
- agent-specific skill usage expectations

This file is not a runtime-enforced policy object. Its effect is prompt-level
guidance, not hard execution enforcement.

### Agent-local skills

Each agent may also have agent-local skills rooted under its own `agent_home`.

These skills exist so that:

- a skill can belong to one agent without becoming global
- a skill can belong to one agent without becoming workspace-wide
- a template can attach role-specific workflows to an agent identity

This RFC does not require that agent-local skills be copied into `agent_home`.
They may be represented by linked or referenced local skill entries, as long as
they appear as agent-scoped skills to the runtime.

## Initialization Contract

When Holon creates a new agent, initialization should establish a local
identity root for that agent rather than just a blank state directory.

The initialization contract should define:

- the agent identity record
- the agent's `agent_home`
- the initial agent-scoped `AGENTS.md`, if any
- the initial agent-local skill attachments, if any
- any explicit bootstrap metadata needed to explain how that state was created

Initialization should not require sending a large inline instruction blob
through normal spawn prompts.

## Template Model

### Templates are initializers

Holon should support an agent template mechanism for initializing new agents.

The purpose of a template is to provide reusable bootstrap content such as:

- an initial `AGENTS.md` body or rendered `AGENTS.md` template
- initial agent-local skill references

The template should be used to materialize the initial agent identity state in
`agent_home`.

### Template package format

The template format should stay minimal, file-based, and friendly to human
maintenance in Git.

A template should be represented as a directory. The directory name is the
template id unless explicitly overridden by metadata.

The target package shape is:

```text
agent_templates/<template_id>/
  template.toml
  AGENTS.md
  skills.toml
```

- `AGENTS.md`
  - required
  - used as the initial agent-scoped `AGENTS.md`
- `template.toml`
  - required
  - contains short metadata used for cataloging, search, compatibility checks,
    and GUI display
- `skills.toml`
  - optional
  - used to attach initial agent-local skill references

Long role guidance belongs in `AGENTS.md`, not inside `template.toml`.
Dependency-like skill references belong in `skills.toml`, not inside
`template.toml`.

This keeps the maintenance format aligned with the content:

- Markdown for long prompt and role text
- TOML for short hand-maintained metadata and dependency declarations
- JSON for daemon/API responses and generated indexes

YAML should not be the primary package format. It is more expressive than Holon
needs for this contract and introduces avoidable parsing ambiguity.

The minimal `template.toml` shape should be:

```toml
schema = "holon.agent_template.v1"
id = "reviewer"
name = "Reviewer"
summary = "Review code changes and publish structured feedback."

[compatibility]
holon = ">=0.1.0"
```

Only `schema`, `id`, `name`, and `summary` are required initially.
Compatibility metadata is optional but should be reserved in the v1 shape so
remote catalogs can expose it without changing the package boundary.

### Template selector

Template creation should use a single `template` selector rather than separate
template id and template path fields.

The selector should support these forms:

- `template_id`
- catalog-qualified selector such as `user:reviewer`, `agent:reviewer`, or
  `github:owner/repo#reviewer`
- absolute local path
- GitHub URL

Resolution should work like this:

- if `template` is an absolute path, use that local template directory
- if `template` is a GitHub URL, treat that URL as an explicit template source
- if `template` is catalog-qualified, resolve it from the named catalog source
- otherwise, treat it as `template_id` and resolve it from the default visible
  catalog

The default local user template root is:

```text
~/.agents/agent_templates/<template_id>/
```

Agent-local template libraries should use the matching agent-home root:

```text
<agent_home>/agent_templates/<template_id>/
```

The earlier experimental `~/.agents/templates` name is not part of the target
specification. Because this mechanism has limited usage, this RFC does not
require a migration or legacy compatibility path.

`template_id` should be a simple stable name, not a path-like string.

### Local and remote library layout

Holon package repositories should use explicit top-level collection
directories:

```text
skills/
  <skill_id>/
    SKILL.md

agent_templates/
  <template_id>/
    template.toml
    AGENTS.md
    skills.toml
```

`agent_templates` is intentionally more explicit than `templates`. It avoids
collisions with prompt templates, UI templates, workflow templates, and other
future template-like assets.

Template repositories use the top-level `agent_templates/` directory for
repository-level discovery. `holon-index.toml` is not part of v1 template
discovery; keeping one conventional path avoids a second source of truth for
remote sync.

### GitHub URL format

Explicit GitHub template URLs should stay narrow.

The accepted GitHub URL form should be:

```text
https://github.com/<owner>/<repo>/tree/<ref>/<path-to-template-dir>
```

The URL must point to the template directory itself, not just a repository root
or a higher-level folder.

The target directory should contain:

- `AGENTS.md`
- `template.toml`
- optional `skills.toml`

Explicit URL application should not require support for:

- repository root URLs
- raw-content URLs
- multiple GitHub URL variants with different semantics

If the URL does not resolve to a readable directory with the expected template
shape, template application should fail.

Repository-level discovery is a separate catalog operation from explicit
template URL application. A daemon discovers templates in configured remote
sources by scanning the `agent_templates/` directory and exposes the catalog
through API responses.

### Builtin default template

Holon ships exactly one hidden built-in `holon-default` template as the
zero-config and offline fallback. It is not exposed as a normal catalog entry
and is not seeded into the user-visible template root on startup.

Creating an agent without an explicit template selector uses this hidden
default. Creating an agent with a selector resolves only visible local
templates, catalog-qualified local templates, explicit paths, and explicit
GitHub template URLs. Compatibility selectors for the hidden default may remain
available, but they should not create duplicate visible catalog entries.

Common role templates such as developer, reviewer, release, or GitHub issue
solver should be delivered through the official remote template source and
materialized into `~/.agents/agent_templates` by sync/install operations.

### `skills.toml` format

The `skills.toml` format should stay minimal.

It should contain an array of typed skill references:

```toml
[[skills]]
kind = "local"
path = "/absolute/path/to/local-skill"

[[skills]]
kind = "github"
package = "vercel-labs/agent-skills@react-best-practices"
```

The rules are:

- `skills` is an array of typed skill references
- local references use:
  - `kind = "local"`
  - `path = "/absolute/path/to/skill"`
- local paths must be absolute paths
- relative local paths are not allowed
- a local path points to a skill directory whose entrypoint is `SKILL.md`
- GitHub references use:
  - `kind = "github"`
  - `package = "<owner>/<repo>@<skill>"`
- the GitHub package string should follow the same package-style convention used
  by `npx skills add`
- this RFC does not require the manifest to expose lower-level installer fields
  such as separate repo and path keys
- invalid, unreadable, or unresolvable entries should cause template
  application to fail rather than silently produce a partially initialized
  agent

The template package does not require additional per-skill metadata in the
manifest. The runtime may derive name and description from the referenced skill
itself.

Earlier experimental implementations used `skills.json`. The target package
format should use `skills.toml` for human-maintained template packages.

### Materializing agent-local skills

Template application should materialize template-attached skills into normal
agent-scoped skill roots under `agent_home`.

The goal is for runtime skill discovery to continue working against ordinary
agent-local skill directories.

The intended behavior is:

- local skill refs may be materialized as symlinks into the agent skill root
- GitHub skill refs may be materialized as installed skill directories in the
  agent skill root
- failed skill resolution or installation should fail template application

Template application should not require a separate runtime skill manifest format
for already initialized agents.

### Templates are not the live source of truth

After initialization, the template should not remain the live source of truth
for the agent.

Instead:

- the live source of truth is the agent's own `agent_home/AGENTS.md`
- the runtime may record template provenance such as template id or source
- later template changes should not silently rewrite an already-created agent

If an operator wants to realign an existing agent with a template, that should
be an explicit action, not an automatic background update.

## Daemon API Requirements

A GUI management surface should not read template directories directly. The
daemon should expose templates as first-class catalog and lifecycle resources.

The API surface should be able to:

- list visible template catalog entries across user, agent-local, and managed
  templates materialized from configured remote sources
- return template details, including metadata, source, package path, rendered
  `AGENTS.md` preview, and declared skill references
- validate a local or remote template package without applying it
- install or update a template package from a configured repository source into
  the local user template root
- create an agent from a selected template selector
- report template provenance for an initialized agent

The API response format should be JSON even though the source package format is
TOML and Markdown.

Catalog entries should preserve enough source information for a GUI to explain
where a template came from without making local path conventions part of the UI
contract.

The daemon should keep template catalog operations distinct from agent
initialization:

- catalog operations discover, inspect, validate, install, or remove templates
- initialization operations apply one selected template to create an agent home

This split mirrors skill discovery versus skill activation and keeps GUI
management from becoming coupled to one creation flow.

### Remote source configuration

Remote template sources are configured explicitly in daemon-level config:

```json
{
  "agent_templates": {
    "remote_sources": {
      "official": {
        "url": "https://github.com/holon-run/holon",
        "ref": "main",
        "enabled": true
      }
    }
  }
}
```

Config fields:

- `url`: remote repository URL; v1 supports GitHub repository URLs
- `ref`: optional branch/tag/commit; omitted means provider default branch
- `enabled`: optional bool, default `true`

v1 supported URL shape: GitHub repository URL
(`https://github.com/<owner>/<repo>`). Internally normalized as `kind = "github"`
so generic git/http can be added later without changing the public config shape.

v1 non-goals:

- no env/CLI override source of truth
- no secret material in config
- no implicit trust source from `template-provenance.json`, existing `agent_home`,
  or ad-hoc URLs
- no configurable remote repo layout path

If config does not explicitly define `official`, Holon registers the official
`https://github.com/holon-run/holon` source by default. Startup must not require
network access: missing, stale, or failed source sync state is surfaced as
source status and diagnostics, not as an agent creation blocker.

### Remote source sync

Sync can be requested explicitly:

```
POST /templates/remote-sources/sync
```

Request body (optional):

```json
{
  "source_id": "official",
  "force": false
}
```

- Omit `source_id` to sync all enabled sources
- `force: true` bypasses cache freshness checks

Sync runs as a daemon job (`kind = "agent_template.remote_sources.sync"`) through
the existing `/jobs` infrastructure. Jobs are asynchronous and trackable.

The daemon may also start a non-blocking background sync for enabled sources
that have never synced or whose last successful sync is stale. That work must
not block startup, default agent creation, or explicit agent creation.

Sync results are persisted in DB table `agent_template_remote_source_syncs` with
fields: `source_id`, `kind`, `url`, `requested_ref`, `enabled`, `status`,
`last_synced_at`, `resolved_ref`, nullable `resolved_revision`, `catalog_json`,
`diagnostics_json`, and `error`.

Sync materializes each discovered template into:

```text
~/.agents/agent_templates/<template_id>/
```

This is intentionally the same root used for user-authored templates and
explicit installs. Managed metadata records the owning remote source and content
hash. A later sync may update templates it owns, but it must refuse to overwrite
a user-owned template directory or a template managed by another source.

The DB row remains source status metadata for APIs and diagnostics. It is not
the live catalog source for synced templates; after materialization, normal
local template discovery exposes the synced templates.

### Catalog API response

`GET /templates/catalog` returns:

```json
{
  "catalog": [
    {
      "catalog_id": "user_global:holon-developer",
      "template": "holon-developer",
      "template_id": "holon-developer",
      "source": "user_global",
      "name": "Holon Developer",
      "description": "A long-lived implementation-focused agent.",
      "included_skills": []
    }
  ],
  "sources": [
    { "source_id": "official", "kind": "github", "enabled": true,
      "status": "synced",
      "url": "https://github.com/holon-run/holon",
      "resolved_ref": "main", "resolved_revision": null,
      "last_synced_at": "2026-01-01T00:00:00Z" }
  ],
  "diagnostics": []
}
```

`resolved_ref` is the configured or default branch name. `resolved_revision` is
nullable in v1 because GitHub contents discovery currently resolves branch
names for request stability but does not require an additional commit SHA
lookup. The visible catalog is local: user-authored templates, explicit
installs, agent-home templates, and managed templates materialized by remote
sync. Hidden built-in defaults are not returned as normal catalog entries.

## Evolving `AGENTS.md`

### `AGENTS.md` is allowed to evolve

Agent-scoped `AGENTS.md` should be allowed to change after initialization.

This is important because the file acts as durable agent-local memory for:

- role refinement
- clarified responsibilities
- updated expectations from operator feedback
- accumulated long-lived guidance that does not belong in one task prompt

The operator may direct the agent to update this file as part of normal
collaboration.

### Effective timing

Updates to `agent_home/AGENTS.md` should take effect on the next prompt
assembly. They do not need to interrupt an in-flight turn.

This keeps behavior inspectable and avoids mid-turn instruction mutation.

### Content boundary

Agent-scoped `AGENTS.md` should primarily capture long-lived role and
collaboration guidance.

It should not become the default place for:

- one-off task goals
- temporary acceptance criteria for a single issue
- transient debugging notes
- short-lived operator prompts that belong to the current turn only

Those belong in operator prompts, work items, or other task-scoped runtime
structures.

## Uniform Agent Treatment

Holon should treat all agents uniformly for initialization:

- every agent may have an `agent_home`
- every agent may have agent-scoped `AGENTS.md`
- every agent may have agent-local skills

The distinction between default, named, and child agents should come from
lifecycle and retention policy rather than a separate initialization model.

For example:

- long-lived agents normally retain their `agent_home`
- a temporary child agent may have its `agent_home` cleaned up when the agent is
  retired and removed

This cleanup policy does not change the initialization contract while the agent
exists.

## Relationship To Existing RFCs

This RFC builds on, and does not replace:

- [Instruction Loading](./instruction-loading.md)
- [Skill Discovery and Activation](./skill-discovery-and-activation.md)
- [Workspace Binding and Execution Roots](./workspace-binding-and-execution-roots.md)

Those RFCs define where instructions and skills are loaded from. This RFC
defines how a new agent gets its own local instruction and skill roots in the
first place.

## Initial Direction

The intended direction is:

1. new agents can be initialized from a template
2. template selection uses one `template` selector that accepts `template_id`,
   catalog-qualified selector, absolute path, or GitHub URL
3. user-authored template packages live under
   `~/.agents/agent_templates/<template_id>/`
4. package repositories use top-level `agent_templates/` convention;
   no index file supported
5. each template package uses `template.toml`, `AGENTS.md`, and optional
   `skills.toml`
6. templates materialize an initial `agent_home/AGENTS.md`
7. templates may attach agent-local skill references
8. initialized agent-local skills are materialized into normal agent-scoped
   skill directories under `agent_home`
9. the daemon exposes list/detail/validate/install/create/provenance API
   operations before the GUI manages templates directly
10. the runtime records template provenance, but not a persistent parallel copy
    of the expanded instruction text as the live identity source
11. automatic refresh-from-template is not part of initialization; explicit
    update or realign actions can be added later
12. later edits to `agent_home/AGENTS.md` are allowed and become effective on the
    next turn
