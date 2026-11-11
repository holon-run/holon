---
name: research-artifact
description: "Create a compact, auditable research package that keeps the question, source ledger, claim matrix, decision brief, and follow-up queue linked by stable IDs."
---

# Research Artifact Skill

## Purpose

Use this skill to turn a completed or paused research pass into a package that
another person can inspect and resume. The package is a record of evidence and
uncertainty, not an implicit knowledge base and not permission to publish.

## Package layout

Create a directory or document with these sections, in this order:

1. `question-card` — question ID, decision, audience, scope, definitions,
   time window, acceptance test, assumptions, and owner.
2. `search-plan` — sub-questions, source types, queries or collection methods,
   date bounds, coverage notes, and deliberate exclusions.
3. `source-ledger` — one row per source with `source_id`, title, author or
   publisher, locator, source type, publication/update date, access date,
   relevant scope, extracted evidence, and limitations.
4. `claim-matrix` — one row per material claim with `claim_id`, wording,
   source IDs, exact locator, status, confidence, counter-evidence, and reason.
   Run `citation-verifier` before marking a claim supported.
5. `decision-brief` — answer first, then evidence, competing explanations,
   caveats, confidence, and what would change the conclusion.
6. `follow-up-queue` — unresolved questions, priority, blocking reason, next
   action, and the condition for closing each item.
7. `provenance` — artifact version, creator, verification time, source access
   notes, and any operator approvals.

Use stable IDs such as `Q-001`, `SRC-001`, `CLM-001`, and `FU-001`. Keep IDs
stable when the package is revised; append a revision note instead of silently
rewriting history.

## Markdown and JSONL contract

Markdown is the human-readable default. When machine processing is useful,
add JSONL records with one record per line and a `record_type` field:

```json
{"record_type":"source","source_id":"SRC-001","locator":"...","accessed":"YYYY-MM-DD"}
{"record_type":"claim","claim_id":"CLM-001","source_ids":["SRC-001"],"status":"unverified"}
```

JSONL is an index of the Markdown package, not a second contradictory source of
truth. Every claim record must link to source IDs, and every supported claim
must have a locator. Do not emit fabricated placeholder evidence as if it were
verified.

## Production procedure

1. Start from the smallest package that answers the decision; do not create a
   broad archive by default.
2. Record sources before prose conclusions and preserve inaccessible/negative
   results as limitations.
3. Run `citation-verifier` and carry its statuses and conflicts into the
   decision brief.
4. Mark the package `complete`, `paused`, or `blocked`; never use `complete`
   when material claims remain unverified without an explicit caveat.
5. Put the next useful action in `follow-up-queue`, including whether operator
   input or external access is required.

## Permissions and safety

- Default output is a local report in the current response or an explicitly
  requested workspace location. Do not write project files, upload sources,
  publish results, or create a persistent archive without exact authorization.
- Preserve original source locators and access dates. Do not copy long
  copyrighted passages; summarize and point to the locator.
- Keep facts, interpretations, recommendations, and open questions in separate
  sections. A polished brief does not upgrade `unverified` evidence.
- If sources conflict, retain both records and document the dimensions of the
  conflict. If a source cannot be inspected, keep the claim `blocked` or
  `unverified`.

## Completion checklist

- [ ] Question and acceptance test are explicit.
- [ ] Material sources have stable IDs and traceable locators.
- [ ] Material claims link to source IDs and verifier statuses.
- [ ] Counter-evidence and limitations are preserved.
- [ ] Freshness and access dates are recorded where relevant.
- [ ] The brief states confidence, unresolved questions, and next actions.
- [ ] No external write or publication occurred without authorization.
