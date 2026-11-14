# holon-system

Host filesystem operations, workspace resolution, process execution, and PTY
support for Holon. The main crate exposes these APIs through `holon::system`.
Shared execution policy and projection types come from `holon-domain`.

Run the host system tests with
`cargo test --locked --manifest-path crates/holon-system/Cargo.toml`.
