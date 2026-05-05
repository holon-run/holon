# Policy and Execution Boundary Implementation Notes

Related handles:

- `rfc-default-trust-auth-and-control`
- `rfc-execution-policy-and-virtual-execution-boundary`
- `rfc-instruction-loading`
- `rfc-workspace-binding-and-execution-roots`
- `rfc-workspace-entry-and-projection`

## Current implementation posture

The runtime projects trust, origin, workspace binding, execution roots, and
process-execution policy as first-class concepts. It distinguishes the active
workspace from shell `cwd` and reports host-local containment limitations rather
than pretending the local backend is a hard sandbox.

That explicit reporting is useful, but it is not the same as complete
enforcement. Current policy posture should be treated as partial until
admission and execution checks consistently gate sensitive actions.

## Open gaps

1. Strengthen auth/admission checks for control-plane and mutating workspace
   operations.
2. Keep host-local execution limitations visible in operator projection and
   tool receipts.
3. Avoid policy drift between instruction-level behavior and runtime-enforced
   behavior.
4. Add tests for denied operations, not only successful operations.
5. Preserve provenance when instructions come from system/developer,
   agent-home, workspace `AGENTS.md`, operator input, or external channels.

## Verification direction

Useful checks should exercise real runtime surfaces:

- workspace binding cannot be changed implicitly by shell `cd`;
- tool calls cannot write outside the active workspace contract;
- lower-trust external input cannot become operator-equivalent authority;
- access-mode and occupancy failures are visible rather than silently ignored.
