# Fallback fault fixture

See the [acceptance guide](../../../../docs/testing/fallback-storm-acceptance.md)
for cases, commands, verdicts, isolation, evidence, and remaining recovery gates.
Use `scripts/docker-fallback-storm.py` from the repository root to own lifecycle;
do not connect this fixture to the shared runtime.

`server.py` uses only the Python standard library and the sibling
`openai_stub/server.py` response helpers. Its process modes are:

- `mock`: successful Responses payloads, HTTP 429, or HTTP 503.
- `proxy`: two TCP listeners, with pass-through, RST, and header/body/SSE-idle
  blackholes.
- `controller`: a network-local process for authenticated HTTP requests and
  read-only evidence collection.
- `request`: one JSON request envelope from stdin.
- `database`: one read-only SQLite snapshot from the isolated case volume,
  including row counts and explicit truncation metadata.

Faults are enabled only after healthy bootstrap by writing `/tmp/qa-fault.json`
inside the mock and proxy containers. The runner supplies that file; no host
credential or configuration file is used. Fixture directories and Python files
must be readable by container UID 10001.
