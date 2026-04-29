# Execution Policy Surface

Decision:

- treat `host_local` as the only implemented backend in phase 1
- expose capability snapshots instead of claiming a strong sandbox
- gate only the surfaces Holon can honestly control today

Reason:

- the runtime knows projection, execution root, and provenance
- it does not yet provide strong confinement for filesystem, network, secrets,
  or child processes
- the capability boundary should be honest and inspectable
