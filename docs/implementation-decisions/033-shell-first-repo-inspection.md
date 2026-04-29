# Shell-First Repo Inspection

Decision:

- retire provider-facing `Read`, `Glob`, and `Grep` from the normal model tool
  surface
- make `exec_command` the primary repo-inspection primitive
- truncate oversized command output before reinjection

Reason:

- one inspection primitive is easier for models to use consistently
- shell-first inspection transfers better across execution backends
