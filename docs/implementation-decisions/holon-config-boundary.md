# Persisted configuration in holon-config

`holon-config` owns the `config.json` format, key schema, validation, and
atomic persistence. It resolves configured provider endpoints, credentials,
model routes and overrides using `holon-provider` and `holon-model-catalog`.
It also owns the Web and authentication settings derived from the file.

The application still owns `AppConfig` and its startup environment, listener,
paths, and runtime service assembly. It supplies the effective model settings
to the routing code through a borrowed view. This preserves the existing
borrowed route selection and resource accounting on the runtime path.
`RuntimeHost` continues to own reload and its publication lifecycle. The
`holon::config` module re-exports the persisted configuration API.

Run the independent suite with
`cargo test --locked --manifest-path crates/holon-config/Cargo.toml`.
Application config composition, reload, and runtime resource accounting tests
remain in the root suite.
