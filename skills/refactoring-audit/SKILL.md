---
name: refactoring-audit
description: "Audit maintainability and technical-debt signals with evidence, rank refactoring candidates, and draft incremental behavior-preserving plans without implementing changes."
---

# Refactoring Audit Skill

## Summary

Use this skill for a read-only repository health audit or for preparing a
small, safe refactoring plan. It defines an evidence discipline and an output
shape; it does not provide a static analyzer and it does not authorize code
changes.

## When To Use

- Finding maintainability hotspots across a repository
- Reviewing a technical-debt register for stale or duplicate entries
- Comparing refactoring candidates before implementation
- Drafting an incremental, behavior-preserving refactoring plan
- Preparing verification gates for an approved refactoring

## Do Not Use

- As a substitute for reviewing a concrete change set or pull request
- To declare a smell a bug, vulnerability, or performance issue without evidence
- To run a broad rewrite, formatter sweep, or dependency upgrade
- To infer permission to edit code, merge changes, or publish findings

## Evidence Rules

1. **Name the scope.** Record the repository, paths, time window, and
   read/write authorization.
2. **Prefer direct evidence.** Cite a file and symbol or line range, relevant
   tests, history, configuration, or measured behavior.
3. **Separate certainty levels.** Use `confirmed`, `signal`, `hypothesis`, or
   `unknown`; explain what would raise confidence.
4. **Avoid metric theater.** A large file, high churn count, or duplicated
   string is a lead. Connect it to coupling, change friction, defect history,
   testability, or another observed impact before ranking it highly.
5. **Record counter-evidence.** Note stable interfaces, strong tests, low
   churn, generated code, or other reasons not to refactor now.
6. **Keep findings atomic.** One finding should have one primary location,
   impact statement, and next step. Link related findings instead of merging
   unrelated smells.

## Audit Taxonomy

Use only categories supported by evidence:

- **Boundary:** responsibilities, ownership, or module seams are unclear.
- **Duplication:** behavior or policy is repeated and changes must stay in sync.
- **Complexity:** control flow or state interactions make behavior difficult to
  understand or verify.
- **Coupling:** a local change requires broad knowledge or coordinated edits.
- **Testability:** important behavior lacks a stable, focused verification seam.
- **Consistency:** neighboring code follows materially different conventions
  that increase maintenance cost.
- **Lifecycle:** compatibility, migration, cleanup, or deprecation paths are
  incomplete or indefinitely retained.
- **Observability:** failures or state transitions cannot be diagnosed with the
  available evidence.

Do not use the category as proof. Explain the observed pattern and impact.

## Candidate Ranking

Rank candidates using a short qualitative score:

| Dimension | Question |
| --- | --- |
| Impact | What maintenance cost or risk is reduced? |
| Confidence | How directly is the claim supported? |
| Surface | How many files, interfaces, and owners are involved? |
| Coupling | How many callers, integrations, or invariants can be affected? |
| Regression risk | What can silently change? |
| Verification readiness | Can behavior be checked before and after? |

Prefer candidates with meaningful impact, strong evidence, bounded surface,
low coupling, and a clear verification path. A low-confidence high-impact
item belongs in an investigation queue, not at the top of an implementation
queue.

## Report Shape

Produce:

```text
Scope and baseline

Evidence matrix
- ID:
- Category:
- Location:
- Observation:
- Evidence:
- Impact:
- Confidence:
- Counter-evidence or gaps:

Priority order
- Candidate:
- Why now:
- Dependencies:

Safe refactoring plan
1. Preserve these invariants:
2. Add or identify these verification gates:
3. Make the smallest seam change:
4. Re-run focused checks and compare behavior:
5. Stop or roll back if:

Authorization state
```

Plans must distinguish inspection, test-only preparation, implementation, and
cleanup. Each implementation step should have a narrow diff boundary and a
verification gate. If behavior cannot be characterized, recommend more
investigation instead of inventing a safe refactoring sequence.
