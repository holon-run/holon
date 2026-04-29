# Failure Artifact Normalization

Decision:

- normalize operator-facing failures across provider, runtime, and task paths
  into one shared `FailureArtifact` contract
- keep bounded metadata as the stable interface

Reason:

- tooling should classify outcomes from one stable field
- a shared contract reduces confusion between provider, runtime, and task
  failures
