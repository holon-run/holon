# Agent templates and skills crate ownership

`holon-agent-templates` owns template and skill discovery, package validation,
installation, and local library metadata. Template installation uses the same
skill installation code as the independent skill management entry points.
GitHub requests and archive downloads use `holon-github`.

The application supplies the embedded templates and skills from their
repository paths. It implements filesystem measurement and resource admission
through the package I/O interface, including lazy directory iteration and
template parser scratch allowances. The application also retains the active
skill registry, activation events, credential profiles, and the database
transactions and retry schedule for remote template synchronization.

Run the independent suite with
`cargo test --locked --manifest-path crates/holon-agent-templates/Cargo.toml`.
