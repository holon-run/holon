# Workspace Tool Boundary

Decision:

- restrict workspace file tools to the configured workspace root
- enforce that restriction in the tool layer itself
- use lexical path normalization rather than partial `canonicalize` checks

Reason:

- coding tools need a hard path boundary that survives model mistakes
- path policy must work even when the target path does not exist yet
