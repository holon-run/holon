# Control Plane and Delegation Implementation Notes

Related handles:

- `rfc-agent-control-plane-model`
- `rfc-agent-delegation-tool-plane`
- `rfc-agent-profile-model`
- `rfc-agent-home-directory-layout`
- `rfc-agent-initialization-and-template`

## Current implementation posture

The runtime already exposes agent identity, lifecycle, active workspace,
waiting state, and child-agent lineage through the control plane. Delegation is
represented as a managed task for parent-supervised child agents, while public
named agents remain self-owned and are not parent-supervised through a task
handle.

The implementation is close to the RFC direction, but the current workflow
still relies on loaded instructions for some discipline:

- using worktrees for delegated coding work;
- requiring child agents to submit PRs instead of pushing directly to main;
- reusing an existing child agent/branch for follow-up on the same objective;
- separating parent review/merge responsibility from child implementation.

## Open gaps

1. Move more delegation workflow invariants from instruction-level convention
   into runtime-visible policy or admission checks where feasible.
2. Keep child-agent workspace mode, branch/worktree ownership, and cleanup
   lifecycle visible in task status and operator-facing projection.
3. Audit public named agent creation so ownership and supervision semantics are
   never confused with private child delegation.
4. Keep agent-home template initialization separate from later agent-local
   durable edits.
