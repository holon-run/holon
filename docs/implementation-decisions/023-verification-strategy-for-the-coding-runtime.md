# Verification Strategy For The Coding Runtime

Decision:

- keep four test layers:
  - unit tests
  - integration tests
  - live Anthropic-compatible tests
  - fixture-based regression tests

Reason:

- coding behavior breaks differently at different layers
- live tests catch provider/runtime mismatches
