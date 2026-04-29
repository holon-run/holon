# Shared Tool Error Envelope

Decision:

- standardize tool failures on one shared envelope
- keep `kind`, `message`, optional `details`, optional `recovery_hint`, and
  `retryable`

Reason:

- freeform strings were too thin for headless recovery
- the agent needs enough structure to distinguish contract and execution
  failures
