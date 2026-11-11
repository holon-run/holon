---
name: citation-verifier
description: "Verify that research claims are supported by traceable sources, while preserving scope, freshness, limitations, and contradictions."
---

# Citation Verifier Skill

## Purpose

Use this skill after sources have been collected and before a research brief is
delivered. It checks the relationship between a claim and its cited evidence;
it does not decide whether a conclusion is desirable and it does not turn
search results into evidence.

## Required inputs

- A claim list with stable IDs, wording, and the importance of each claim.
- A source ledger with stable IDs, title, publisher or author, locator,
  publication/update date when available, access date, scope, and limitations.
- The relevant excerpt, table, section, or reproducible locator for each
  claim/source pair. A homepage or search snippet is not a sufficient locator.

If an input is missing, report the missing field and mark affected claims
`unverified`; do not fill it with a guessed citation.

## Verification procedure

1. Normalize each claim so that it has one testable proposition, a defined
   population or object, a time window, and any important qualifier.
2. Check **entailment**: does the cited passage support the exact proposition,
   or only a weaker/narrower statement? Record a short paraphrase, not an
   invented quotation.
3. Check **scope**: compare geography, population, version, methodology,
   definitions, and date. A source can be credible and still be out of scope.
4. Check **freshness**: distinguish publication date, last update, and access
   date. Flag time-sensitive claims whose evidence may have changed.
5. Check **source quality** separately from entailment: identify primary,
   secondary, official, self-reported, anonymous, or untraceable material and
   record the reason it matters.
6. Search for counter-evidence when the claim is material, disputed, surprising,
   or supported by only one weak source. Preserve conflicts rather than
   collapsing them into an average.
7. Assign one terminal status:
   `supported`, `partially-supported`, `contradicted`, `unverified`, or
   `blocked`. Include the reason, source IDs, and the next check if unresolved.

## Output contract

Return a claim/evidence matrix with at least these columns:

| claim_id | claim | source_ids | locator | scope/date check | source quality | counter-evidence | status | reason |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |

Also return:

- `verified_at`: the date/time of the verification pass;
- `coverage`: material claims checked versus total material claims;
- `conflicts`: a list of unresolved or context-dependent conflicts;
- `unverified`: claims that must not appear as established facts;
- `next_checks`: the smallest useful follow-up actions.

Keep the distinction explicit:

- **supported** means the cited material entails the claim within its scope;
- **partially-supported** means only a qualified or narrower claim is entailed;
- **contradicted** means credible evidence materially conflicts with the claim;
- **unverified** means evidence is missing, inaccessible, or does not entail it;
- **blocked** means the check cannot proceed because access or required input is
  unavailable.

## Safety rules

- Never invent a URL, DOI, title, date, quotation, page number, or source
  identity.
- Never cite a source you did not inspect when the task requires claim-level
  verification. Label operator-provided excerpts as excerpts.
- Never remove a contradictory source merely because it is inconvenient.
- Do not use citation count, search rank, repository stars, or model agreement
  as a substitute for entailment and scope checks.
- For high-risk topics, report verification limits and require qualified human
  review before the result is used as advice.
