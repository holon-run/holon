# Analysis Evidence Runtime Fixture

This fixture models a small event-ingestion runtime with clear implementation
gaps.

- `src/runtime.js`: pulls items from an in-memory queue and records events
- `src/parser.js`: parses inbound payloads without strong validation
- `src/report.js`: summarizes run status but has weak error reporting
- `src/storage.js`: appends events and reports but has no recovery or snapshot
  support

The benchmark should reward recommendations that are grounded in these concrete
limitations rather than generic platform advice.
