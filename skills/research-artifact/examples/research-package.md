# Illustrative Research Package

> This is a format example only. The records below are intentionally marked
> illustrative and must not be used as evidence for a real decision.

## question-card

- `question_id`: `Q-001`
- `decision`: Should the team investigate option A before option B?
- `audience`: Project owner
- `scope`: Illustrative comparison of two options
- `time_window`: `2026-01-01` to `2026-03-31`
- `acceptance_test`: Identify at least one traceable strength, limitation, and
  unresolved risk for each option.
- `status`: `paused`

## search-plan

- `sub_questions`: cost, implementation risk, and evidence gaps
- `source_types`: operator-provided documents and primary project records
- `coverage_note`: No external retrieval was performed for this example.
- `exclusions`: No claims about real products or markets are made.

## source-ledger

| source_id | title | locator | source_type | accessed | scope | limitations |
| --- | --- | --- | --- | --- | --- | --- |
| `SRC-001` | Operator-provided comparison excerpt | `operator://example/source-001` | operator-provided | `2026-09-28` | option A | illustrative excerpt; not independently verified |
| `SRC-002` | Operator-provided risk excerpt | `operator://example/source-002` | operator-provided | `2026-09-28` | option B | illustrative excerpt; not independently verified |

## claim-matrix

| claim_id | claim | source_ids | locator | status | confidence | reason |
| --- | --- | --- | --- | --- | --- | --- |
| `CLM-001` | Option A has one identified implementation advantage. | `SRC-001` | excerpt heading `advantage` | `unverified` | low | Example source was not independently inspected. |
| `CLM-002` | Option B has one identified delivery risk. | `SRC-002` | excerpt heading `risk` | `unverified` | low | Example source was not independently inspected. |

## decision-brief

**Answer:** Paused. The example does not support choosing either option.

**Evidence:** Two operator-provided excerpts are recorded, but both claims
remain `unverified` until the underlying material is inspected.

**What would change the conclusion:** Inspect the original records and compare
their dates, scope, methods, and counter-evidence.

## follow-up-queue

| follow_up_id | priority | action | close condition |
| --- | --- | --- | --- |
| `FU-001` | high | Inspect the original material behind `SRC-001` and `SRC-002`. | Both sources have traceable locators and scope checks. |

## provenance

- `artifact_version`: `1`
- `created_for`: format illustration
- `verified_at`: not applicable
- `external_writes`: none
