# Diagnostics and Best Practices

Diagnostic confidence levels and common contract rules for the github-solve skill.

## Diagnostic Confidence Levels

When diagnosing CI failures, communicate your confidence:

- **High**: Root cause is clearly identified, all evidence points to the same conclusion
- **Medium**: Root cause is likely but not 100% certain, some evidence supports diagnosis
- **Low**: Significant conflicting evidence exists (e.g., tests pass locally but fail in CI)

When confidence is **low** or fix_status is **"not-applicable"**:

1. Document all conflicting evidence
2. List alternative explanations
3. Request specific investigation
4. Consider `fix_status: "unverified"` instead of "not-applicable"

## Common Contract Rules

Use the common contract rules without modification. The common contract provides:

- Sandbox environment rules and physics
- Developer role expectations
- Output artifact requirements
- Testing and verification guidelines

For detailed information on the common contract, see the main project documentation.
