# RFC-0003: Skill-Artifact Architecture

| Metadata | Value |
| :--- | :--- |
| **Status** | **Draft** |
| **Author** | Holon Contributors |
| **Created** | 2026-01-06 |
| **Parent** | RFC-0001, RFC-0002 |
| **Issue** | [#433](https://github.com/holon-run/holon/issues/433) |

## 1. Summary

This RFC proposes a unified **Skill-Artifact-Publisher** architecture to replace the current mode-based design. The key changes are:

1. **Unify `solve` and `pr-fix` modes** into a single skill with context-aware behavior
2. **Organize skills by platform and scenario** (e.g., `github/solve`, `jira/solve`)
3. **Simplify artifacts** to core operations only, allowing agents to use `gh` CLI for auxiliary operations
4. **Publisher behavior driven by artifacts**, not modes

## 2. Motivation

### 2.1 Current Pain Points

- **Mode fragmentation**: `solve` vs `pr-fix` split is artificial; the runtime already distinguishes Issue vs PR by context
- **Artifact explosion**: Defining artifacts for every possible operation (comment, label, project update) leads to complexity
- **Limited flexibility**: Agents cannot perform simple operations without going through the artifact pipeline

### 2.2 Design Goals

- **Simplicity**: Reduce concepts agents need to understand
- **Flexibility**: Allow agents to perform low-risk operations directly
- **Auditability**: Maintain artifact-based tracking for high-risk operations (code changes, PR creation)
- **Extensibility**: Support new platforms (Jira, GitLab) without changing core abstractions

## 3. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                      SKILL LAYER                                │
│  Skills organized by platform/scenario:                         │
│  • github/solve    • github/review    • jira/solve             │
│  • Skills are loaded from .claude/skills/{platform}/{skill}/   │
└────────────────────────────┬────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│                      AGENT EXECUTION                            │
│  Agent operates with:                                           │
│  • Skill instructions (SKILL.md)                                │
│  • Direct gh CLI access (for auxiliary operations)              │
│  • Artifact output (for core operations)                        │
└────────────────────────────┬────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│                      ARTIFACT LAYER                             │
│  Minimal, focused artifacts:                                    │
│  • changes/*.patch     → Code changes                           │
│  • messages/summary.md → PR body / execution summary            │
│  • review/replies.json → Review thread replies                  │
└────────────────────────────┬────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│                      PUBLISHER LAYER                            │
│  Publisher actions driven by artifacts:                         │
│  • changes/*.patch exists  → Create/update PR                   │
│  • review/replies.json     → Post review replies                │
│  • Context determines create vs update                          │
└─────────────────────────────────────────────────────────────────┘
```

## 4. Skill Design

### 4.1 Skill Organization

Skills are organized by platform and scenario:

```
.claude/skills/
├── github/
│   ├── solve/
│   │   └── SKILL.md       # Issue → Code changes → PR
│   ├── review/
│   │   └── SKILL.md       # Review PR, provide feedback
│   └── triage/
│       └── SKILL.md       # Categorize and label issues
├── jira/
│   └── solve/
│       └── SKILL.md       # Jira ticket → Code changes
└── local/
    └── report/
        └── SKILL.md       # Generate reports from local files
```

### 4.2 Skill Loading

Skills are specified via:
- CLI: `holon --skill github/solve`
- Config: `skills: ["github/solve"]` in `holon.yaml`
- Spec: `skills: ["github/solve"]` in `spec.yaml`

The Runner loads skills from `.claude/skills/{skill}/SKILL.md`.

### 4.3 Unified github/solve Skill

The `github/solve` skill replaces both `solve` and `pr-fix` modes:

```markdown
# SKILL.md - github/solve

## Context Detection
- If `/holon/input/context/github/pr.json` exists → PR-fix behavior
- If only `/holon/input/context/github/issue.json` exists → Issue-solve behavior

## Capabilities
Agent MAY use these commands directly:
- `gh issue view/comment`
- `gh pr view/comment`
- `gh issue edit` (labels, assignees)
- `gh api` (read operations)

Agent MUST NOT use directly (use artifacts instead):
- `git push`
- `gh pr create/merge`

## Output Contract
Required:
- `messages/summary.md` - Execution summary

Conditional:
- `changes/*.patch` - When code changes are made
- `review/replies.json` - When replying to PR review threads
```

### 4.4 Builtin Skills Storage

Holon provides builtin skills that are embedded in the binary. These are stored at the repository root level for visibility:

```
holon/
├── skills/                     # Builtin skills (top-level, easy to discover)
│   └── github/
│       ├── solve/
│       │   └── SKILL.md
│       └── review/
│           └── SKILL.md
├── agents/                     # Agent implementations
├── pkg/
│   └── skills/
│       ├── skills.go           # User skill resolution (existing)
│       ├── builtin.go          # Builtin skill loading (new)
│       └── builtin/
│           └── embed.go        # go:embed for builtin skills
└── ...
```

Builtin skills are embedded using Go's `embed` package:

```go
// pkg/skills/builtin/embed.go
package builtin

import (
    "embed"
    "io/fs"
)

//go:embed skills/*
var builtinSkills embed.FS

func FS() fs.FS {
    sub, _ := fs.Sub(builtinSkills, "skills")
    return sub
}

func Has(ref string) bool {
    path := filepath.Join(ref, "SKILL.md")
    _, err := fs.Stat(FS(), path)
    return err == nil
}

func Load(ref string) ([]byte, error) {
    path := filepath.Join(ref, "SKILL.md")
    return fs.ReadFile(FS(), path)
}
```

### 4.5 Skill Loading Priority

Skills are resolved in the following order (first match wins):

1. **Remote URL**: If skill ref is a URL, download and extract
2. **Workspace skill**: `.claude/skills/{skill}/SKILL.md` in the workspace
3. **Absolute/relative path**: Direct filesystem path
4. **Builtin skill**: Embedded in the Holon binary

```go
func (r *Resolver) Resolve(skillRef string) (Skill, error) {
    // 1. Remote URL
    if remote.IsURL(skillRef) {
        return r.loadRemote(skillRef)
    }
    
    // 2. Workspace skill
    workspaceSkill := filepath.Join(r.workspace, ".claude/skills", skillRef)
    if exists(workspaceSkill) {
        return r.loadLocal(workspaceSkill)
    }
    
    // 3. Absolute/relative path
    if exists(skillRef) {
        return r.loadLocal(skillRef)
    }
    
    // 4. Builtin skill
    if builtin.Has(skillRef) {
        return builtin.Load(skillRef)
    }
    
    return Skill{}, fmt.Errorf("skill not found: %s", skillRef)
}
```

This priority order enables:
- **User override**: Workspace skills take precedence over builtin skills
- **Customization**: Users can copy and modify builtin skills
- **Defaults**: Builtin skills work out-of-the-box without configuration

## 5. Artifact Design

### 5.1 Core Principle

**Artifacts are for high-risk, auditable operations only.**

| Operation Type | Risk | Mechanism |
|----------------|------|-----------|
| Code changes | High | Artifact (`changes/*.patch`) |
| PR creation/merge | High | Artifact + Publisher |
| Review replies | Medium | Artifact (`review/replies.json`) |
| Comments | Low | Direct `gh` call |
| Labels/Assignees | Low | Direct `gh` call |
| Issue creation | Low | Direct `gh` call |

### 5.2 Artifact Structure

```
/holon/output/
├── changes/                    # Code changes
│   └── *.patch                 # Git-compatible patches
├── messages/                   # Text outputs
│   └── summary.md              # Main summary (PR body, etc.)
├── review/                     # Review-specific
│   └── replies.json            # [{ thread_id, body }]
└── meta/                       # Metadata (optional)
    └── manifest.json           # Status, decisions, etc.
```

### 5.3 Artifact Format: review/replies.json

```json
[
  {
    "thread_id": "PRRT_xxx",
    "body": "Fixed in the latest commit...",
    "resolved": true
  }
]
```

## 6. Publisher Behavior

### 6.1 Context-Aware Actions

Publisher determines actions based on **artifacts present** and **input context**:

| Input Context | Artifact | Publisher Action |
|---------------|----------|------------------|
| Issue only | `changes/*.patch` | Create PR |
| Issue only | `messages/summary.md` only | No action (agent likely commented directly) |
| PR exists | `changes/*.patch` | Push to PR branch |
| PR exists | `messages/summary.md` | Update PR body |
| PR exists | `review/replies.json` | Reply to review threads |

### 6.2 No Operation Distinction in Artifacts

Artifacts do **not** specify create vs update. The Publisher infers this from context:
- PR exists → update
- No PR → create

This keeps artifacts simple and context-independent.

## 7. Agent Direct Operations

### 7.1 Allowed Direct Operations

Agents MAY perform these operations directly via `gh` CLI:

- **Read operations**: `gh issue view`, `gh pr view`, `gh api`
- **Comments**: `gh issue comment`, `gh pr comment`
- **Metadata**: `gh issue edit --add-label`, `gh issue edit --add-assignee`
- **Issue management**: `gh issue create`, `gh issue close`
- **Project boards**: `gh project item-edit`

### 7.2 Disallowed Direct Operations

Agents MUST NOT perform these directly (use artifacts):

- **Code push**: `git push`
- **PR lifecycle**: `gh pr create`, `gh pr merge`, `gh pr close`

### 7.3 Auditability

For direct operations, agents SHOULD:
- Log actions in execution output
- Include summary of actions in `messages/summary.md`

## 8. Migration Path

### 8.1 Backward Compatibility

- `--mode solve` → internally maps to `--skill github/solve`
- `--mode pr-fix` → internally maps to `--skill github/solve` (with PR context)

### 8.2 Phased Migration Plan

#### Phase 0: Foundation (No Behavior Change)

**Goal**: Add skill infrastructure without changing existing behavior.

1. Create `skills/` directory at repository root with initial `github/solve/SKILL.md`
2. Add `--skill` CLI parameter (coexists with `--mode`)
3. Implement skill loading logic in `pkg/skills/`
4. If `--skill` specified, load skill; otherwise use existing `--mode` logic

**Validation**: All existing tests pass, `--mode` works unchanged.

#### Phase 1: Skill System Available

**Goal**: Make skill system fully usable while keeping `--mode` as default.

1. Implement builtin skill embedding (`pkg/skills/builtin/`)
2. Add skill loading priority (workspace > builtin)
3. Update prompt compiler to use skill content
4. Document `--skill` usage

**Validation**: `holon --skill github/solve` produces same result as `holon --mode solve`.

#### Phase 2: Unify solve + pr-fix

**Goal**: Single `github/solve` skill handles both contexts.

1. Create unified `github/solve/SKILL.md` with context detection
2. Internal mapping: `--mode solve` → `--skill github/solve`
3. Internal mapping: `--mode pr-fix` → `--skill github/solve`
4. Update Publisher to handle unified artifact output

**Validation**: Both Issue and PR workflows work with single skill.

#### Phase 3: Enable Direct Operations

**Goal**: Allow agents to use `gh` for auxiliary operations.

1. Update `SKILL.md` to declare allowed `gh` operations
2. Verify `gh` authentication works in container
3. Update Publisher to detect "agent already acted" cases
4. Add logging for direct operations

**Validation**: Agent can comment, label, etc. directly; Publisher doesn't duplicate.

#### Phase 4: Deprecate --mode

**Goal**: Complete migration.

1. Add deprecation warning to `--mode`
2. Update all documentation to use `--skill`
3. Update GitHub Action inputs
4. After 1-2 release cycles, remove `--mode`

### 8.3 Deprecation Timeline

```
v0.x.current  ──┬── Phase 0: Add skill infrastructure
                │
v0.x+1        ──┼── Phase 1: Skill system available
                │
v0.x+2        ──┼── Phase 2: Unify solve + pr-fix
                │
v0.x+3        ──┼── Phase 3: Enable direct operations
                │
v0.x+4        ──┼── Phase 4: Deprecate --mode (warning)
                │
v1.0          ──┴── Remove --mode
```

## 9. Future Extensions

### 9.1 New Platforms

Adding a new platform (e.g., GitLab):
1. Create `gitlab/solve/SKILL.md` with platform-specific instructions
2. Implement `GitLabPublisher` that handles same artifact types
3. No changes to core abstractions needed

### 9.2 New Scenarios

Adding a new scenario (e.g., PM/orchestration):
1. Create `github/orchestrate/SKILL.md`
2. Agent uses `gh` directly for most operations (create issues, update boards)
3. Only code changes go through artifacts

## 10. Open Questions

1. **Skill composition**: Can multiple skills be active in one execution?
2. **Skill versioning**: How to version skills for reproducibility?
3. **Permission enforcement**: Should skill-declared permissions be enforced, or just advisory?

## 11. References

- Issue: [#433 - Design: unify solve + pr-fix](https://github.com/holon-run/holon/issues/433)
- Parent: RFC-0001 (Holon Protocol), RFC-0002 (Agent Contract)
