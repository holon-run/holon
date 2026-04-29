# Runtime Config Provider Credentials

Holon's first provider credential surface uses an owner-only
`~/.holon/credentials.json` file as the durable credential store fallback.

The provider definitions in `config.json` store only routing metadata and
credential profile ids. Raw secret material stays in the credential store and
operator-facing CLI output reports only redacted profile status.

This keeps the runtime configuration boundary usable before Holon has an
OS-keychain abstraction or daemon-mediated config mutation API. CLI mutations
therefore report `applied_via: "offline_store"` for now. The later daemon path
should use the same persisted stores and validation rules, then refresh the
running provider registry for future turns.
