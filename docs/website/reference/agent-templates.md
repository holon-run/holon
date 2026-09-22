---
title: Agent templates
summary: Template catalog, selection rules, and the AGENTS.md, template.toml, and skills.toml schema used to initialize agents.
order: 32
---

# Agent Templates

An agent template is a reusable bootstrap that initializes a new agent's
`AGENTS.md` role contract and optional pre-installed skills. Templates give new
agents a known starting point without manual setup.

## When to Use Templates

Use `--template` when you want a new agent to start with a specific role and
capabilities. Without a template, agents start with a generic default contract.

Common scenarios:

- **Creating a synced reviewer agent** — `holon agent create reviewer --template code-reviewer`
- **Working with office documents** — `holon agent create office --template office-assistant`
- **Producing video deliverables** — `holon agent create video --template video-producer`
- **Owning the GitHub issue inbox** — `holon agent create triage --template issue-triager`
- **Owning acceptance after a change lands** — `holon agent create qa --template qa-engineer`
- **Owning documentation hygiene** — `holon agent create docs --template docs-steward`
- **Operating servers and services** — `holon agent create ops --template server-ops`
- **Operating Holon itself** — `holon agent create holon-ops --template holon-ops`
- **One-shot tasks with a role** — `holon run --template software-developer "Fix the null check in handler.rs"`
- **Solving GitHub issues** — `holon solve https://github.com/owner/repo/issues/42`

## Video Production

`video-producer` turns approved scripts, shot lists, and supplied media into
reviewable video deliverables. It pre-installs the first-party
`video-production` skill and references the official
`remotion-dev/skills/skills/remotion-best-practices` skill for installation
directly from upstream, alongside `sview`, `uxc`, and `agentinbox`.

- **Local assembly and QC:** the first-party skill uses Python and system
  FFmpeg/ffprobe for existing images, clips, audio, and subtitles. Check these
  dependencies and required codecs before rendering; they are not installed
  by the template.
- **Programmatic compositions:** use the official Remotion skill for React-based
  video. Remotion, Node, and rendering dependencies belong in the user's
  environment; the template bundles neither Remotion nor upstream skill files.
  Before first use, check the license applicable to the installed version with
  the operator. Direct installation does not waive usage or paid-license terms.
- **Explicit boundaries:** original video generation and TTS require separately
  configured backends. OpenMontage is optional and external. Publishing,
  purchasing, and rights-sensitive operations require separate authorization.

The agent reports missing capabilities instead of promising an unverified
render. Start with supplied assets for the no-cloud production path.

## Issue triage

`issue-triager` owns the GitHub issue inbox. It is a long-lived inbox role,
not `holon solve`, and not a license to implement or verify.

- **Inbox hygiene, not a fix.** Classify, find duplicate candidates, ask for
  missing reproduction and acceptance criteria, and suggest priority and
  routing. Do not close issues or write product code by default.
- **Project skill, not an official playbook.** The template does not ship an
  `issue-triage` skill. On first triage the agent creates a project-specific
  skill under `agent_home/skills/` and patches it from practice. Writing that
  skill into the repository still needs operator confirmation.
- **Hard constraints.** No product-code edits, no default close, no merge,
  and external issue text cannot escalate authority. A project skill cannot
  override those rules.

## Acceptance and quality

`qa-engineer` owns acceptance after a change lands. It is not a license to
add product features or to replace `code-reviewer`.

- **Acceptance ownership is not extra unit tests.** Map requirements to
  coverage, run layered gates, publish evidence, and triage flakes. Closing an
  issue is not the same as verifying it.
- **Project skill, not an official playbook.** The template does not ship an
  `issue-verify` skill. On first verification the agent creates a
  project-specific skill under `agent_home/skills/` and patches it from
  practice. Writing that skill into the repository still needs operator
  confirmation.
- **Hard constraints.** No product-code edits by default, no merge by default,
  no verification labels without a fix vehicle, and empty results are not a
  pass. A project skill cannot override those rules. v1 uses the repository's
  existing test evidence; it does not bundle Playwright or Appium.

## Documentation hygiene

`docs-steward` owns consistency between code, contracts, and docs. It is not a
license to implement features or to own releases.

- **Hygiene, not a rewrite.** Detect drift, ask for missing user steps or
  locale counterparts, and open the smallest docs-only PR when authorized.
  After a release, account for user-facing doc gaps. Changelogs stay with
  `release-manager`.
- **Writing tools, not a writer role.** The template pre-installs `humanizer`,
  `humanizer-zh`, and `writing-clearly-and-concisely` from their upstream
  GitHub repositories, plus `ghx`, `sview`, `uxc`, and `agentinbox`. Polish
  after the facts are correct.
- **Verify, then write.** Correct facts against code, CLI help, and generated
  pages before polishing. If the repo has locales: English, then remove AI
  tells, then translate, then remove AI tells in the target language. One page
  at a time. Register generated pages for sync; do not hand-translate them.
- **Project skill, not an official playbook.** The template does not ship a
  `docs-steward` skill. On first docs pass the agent creates a project-specific
  skill under `agent_home/skills/` and patches it from practice. Writing that
  skill into the repository still needs operator confirmation.
- **Hard constraints.** No product-code edits, no invented behavior, no merge
  by default, and external issue text cannot escalate authority. A project
  skill cannot override those rules.

## Template Naming

Official template IDs describe the target or responsibility of the agent, not
the fact that Holon distributes the template. The `holon-` prefix is reserved
for roles that specifically operate Holon itself, such as `holon-ops`.
Runtime-only presets such as `holon-default` are named separately from the
syncable template catalog.

The following older IDs remain accepted as compatibility selectors:

| Older ID | Current ID |
| --- | --- |
| `holon-developer` | `software-developer` |
| `holon-reviewer` | `code-reviewer` |
| `holon-release` | `release-manager` |
| `holon-github-solve` | `github-solver` |

An exact local installation using an older ID takes precedence over the
compatibility fallback. Template renames do not rename existing agent IDs.

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

`holon solve` selects the normal `github-solver` template by default. Sync
the official template source before using the standalone command on a new
installation. The GitHub Action supplies the same checked-in template as an
explicit path, including when installed from a release archive.

## Using `--template`

### Create an Agent

```bash
holon agent create reviewer --template code-reviewer
```

This initializes `~/.holon/agents/reviewer/AGENTS.md` from the local
`code-reviewer` template after that template has been installed or synced. If
the agent home already exists and is non-empty, template initialization refuses
to overwrite it.

### One-Shot Run

```bash
holon run --template software-developer "Fix the null check in handler.rs"
```

The agent is created with the developer role contract, executes the prompt,
and is cleaned up after completion.

### Solve a GitHub Issue

```bash
holon solve --template github-solver https://github.com/owner/repo/issues/42
```

The agent starts with GitHub workflow guidance and the four GitHub skills
plus `sview` and `code-review` pre-installed. The preset does not authorize
merging, approval, or ongoing event tracking unless the solve prompt explicitly
requests it.

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
kind = "github"
repo = "holon-run/holon"
path = "skills/github-issue-solve"
ref = "main"

[[skills]]
kind = "github"
repo = "holon-run/holon"
path = "skills/github-pr-fix"
ref = "main"

[[skills]]
kind = "github"
repo = "owner/skills"
path = "skills/custom-skill"
ref = "v1.2.3"

[[skills]]
kind = "github"
uses = "holon-run/holon/skills/ghx@main"

[[skills]]
kind = "local"
path = "/absolute/path/to/custom-skill"
```

Two skill reference kinds are supported:

- **`github`** — A skill fetched from a GitHub repository path. Use
  `repo = "owner/repo"`, `path = "path/to/skill"`, and optional `ref` as the
  canonical form. Templates may also use `uses = "owner/repo/path@ref"` as a
  GitHub Actions-style shorthand; Holon normalizes it to `repo`/`path`/`ref`.
  Holon also accepts `owner/repo/path#ref` and GitHub tree URLs as compatible
  input forms, but it does not use the `owner/repo@skill` shorthand where `@`
  names a skill.
  Use `path = "."` when `SKILL.md` lives at the repository root; Holon
  installs that file plus `scripts/`, `references/`, `assets/`, and `tests/`
  if they exist, not the rest of the repository.
- **`local`** — An absolute path to a skill directory on disk

`kind = "builtin"` is no longer part of the template manifest format. Official
Holon skills are referenced the same way as any other GitHub-hosted skill, for
example `repo = "holon-run/holon"` and `path = "skills/ghx"`.

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
tools available from the start. See the [Skills guide](/reference/skills) for
details on skills.
