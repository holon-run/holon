# Provider adapter behavior matrix

This matrix records the adapter boundary before any helper convergence. The
rows describe the behavior that the default deterministic test suites must
protect. A provider-specific protocol remains local when its request shape,
response mapping, or error semantics differ.

| Adapter | Request boundary | Success mapping | Non-success / decode errors | Timeout / cancellation | Size limits | Stream / tools |
| --- | --- | --- | --- | --- | --- | --- |
| `decision-clef` | HTTP `POST` with Clef `state` + `questions`; `choice`, `score`, and `noul` are selected through request metadata. | Untagged Clef answers map to `Select`; choice labels resolve to candidate values; provenance is `cloudflare-clef`. | Non-2xx is `DecisionError::Provider`; invalid JSON is `Serialization`; missing or invalid answer is `InvalidResponse`. | Request and bounded body reads use the caller context; timeout is `DeadlineExceeded`, cancellation is `Cancelled`. | Request and response bodies are bounded and return `ResourceExhausted`. | No streaming or tool-call contract; Clef question criteria remain provider-local. |
| `decision-jev` | HTTP `POST` with Jev/System One `state` + typed `questions` and Jev gateway headers. | Typed choice answers map to `Select`; score/boolean answers are explicit `Abstain` outcomes for the choice adapter. | Non-2xx is `Provider`; invalid JSON is `Serialization`; missing/unknown answers are `InvalidResponse`. | Request and bounded body reads honor timeout and cancellation as `DeadlineExceeded` / `Cancelled`. | Request and response bodies are bounded and return `ResourceExhausted`. | No streaming or tool-call contract; Jev answer types remain provider-local. |
| `decision-openai` | HTTP `POST` to normalized Chat Completions endpoint; decision request is embedded in a JSON-only chat prompt. | First chat message content is decoded as `DecisionResponse`; missing provenance is filled with `openai-compatible`. | Non-2xx is `Provider`; invalid outer/content JSON is `Serialization`; empty content is `InvalidResponse`. | Request and bounded body reads honor timeout and cancellation as `DeadlineExceeded` / `Cancelled`. | Request and response bodies are bounded and return `ResourceExhausted`. | This decision adapter is non-streaming and does not expose model tool calls. |
| `decision-openai-decisions` | HTTP `POST` to normalized `/v1/decisions`; provider-native `questions` carry predicate, choice, or score semantics. | Provider answers map to `Select` with provider evidence; candidate IDs resolve against the original request. | Non-2xx is `Provider`; invalid JSON is `Serialization`; missing/unsupported answers are `InvalidResponse`. | Request and bounded body reads honor timeout and cancellation as `DeadlineExceeded` / `Cancelled`. | Request and response bodies are bounded and return `ResourceExhausted`. | No streaming or tool-call contract; Decisions question types remain provider-local. |
| `decision-local-onnx` | No network request. Constructor validates a local model directory, manifest, path containment, and optional SHA-256 checksum. | With the `onnx` feature, inference returns local evidence and `local-onnx` provenance. | Missing/invalid files and manifests fail during construction with `LocalOnnxError`; without `onnx`, `decide` returns `Provider(FeatureDisabled)`. | Context is checked before inference; no HTTP timeout or cancellation mapping is promised for the blocking inference body. | Local file and manifest boundaries apply; no HTTP body limits. | No stream or tool-call contract; this is an embedded zero-egress provider. |
| `holon-model-client` OpenAI-compatible transport | HTTP `POST` to `/chat/completions`; provider-neutral messages, tools, response format, extensions, and context headers are lowered at this boundary. | First non-streaming choice is decoded into `CompletionResponse`; tool calls, usage, reasoning, and raw provider data are retained. | Non-2xx is `ClientError::Http` with status/request ID/body; transport failures are `Transport`; JSON shape failures are `Decode`. | Request timeout is delegated to `reqwest` and reported as transport failure; cancellation is owned by the host/runtime context, not normalized here. | No client-side body-size contract yet. | Streaming and continuation are explicitly `Unsupported`; tool definitions, assistant tool calls, and tool results are supported. |

## Deterministic contract test rules

- Default `cargo test` paths use loopback fixtures, pure response fixtures, or
  temporary local model directories only. They do not require credentials,
  external model endpoints, or network access.
- Live provider tests remain opt-in (`#[ignore]`) and are run separately when
  credentials and an external endpoint are intentionally supplied.
- Contract tests cover the common lifecycle where the adapter owns it:
  request validation, successful response mapping, non-success status,
  response decoding, bounded request/response bodies, timeout, and
  cancellation. Provider-specific tests additionally lock each wire shape and
  response mapping.
- The matrix intentionally does not imply a shared helper. The common HTTP
  polling code currently maps errors through different public contracts, so
  helper extraction remains out of scope until a later change can prove those
  semantics equivalent.
