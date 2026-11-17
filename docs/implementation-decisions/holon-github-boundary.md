# Repository access in holon-github

`holon-github` owns authenticated repository API requests and the download,
bounded extraction, and caching of repository archives. Its callers supply the
HTTP clients, configured credential, environment variable names, and cache
locations appropriate to their entry points.

`holon-agent-templates` owns template and skill discovery, selection, and
installation. The application renders GitHub errors on the control surface.
The Copilot model integration uses the provider crate.

The GitHub client and the application share the error redaction function
exported by `holon-domain` through its `error-sanitization` feature. This keeps
URL parsing out of the default domain crate build.

Run the repository-access tests with
`cargo test --locked --manifest-path crates/holon-github/Cargo.toml`.
