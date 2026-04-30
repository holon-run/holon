# Implementation Decisions

This directory records short implementation-specific decisions that were chosen
during development but do not belong in code comments or canonical RFCs.

Use this directory when:

- multiple plausible implementation choices existed
- the final choice matters for future maintenance
- the reason is not obvious from the code alone
- the reason does not belong in a broader product or runtime RFC

Prefer one decision per file. Keep each note short and focused:

- what was chosen
- why that option won
- what boundary or tradeoff it preserves

Current decision notes:

- [001 Anthropic Compatibility](./001-anthropic-compatibility.md)
- [002 Context V1 And Compaction](./002-context-v1-and-compaction.md)
- [003 Background Task V1](./003-background-task-v1.md)
- [004 Policy Boundary V1](./004-policy-boundary-v1.md)
- [005 Execution Policy Surface](./005-execution-policy-surface.md)
- [006 Local TUI Surface](./006-local-tui-surface.md)
- [007 Local Daemon Lifecycle Surface](./007-local-daemon-lifecycle-surface.md)
- [008 Agent-Level Model Override](./008-agent-level-model-override.md)
- [009 Local Operator Troubleshooting Workflow](./009-local-operator-troubleshooting-workflow.md)
- [010 External Event Surfaces](./010-external-event-surfaces.md)
- [011 Multi-Agent Host Shape](./011-multi-agent-host-shape.md)
- [012 Closure Outcome Versus Runtime Status](./012-closure-outcome-versus-runtime-status.md)
- [013 Continuation Resolution](./013-continuation-resolution.md)
- [014 Tool Calling Shape](./014-tool-calling-shape.md)
- [015 Objective State Retired In Favor Of Work-Queue Truth](./015-objective-state-retired-in-favor-of-work-queue-truth.md)
- [016 Workspace Tool Boundary](./016-workspace-tool-boundary.md)
- [017 Shell Exposure Model](./017-shell-exposure-model.md)
- [018 Sleep As A Terminal Tool Round](./018-sleep-as-a-terminal-tool-round.md)
- [019 Subagent V1 Shape](./019-subagent-v1-shape.md)
- [020 Context For Coding Follow-Ups](./020-context-for-coding-follow-ups.md)
- [021 Shared Tool Error Envelope](./021-shared-tool-error-envelope.md)
- [022 Turn Terminal Settlement Before Closure](./022-turn-terminal-settlement-before-closure.md)
- [023 Verification Strategy For The Coding Runtime](./023-verification-strategy-for-the-coding-runtime.md)
- [024 Main Session Tool Loop Limits](./024-main-session-tool-loop-limits.md)
- [025 Workspace Binding Model](./025-workspace-binding-model.md)
- [026 OpenAI Codex Transport Contract](./026-openai-codex-transport-contract.md)
- [027 Provider Retry Classification](./027-provider-retry-classification.md)
- [028 Provider Attempt Timeline](./028-provider-attempt-timeline.md)
- [029 Failure Artifact Normalization](./029-failure-artifact-normalization.md)
- [030 Local Skills V1](./030-local-skills-v1.md)
- [031 Operator-Facing Token Usage](./031-operator-facing-token-usage.md)
- [032 Tool Schema Source Of Truth](./032-tool-schema-source-of-truth.md)
- [033 Shell-First Repo Inspection](./033-shell-first-repo-inspection.md)
- [034 Work-Item Rollout Remains Message-Driven First](./034-work-item-rollout-remains-message-driven-first.md)
- [035 Work-Queue Prompt Projection](./035-work-queue-prompt-projection.md)
- [036 Work-Item Adoption Uses Explicit Mutation Tools](./036-work-item-adoption-uses-explicit-mutation-tools.md)
- [037 Control-Plane Work-Item Enqueue](./037-control-plane-work-item-enqueue.md)
- [038 Turn-End Work-Item Commit Uses A Bound Active Snapshot](./038-turn-end-work-item-commit-uses-a-bound-active-snapshot.md)
- [039 Idle Activation Comes From The Persisted Work Queue](./039-idle-activation-comes-from-the-persisted-work-queue.md)
- [040 /status Remains Agent-Facing While /state Stays Bootstrap-Oriented](./040-status-remains-agent-facing-while-state-stays-bootstrap-oriented.md)
- [041 Weak Verification Text Is Kept As Raw Evidence](./041-weak-verification-text-is-kept-as-raw-evidence.md)
- [042 Child Agent Task Workspace Mode](./042-child-agent-task-workspace-mode.md)
- [043 Task-Owned Worktree Cleanup](./043-task-owned-worktree-cleanup.md)
- [044 Work Item Reactivation Uses Continuable Closure](./044-work-item-reactivation-uses-continuable-closure.md)
- [045 Anthropic Context Management](./045-anthropic-context-management.md)
- [046 OpenAI Incremental Continuation](./046-openai-incremental-continuation.md)
- [047 Anthropic Rolling Cache Marker](./047-anthropic-rolling-cache-marker.md)
- [048 Compatible Provider Catalog](./048-compatible-provider-catalog.md)
- [049 Anthropic Cache Break Classification](./049-anthropic-cache-break-classification.md)
- [050 Anthropic Claude CLI-Like Cache Lowering](./050-anthropic-claude-cli-like-cache-lowering.md)
- [051 Runtime Config Provider Credentials](./051-runtime-config-provider-credentials.md)
- [052 Turn-Local Compaction Remains Request Projection](./052-turn-local-compaction-remains-request-projection.md)
