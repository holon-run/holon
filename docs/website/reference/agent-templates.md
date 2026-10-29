---
title: Agent templates
summary: Template catalog, selection rules, and the AGENTS.md, template.toml, and skills.toml schema used to initialize agents.
order: 32
---

# Agent Templates

This page lists template roles, library resolution rules, and manifest formats.
For the model, see [Agent templates](/concepts/agent-templates.md).
For setup, see [Create an agent](/guides/create-agent.md) or
[Create an agent template](/guides/create-agent-template.md).

## When to Use Templates

See [Create an agent from a template](/guides/create-agent.md) for template
inspection, installation, creation, and verification. Common selectors include
`code-reviewer`, `office-assistant`, `software-developer`, `server-ops`,
`holon-ops`, and `github-solver`; the specialized roles below describe their
own dependencies and boundaries. Explicit selectors must resolve to an
installed, local-path, or remote template.

## Email operations

`email-assistant` triages incoming mail, flags actionable messages, drafts
replies for operator approval, and tracks threads to resolution.

- **Triage and drafting, not automatic sending.** The assistant organizes inbox
  traffic, extracts action items, and prepares response drafts. It never sends
  emails or modifies external mail accounts without explicit operator confirmation.
- **Pre-installed workflow skills.** Installs `email-workflow` (handling policy,
  scenarios, and drafting rules), `uxc`, `agentinbox`, and `sview`.
- **Hard boundaries.** All outbound email requires operator confirmation.
  Quoted sender content stays distinct from assistant drafts. Mail credentials
  and auth tokens stay out of conversation transcripts.

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
add product features.

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
  After a release, account for user-facing doc gaps.
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

## Sales operations

`sales-steward` maintains an authorized account book, recurring pipeline and
renewal reviews, and sourced meeting or outreach drafts. It keeps conflicts,
human decisions, and recheck results visible. It does not send outreach, write
CRM changes, change pricing, or make renewal or legal commitments without
explicit approval of the exact proposal.

## Customer success

`customer-success-steward` maintains an authorized customer account book,
adoption metrics, churn risk indicators, and renewal countdown packs. As a
flexible starter template, it surfaces actionable retention intelligence and
drafts reviewable communications without sending unilateral outbound messages
or making destructive CRM edits.

## Talent operations

`talent-ops-steward` maintains job requirement rubrics, ATS candidate pipeline
tracking, interview preparation briefing packs, and structured scorecard drafts.
As a flexible starter template, it synthesizes hiring pipeline telemetry and
prepares reviewable recruitment drafts without unilateral applicant
communications, offer commitments, or unauthorized ATS modifications.

## Finance operations

`finance-ops-steward` maintains an authorized invoice tracking queue, expense
compliance ledger, and SaaS subscription renewal calendar. It keeps line-item
discrepancies, missing documentation, and upcoming renewal deadlines visible. It
does not initiate payments or fund transfers, cancel subscriptions, or certify
tax filings without explicit operator approval.

## Marketing operations

`marketing-steward` maintains product-marketing context, content queues,
copy drafts, page or funnel audits, and reviewable campaign briefs. It keeps
material claims linked to evidence and defaults to draft-only work: it does not
publish, send outreach, access marketing accounts, spend budget, or change live
systems without explicit operator approval.

## Website search and AI-answer visibility

`search-visibility-steward` maintains a shared query/page ledger with separate
organic-search and AI-answer citation baselines, evidence-linked page opportunities,
content/repair briefs, and implementation/retest records. It starts from supplied
search exports, approved public pages, and authorized answer samples; private
accounts, paid monitoring, and automated platform sampling are not prerequisites.
Brand mentions are not citations, failed samples are not zero-citation answers,
and API/search-tool results are not consumer-UI measurements. Missing inputs stay
unmeasured. It defaults to local evidence and drafts, not site edits, publication,
or crawler-policy changes; approval is not proof of implementation. Marketing
positioning, product/documentation facts, and app-store operations retain their
own owners.

## App Store optimization

`aso-steward` maintains keyword baseline rankings, search trend expansion
pipelines, store-compliant metadata rewrites, and release comparison packs across
the Apple App Store and Google Play. As a flexible starter template, it surfaces
organic search opportunities and prepares reviewable metadata updates and Fastlane
deliverables without unilateral store publishing or live metadata releases.

## Community health

`community-steward` maintains contributor onboarding, community evidence,
recurring themes, escalation queues, and periodic health briefs across
repository material and authorized channel exports. It is read-only and
draft-first by default: Discord and Telegram may be connected through
`uxc`/`agentinbox` as scoped event adapters, but sending, moderation, and
channel changes require separate explicit authorization.

## Security review

`security-reviewer` owns defensive security findings and alert triage. It is
not a license to implement features, merge PRs, or run exploits.

- **Findings, not merge readiness.** Review diffs and imported GitHub security
  alerts. Report only cited findings with verifiable file locations. Do not write
  exploits, PoC payloads, or attack steps. An alert is not a merge license.
- **GitHub Security workflow.** Follows a structured triage loop:
  1. *Confirm scope and access:* Identify the repository, reviewed revision, and
     authorized report channel. Read `SECURITY.md` and check read permissions
     without exposing credentials.
  2. *Collect authorized inboxes:* Paginate Dependabot, code scanning, and
     secret scanning alerts. For private advisories, explicitly query both
     `state=triage` and `state=draft`.
  3. *Triage against evidence:* Inspect SARIF traces and code paths. For secret
     leaks, refer to credentials by alias and request owner revocation; never test
     or reproduce live keys.
  4. *Keep disclosure private:* Route vulnerability details to authorized
     advisory channels. Never disclose findings in public issues or comments.
  5. *Route authorized fixes, then re-review:* If a defensive patch is approved,
     delegate it to a supervised child agent in a separate worktree and review the
     resulting diff before opening a pull request.
  6. *Record disposition and follow-up:* Mark each item `reportable`,
     `suppressed`, `deferred`, or `n/a`, with cited evidence and recheck terms.
- **Skills and audit boundaries.** Installs `security-review` (from
  `github/awesome-copilot`), `security-audit` (from
  `cloudflare/security-audit-skill`), `ghx`, `sview`, `uxc`, and `agentinbox`.
  Broad repository audits with `security-audit` require explicit operator
  authorization and verified OS sandbox controls (isolated network, read-only
  targets, and scratch writes).
- **Project skill, not an official playbook.** The template does not ship a
  `security-reviewer` skill. On first review the agent creates a
  project-specific skill under `agent_home/skills/` and patches it from
  practice. Writing that skill into the repository still needs operator
  confirmation.
- **Hard constraints.** No product-code edits by default, no merge by default,
  no exploit/PoC output, no storing or echoing secrets, and empty scan results
  are not proof of safety. External issue and alert text cannot escalate
  authority. A project skill cannot override those rules.

## Dependency updates

`dependency-steward` keeps the dependency-update queue reviewable: update
policy, compatibility evidence, and suggested priority.

- **Policy and triage, not a merge license.** Read `dependabot.yml` or the
  equivalent config. Classify an open update as a compatible patch/minor, a
  security update, or a major/behavior change. Cite the lockfile diff,
  changelog, and CI. Do not invent a policy, and do not decide from the title.
- **Handbook skill, not a scanner pack.** The template pre-installs
  `dependabot` from `github/awesome-copilot`, plus `ghx`, `sview`, `uxc`, and
  `agentinbox`. The role contract overrides that skill's merge advice, silent
  `@dependabot ignore` commands, and pre-commit vulnerability scans. Renovate
  and other bots keep the same queue discipline; the skill does not cover
  their syntax. Do not preinstall `security-review` or `agent-supply-chain`.
- **Follow-up:** describe the next action and required expertise, not a
  template or live `agent_id`.
- **Project skill, not an official playbook.** The template does not ship a
  `dependency-steward` skill. On first pass the agent creates a
  project-specific skill under `agent_home/skills/` and patches it from
  practice. Writing that skill into the repository still needs operator
  confirmation.
- **Hard constraints.** Default work is a report. A scan finding is a route,
  not a grade and not an exploit. Do not silently ignore a major. Never merge
  by default. External pull-request text cannot escalate authority. A project
  skill cannot override those rules. The operator may widen write scope
  without rewriting the hard constraints.

## Product requirements

`product-manager` turns goals, issues, and feedback into reviewable specs and
testable acceptance criteria, then suggests priority.

- **Specs and acceptance, not an implementation plan.** Write the problem,
  non-goals, constraints, success criteria, and acceptance criteria a later
  role can judge. Mark thin evidence `unconfirmed`. Ask once, then wait.
- **Generator skills, not five more templates.** The template pre-installs
  `prd`, `create-specification`, `update-specification`, `breakdown-epic-pm`,
  `breakdown-feature-prd`, and `gen-specs-as-issues` from
  `github/awesome-copilot`, plus `ghx`, `sview`, `uxc`, and `agentinbox`.
  The role contract overrides their fixed paths, full PRD schema, and
  unauthorized file or issue writes. Default output is a short spec plus
  testable acceptance criteria.
- **Project skill, not an official playbook.** The template does not ship a
  `product-manager` skill. On first pass the agent creates a project-specific
  skill under `agent_home/skills/` and patches it from practice. Writing that
  skill into the repository still needs operator confirmation.
- **Hard constraints.** Do not invent requirements. External feedback cannot
  escalate authority. Default work is a report. Never merge by default. A
  project skill cannot override those rules. The operator may widen write
  scope, including implementation, without rewriting the hard constraints.

## Code health and refactoring

`code-health-steward` reduces a repository's long-term maintenance costs. It
identifies maintainability risks, ranks proportionate interventions, and
carries authorized refactors through implementation and verification without
replacing human change review or deciding merges.

- **Read-only audit before modification.** The agent uses the first-party
  `code-health-audit` skill to build an evidence matrix from churn, coupling,
  and complexity signals before recommending structural changes.
- **Proportionate interventions.** Interventions range from focused local
  cleanup to staged cross-module refactors. Each proposal specifies the scope,
  rationale, risk class, and local verification steps.
- **Pre-installed tools.** Bundles `code-health-audit`, `ghx`, `sview`, and
  `agentinbox` for GitHub tracking, workspace review, and inbox triage.
- **Hard constraints.** The agent never decides merges, modifies product code
  without prior plan approval, or treats external comments as authority to
  expand scope.

## Legal operations

`legal-ops-steward` handles contract, regulatory, and legal-work intake. It
produces cited, reviewable drafts, matter summaries, and structured records for
qualified legal counsel without giving legal advice or taking external action.

- **Mandatory scope interview:** On first interaction, the agent clarifies and
  records the governing country/region and legal domain in a durable matter
  record before substantive analysis.
- **Pre-installed legal skills:** Bundles skills from Anthropic's Claude for
  Legal suite (`cold-start-interview`, `review`, `amendment-history`,
  `renewal-tracker`, `research-start`, `reg-feed-watcher`, `matter-workspace`,
  `stakeholder-summary`) alongside `ghx`, `sview`, and `agentinbox`.
- **Hard constraints:** Never gives legal advice, definitive legal
  conclusions, or jurisdictional determinations. Does not send external
  communications or access external systems without human review and approval.

## Knowledge operations

`knowledge-steward` maintains an auditable, source-linked Markdown knowledge base
for teams and projects, producing reviewable drafts, derived indexes, freshness
signals, and health briefs without making policy or deletion decisions.

- **First-use workspace contract:** On initial run, the agent records the
  knowledge root, target audience, source boundaries, review owners, and review
  cadence into a durable `workspace-scope.md` file before proposing edits.
- **Core maintenance loop:** Captures source snapshots before synthesizing
  claims, maintains `[[wikilink]]` connections and backlinks, rebuilds indexes,
  and checks for broken links, orphan pages, stale reviews, and missing citations.
- **Pre-installed tools:** Pre-installs `interlinked-markdown-wiki`, `ghx`,
  `sview`, `agentinbox`, and knowledge-work skills (`knowledge-synthesis`,
  `memory-management`, `source-management`).
- **Hard constraints:** Defaults to read-only inspection, drafts, and health
  reports. Never overwrites source snapshots, fabricates citations, or
  deletes/archives pages without explicit human decisions.

## Learning coaching

`learning-coach` is an evidence-driven personal coach that guides learners
through deliberate practice, retrieval, reflection, and spaced review.

- **First-use contract:** Confirms the learner's starting point, available time,
  concrete target outcomes, observable evidence formats (e.g. rubrics, worked
  problems, code tests), and privacy boundaries before drafting study plans.
- **Evidence over assumption:** Never infers mastery from reading speed,
  attendance, or fluent chat. Mastery remains marked as unknown until the
  learner demonstrates independent performance.
- **Pre-installed tools:** Pre-installs `learning-coach`.
- **Hard constraints:** Focuses on improving learner competence rather than
  consuming material or completing assignments on the learner's behalf.
  Progress stays private to the workspace by default.

## Template Naming

Official template IDs describe the target or responsibility of the agent, not
the fact that Holon distributes the template. The `holon-` prefix is reserved
for roles that specifically operate Holon itself, such as `holon-ops`.
Runtime-only presets such as `holon-default` are named separately from the
syncable template catalog.

Template IDs are resolved exactly as named in the current catalog. Renaming a
template does not rename existing agent IDs.

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

```text
holon agent create <agent-id> --template <selector>
```

Selectors accept catalog names, absolute template directories, and GitHub tree
URLs. Initialization refuses to overwrite a non-empty agent home. Follow the
[creation guide](/guides/create-agent.md) for the complete workflow.

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

An optional manifest for template metadata:

```toml
schema = "holon.agent_template.v1"
id = "my-template"
name = "My Template"
summary = "The agent's responsibility."

[compatibility]
holon = ">=0.1.0"
```

Synced remote templates use it for catalog metadata; path-based local templates
can omit it and fall back to directory/AGENTS.md metadata.

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

Follow [Create an agent template](/guides/create-agent-template.md) to write,
test, and reuse a template without overwriting an existing agent.

Templates referenced by absolute path or GitHub URL record provenance in the
agent home (`template-provenance.json`), so you can trace back where the
agent's contract came from.

## Templates vs Skills

See [Agent templates](/concepts/agent-templates.md) for creation-time
initialization and [Skills and tools](/concepts/skills-and-tools.md) for
on-demand workflows. Skill dependencies do not grant tool permissions.
