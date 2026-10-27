# Continuation cooldown fixture

`continuation_cooldown.py` is provider-only: it returns real HTTP 503 responses
with `Retry-After`, holds provider responses, and emits native Responses
reasoning plus a real gated `ExecCommand`. It never writes runtime SQLite
state or creates synthetic recovery inputs.

The shared `fault_proxy/server.py` HTTP Mock dispatches `native-cooldown` and
`recovery-cooldown` requests to this fixture before its generic fault handling.
Other scenarios retain their existing dispatch; task-result recovery still
takes precedence.

Cases are registered in
[`../continuation_interjection_manifest.json`](../continuation_interjection_manifest.json)
and executed by the existing isolated runner:

```sh
python3 scripts/docker-continuation-interjection.py \
  --image "$HOLON_IMAGE" --python-image "$PYTHON_IMAGE" \
  --expected-revision "$REVISION" --evidence-dir "$EVIDENCE" \
  --case T3772 --case R3773
```

Images must already exist locally. The runner owns the isolated containers,
internal networks and disposable volume. Do not run against a shared daemon.

## T3772

Agent `qa` receives opaque provider reasoning and executes one gated tool on A.
Helper `qa2` gets a real A HTTP 503 with a 12-second server floor, then completes
on B. Only after the canonical fence exists does the runner release the tool.
The native continuation must remain in the original active execution.

`cooldown-observation.json` records a real one-second pre-deadline interval with
unchanged HTTP, TCP and durable request-ledger observations. Query state and
`agent_state_changed` must agree on sanitized cause, model and absolute deadline.
The subsequent request must carry the original native context and exactly one
original tool result, use the exact same model, and occur no earlier than that
deadline. One completed turn, result brief, successful tool execution and source
consumption are required; parking, replay, reselection or repair do not pass.

## R3773

The source's real post-tool A request is held **after it reached the provider**.
The helper then receives real A and B HTTP failures, creating shared scope
cooldowns. Releasing the held source HTTP 503 creates a runtime-owned recovery
input; admission must retain it in `Queued` with a canonical parking deadline.
After that deadline it must dispatch and consume once with the original tool
evidence and source owner, without running the tool again.

This is the deterministic **scheduler-admission** compound-fault gate. It does
not claim to cover the narrower already-admitted/pre-send race that produces an
`Interrupted` recovery source; that race requires the Rust barrier regression.

## Fast verification

```sh
python3 -m unittest discover -s tests -p 'test_continuation*.py' -v
```

Synthetic evidence in Python unit tests checks rejection paths in the strict
oracles; it is not a replacement for executing the Docker cases.
The localhost HTTP tests use that same `fault_proxy.Mock`, including the held
source request and real A/B helper failures, rather than a test-only handler.
