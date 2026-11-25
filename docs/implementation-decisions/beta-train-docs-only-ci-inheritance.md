# Beta train: docs-only main CI inheritance

## Choice

`beta-train.yml` Preflight no longer reads a single `conclusion` value. It calls
`scripts/beta_train_ci_gate.sh`, which classifies the candidate head's main CI as
`success` / `pending` / `blocked` / `missing` from the run's `status` first, and
allows exactly one permissive `missing` branch: the head has no CI run because
every file changed since the last green main CI is outside `ci.yml`'s
`on.push.paths`. Everything else skips, and the gate reads that whitelist from
`ci.yml` at runtime instead of keeping a second copy.

## Reason

`ci.yml` filters `push` by a positive paths whitelist. A docs-only commit
therefore creates no CI run at all, and the old `conclusion // "none"` shape
collapsed "run still in progress", "run never created", and "run failed" into
one non-`success` string that told operators to fix a failure that did not
exist. Because the missing run is structural, the train skipped every day until
the next code push; a wording fix alone would not restore the daily cadence.

## Preserved boundary

Inheritance replaces only the "main CI is green" evidence. The candidate still
goes through the Release E2E attestation before any tag, and `pending` /
`blocked` / unprovable `missing` windows stay fail-closed, so the gate never
trades an unverified candidate for schedule. `docs/website/.tools/**` is not
special-cased: `docs-site.yml` verifies the site independently and `release.yml`
does not build it. If the website ever joins the release-critical path, that
assumption must be revisited.
