# Analysis Runtime Fixture

This fixture models a small headless runtime with three main responsibilities:

- `src/runtime.js`: pulls work from an in-memory queue and writes brief results
- `src/storage.js`: persists events and briefs to JSONL files
- `src/http.js`: normalizes incoming HTTP requests into runtime events

The next milestone for this project should be chosen based on the code, not on
generic agent-platform advice.
