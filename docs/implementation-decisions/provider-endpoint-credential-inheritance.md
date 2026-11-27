# Protocol sub-endpoints inherit the main provider credential

Decision:

- A provider entry owns one credential per credential scope. Protocol
  sub-endpoints that share that scope — the `(route_provider, endpoint)` pairs
  other than `default` whose built-in credential environment variables are a
  subset of the main entry's, such as `opencode-go-messages`,
  `tencent-tokenhub-responses`, or the OAuth `github-copilot-chat-completions` —
  resolve the credential configured for the main entry instead of their own
  built-in environment variable.
- Sub-endpoints with an independent credential scope keep their own key. The
  built-in plan endpoints (`dashscope-token-plan`, `dashscope-coding-plan`,
  `stepfun-plan`, `byteplus-coding`) bill a different account, so inheriting the
  main entry credential there would silently spend the wrong account.
- Inheritance applies only when the main entry has an explicitly configured
  credential (`providers.<id>.auth` or `providers.<id>.endpoints.default.auth`).
  Without one, the sub-endpoint keeps its built-in default and never gains a
  credential on its own.
- An explicit credential for the sub-endpoint always wins: its own
  `providers.<sub-endpoint-id>.auth`, or `providers.<provider>.endpoints.<endpoint_id>.auth`
  for that endpoint.
- Inheritance never crosses provider boundaries and never copies a credential
  between unrelated providers.

Reason:

- The built-in registry already gave protocol sub-endpoints the same
  environment variable as their main entry, but a credential saved through the
  Web GUI or CLI credential store lands in `providers.<id>.auth`. Without
  inheritance the sub-endpoint kept looking for the environment variable, so
  routes such as `@messages` and `@responses` failed authentication or forced
  the operator to configure the same credential twice.
- The rule is stated in terms of the `(provider, endpoint)` pair, so the
  effective credential of one endpoint stays inside its provider account.
- The credential scope is derived from the registry instead of a per-endpoint
  list: a sub-endpoint inherits when its built-in env vars are all main-entry
  env vars (two empty lists, the OAuth case, count as shared). Adding a plan
  endpoint with its own key therefore keeps it out of inheritance by
  construction.

Boundary:

- Provider visibility, protocol priority, and which endpoints appear in the
  first-class provider list are unchanged.
- Inheritance only reads explicitly configured credentials; it does not change
  how a stored entry materializes its own `auth`. A stored provider entry
  without a credential block still materializes its default auth as before.
- The sibling work on converging internal protocol sub-endpoints may change
  which sub-endpoints exist; the inheritance rule follows the registry and
  needs no per-endpoint list.
