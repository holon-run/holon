---
name: interlinked-markdown-wiki
description: "Maintain a file-first, auditable Markdown knowledge base with stable pages, sources, wikilinks, backlinks, indexes, freshness, and reviewable health reports."
---

# Interlinked Markdown Wiki

Use this skill when an agent must maintain a navigable knowledge base rather
than merely write an isolated document. The knowledge base is a set of
reviewable Markdown files whose links, sources, time semantics, and unresolved
questions remain inspectable by a person and by simple repository tools.

## Operating contract

- Treat the knowledge base as a bounded workspace. Read only paths and sources
  the operator has made available; do not discover or copy unrelated data.
- Preserve source facts. Never overwrite an immutable source snapshot, silently
  delete a page, or turn an inference into an organization fact.
- Default to read-only analysis or a draft. When a write is authorized, make
  the smallest additive or revisioned change and record what changed.
- Keep facts, interpretations, proposed changes, and unknowns in separate
  sections. Cite the source page or snapshot for every material claim.
- Prefer deterministic Markdown and plain-text indexes over a database or
  opaque embedding store. A graph or vector index may be an optional derived
  view, never the only copy of knowledge.
- Treat conflicting sources as unresolved until a human or an explicitly
  authoritative source settles the conflict.

## Knowledge package layout

Use an existing workspace layout when one is already established. Otherwise
create or propose this layout before writing:

```text
knowledge/
├── pages/              # curated topic pages, one stable page per concept
├── sources/            # immutable or append-only source snapshots
├── maintenance/       # page ownership, review cadence, and open questions
├── indexes/            # generated navigation, backlinks, and unresolved links
├── reports/            # dated health and change reports
└── activity/           # append-only maintenance events
```

Do not create all directories merely as placeholders. Create only the parts
needed for the requested operation and explain any proposed additions.

## Page and source shapes

Every curated page should begin with frontmatter like:

```yaml
---
id: topic-slug
title: Human-readable title
type: topic
status: draft
aliases: []
updated_at: 2026-01-01T00:00:00Z
review_after: 2026-04-01
sources:
  - source-id
---
```

Required page rules:

- `id` is stable, lowercase, and unique; renaming a title does not change it.
- `status` is one of `draft`, `review`, `published`, `stale`, or `archived`.
  Archiving requires an explicit human decision and a replacement or reason.
- `updated_at` records the page revision time, not the source publication time.
- `review_after` is a review prompt, not proof that the page became false.
- `sources` names source records; do not cite a source that was not captured or
  otherwise made available.
- Use `[[topic-slug]]` for internal links. Use an alias only when it resolves
  unambiguously, such as `[[topic-slug|display text]]`.

Source records should preserve the original text or a bounded snapshot, with:

```yaml
---
id: source-id
kind: document
title: Source title
origin: https://example.invalid/original
retrieved_at: 2026-01-01T00:00:00Z
published_at: UNKNOWN
content_hash: sha256:UNKNOWN
authority: stated | inferred | unknown
---
```

If a source changes, append a new snapshot or revision record. Do not make a
mutable page appear to be an immutable source.

## Maintenance workflow

1. **Inventory:** identify the requested workspace, existing layout, authority
   boundary, and the sources actually available.
2. **Capture:** preserve new source material with retrieval time and provenance
   before synthesizing it. Mark missing metadata `UNKNOWN`.
3. **Plan:** choose create, revise, link, review, or report mode. List pages and
   indexes that may change before making writes.
4. **Synthesize:** write a concise page that separates source-backed facts,
   interpretation, decisions, open questions, and conflicting evidence.
5. **Link:** add links to related pages and update the backlink/index view. Keep
   links stable even when display titles change.
6. **Audit:** detect broken links, duplicate IDs, orphan pages, stale review
   dates, missing citations, source drift, and unrecorded changes.
7. **Report:** produce a dated maintenance event and a human-readable health
   report. State what was changed, what was not changed, and what needs review.

## Index and health report

An index is a derived view and may be regenerated. At minimum it should make
these conditions visible:

- page ID, title, status, last update, and review date;
- outgoing links and backlinks;
- unresolved link targets and duplicate IDs;
- pages with no source, no inbound link, or no recent review;
- source records with missing retrieval/authority metadata;
- conflicting claims or pages awaiting human review.

Use counts only as evidence from the current snapshot. A zero count means
"none found in this scan", not "none exists".

## Safe change and review boundaries

- For uncertain synthesis, create or update a `draft` page and add an item to
  `maintenance/review-queue.md` instead of publishing it.
- For destructive requests, prepare a proposed deletion/archive record and ask
  for explicit confirmation; do not remove the only copy of a source.
- For access-controlled or personal data, preserve the access boundary and
  record that the source was unavailable rather than requesting broader access.
- For automated recurring maintenance, append an activity event with actor,
  timestamp, changed paths, source IDs, and unresolved risks.
- A health report is not an approval, legal conclusion, product decision, or
  claim that the knowledge base is complete.

## Completion checklist

Before declaring a maintenance task complete, verify:

- every changed page has stable frontmatter and source references;
- all `[[wikilink]]` targets resolve or are listed as unresolved;
- backlinks/indexes reflect the changed links;
- source snapshots were not rewritten or silently omitted;
- freshness, conflicts, unknowns, and review work are explicit;
- the activity log and dated health report describe the result.
