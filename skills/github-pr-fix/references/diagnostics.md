# Diagnostics and Best Practices

Diagnostic guidance for `github-pr-fix`.

## Diagnostic Confidence Levels

When reporting CI/test diagnosis, label confidence explicitly:

- **High**: Root cause is directly supported by logs/diff/evidence.
- **Medium**: Most evidence supports one cause, with minor uncertainty.
- **Low**: Competing explanations remain plausible.

When confidence is low:
1. Record conflicting evidence.
2. List plausible alternatives.
3. State what additional data would disambiguate.
4. Avoid claiming definitive remediation.

## Manifest-Aware Diagnosis

- Use `${GITHUB_CONTEXT_DIR}/manifest.json` as source of truth for available artifacts.
- If key artifacts are missing (`status != present`), downgrade confidence and explain impact.
- Do not infer absent context from expected filenames.

## Verification Discipline

- Prefer runnable build/test evidence over static reasoning.
- If full verification is impossible, report exact attempted commands and blockers.
- Keep summary actionable: diagnosis, change applied, residual risk, and next step.
