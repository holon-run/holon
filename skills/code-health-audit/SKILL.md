---
name: code-health-audit
description: "Audit code-health and technical-debt signals with evidence, rank proportionate interventions from focused cleanup to broad coordinated refactors, and draft implementation-ready plans without granting implementation permission."
---

# Code Health Audit Skill

## Summary

Use this skill for a read-only repository health audit or for preparing an
evidence-backed maintenance plan. Plans may cover a focused cleanup, a
multi-phase refactor, or a broad coordinated change when that scale is
justified by the repository's maintenance cost. It defines an evidence
discipline and an output shape; it does not provide a static analyzer and it
does not authorize code changes.

## When To Use

- Finding maintainability hotspots across a repository
- Reviewing a technical-debt register for stale or duplicate entries
- Comparing refactoring candidates before implementation
- Drafting a proportionate, behavior-preserving maintenance or refactoring plan
- Preparing verification gates for an approved refactoring

## Do Not Use

- As a substitute for reviewing a concrete change set or pull request
- To declare a smell a bug, vulnerability, or performance issue without evidence
- To run a broad rewrite, formatter sweep, or dependency upgrade as an
  unapproved side effect of an audit
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

Prefer candidates with meaningful impact, strong evidence, a justified
intervention surface, manageable coupling, and a clear verification path. A
large surface is not itself a reason to reject a candidate when leaving the
problem in place has greater maintenance cost. A low-confidence high-impact
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

Maintenance or refactoring plan
1. Preserve these invariants:
2. Add or identify these verification gates:
3. Choose the right-sized intervention:
   - use a focused seam change when it addresses the root cause;
   - sequence a multi-phase change when that reduces risk;
   - use a broad coordinated refactor when smaller changes would preserve the
     maintenance problem or create temporary inconsistency.
4. Re-run focused checks, broader gates, and compare behavior as appropriate:
5. Stop or roll back if:

Authorization state
```

Plans must distinguish inspection, test-only preparation, implementation, and
cleanup. Each implementation phase should have an explicit boundary and a
verification gate; the boundary may span many files when the change is
coordinated and justified. If behavior cannot be characterized, recommend
more investigation instead of inventing a safe refactoring sequence.
