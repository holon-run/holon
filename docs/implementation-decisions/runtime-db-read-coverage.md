# RuntimeDb read coverage after the list-only pool rollout

Date: 2026-10-21. Baseline: `28d792828563ed5a7c649fa0aba96ce0db7d195c`
(includes #3662). P0/P1 implemented; local validation is recorded below.
This is not a production rollout claim.

## Choice and preserved boundary

Use the existing per-owner reader pool, rather than changing the mixed-role
`connection()` API. Pool capacity, acquisition budget, protection checks,
writer ordering, SQL selection, source/generation fences, and polling intervals
stay unchanged. SELECT statements return owned results before decoding outside
helpers, unrelated filesystem work, async waits, or a subsequent write.
Existing explicit read transactions keep their original snapshot boundaries.
No helper may borrow a reader and then call another repository that checks out.

The table inventories new direct reader-closure entrypoints by method, not all
transitive callers. Every row is read-only except the explicitly marked
preflight: `delete_acknowledged_through` releases its SELECT lease before its
existing writer transaction. Shared template row loading covers
`agent_template_remote_source_needs_sync` and
`record_agent_template_remote_source_sync_failure`; snapshot loading batches
sources under one lease. Only a successful missing-row result permits an empty
catalog; read/decoding failures must not overwrite the old catalog.

## Method inventory

| Module | Owner | Reader methods |
| --- | --- | --- |
| `agent_template.rs` | `module` | `load_remote_template_catalog_snapshot`, `load_remote_source_row` |
| `memory/index.rs` | `module` | `runtime_payload_page` |
| `model_config_migration.rs` | `module` | `inspect_agent_state` |
| `runtime/scheduler_acceptance.rs` | `module` | `execution_claim_row_counts` |
| `runtime_db/agent_message_delivery.rs` | `AgentMessageDeliveryRepository<'_>` | `rowid`, `first_accepted_from_sender_after`, `latest`, `latest_for_message` |
| `runtime_db/agent_relations.rs` | `AgentCanonicalRelationRepository<'_>` | `latest`, `lineage_children` |
| `runtime_db/authentication.rs` | `AuthenticationRepository<'_>` | `find_user`, `find_user_by_id`, `find_session` |
| `runtime_db/conversation.rs` | `ConversationRepository<'_>` | `brief_snapshot`, `summary_page`, `shadow_diagnostics`, `summary_snapshot_after_context`, `activities`, `activity_snapshot`, `change_batch` |
| `runtime_db/conversation/locate.rs` | `ConversationRepository<'_>` | `summary_window_snapshot`, `locate_source` |
| `runtime_db/destructive_operations.rs` | `DestructiveOperationRepository<'_>` | `by_id`, `pending_verification` |
| `runtime_db/index_outbox.rs` | `RuntimeIndexOutboxRepository<'_>` | `high_watermark`, `agent_ids_with_pending`, `agent_ids_with_pending_async`, `agent_ids_with_watermarks`, `agent_ids_with_watermarks_async`, `high_watermark_for_agent`, `produced_watermark_for_agent`, `pending_count_for_agent`, `oldest_pending_created_at_for_agent`, `read_after`, `delete_acknowledged_through` |
| `runtime_db/mod.rs` | `RuntimeDb` | `provider_quota_incident`, `storage_domain`, `storage_domains`, `storage_domain_is_complete`, `event_log_epoch`, `runtime_id`, `visibility_policy_generation` |
| `runtime_db/observer_sync.rs` | `crate::runtime_db::RuntimeDb` | `observer_sync_foundations`, `agent_roster_snapshot_rows`, `agent_projection_snapshot_rows`, `agent_event_recovery_window` |
| `runtime_db/reclamation_reminders.rs` | `RuntimeDb` | `pending_subagent_reminders`, `subagent_cleanup_candidates` |
| `runtime_db/repositories.rs` | `WorkItemRepository<'_>` | `latest`, `latest_for_agent`, `open_for_agent`, `latest_non_open_for_agent`, `latest_many`, `latest_all`, `due_blocked_rechecks`, `next_recheck_at` |
| `runtime_db/repositories.rs` | `AgentStateRepository<'_>` | `latest`, `latest_all` |
| `runtime_db/repositories.rs` | `WorkspaceEntryRepository<'_>` | `latest_all` |
| `runtime_db/repositories.rs` | `WorkspaceOccupancyRepository<'_>` | `latest_all` |
| `runtime_db/repositories.rs` | `ExecutionRootEntryRepository<'_>` | `get`, `active_for_workspace`, `latest_all` |
| `runtime_db/repositories.rs` | `AgentIdentityRepository<'_>` | `latest_all`, `latest` |
| `runtime_db/repositories.rs` | `AgentBootstrapRepository<'_>` | `latest` |
| `runtime_db/repositories.rs` | `AgentDeletionRepository<'_>` | `latest_for_agent`, `due_jobs`, `earliest_retry_at` |
| `runtime_db/repositories.rs` | `WorkItemDelegationRepository<'_>` | `latest_all`, `recent`, `recent_for_agent`, `latest_for_child` |
| `runtime_db/repositories.rs` | `WorkItemContinuationRepository<'_>` | `recent`, `recent_for_agent`, `active_for_agent`, `latest_all` |
| `runtime_db/repositories.rs` | `ContextEpisodeRepository<'_>` | `recent`, `recent_for_agent` |
| `runtime_db/repositories.rs` | `ExternalTriggerRepository<'_>` | `latest`, `latest_for_agent`, `latest_for_agent_limit`, `active_default_for_agent`, `active_by_token_hash`, `latest_all` |
| `runtime_db/repositories.rs` | `OperatorNotificationRepository<'_>` | `read_recent_for_agent` |
| `runtime_db/repositories.rs` | `OperatorTransportBindingRepository<'_>` | `latest_for_agent`, `read_recent_for_agent`, `latest_all` |
| `runtime_db/repositories.rs` | `OperatorDeliveryRepository<'_>` | `latest_for_agent`, `read_recent_for_agent`, `latest_all` |
| `runtime_db/repositories.rs` | `TaskRepository<'_>` | `latest`, `latest_all`, `message_waits_for_reply`, `activity_watermark_for_agent`, `active_owner_agent_ids`, `query_for_agent` |
| `runtime_db/repositories.rs` | `WaitConditionRepository<'_>` | `latest_all`, `latest_for_agent`, `snapshot_for_agent`, `recent`, `recent_for_agent`, `for_turn`, `active_for_agent`, `unresolved_for_agent`, `active_all`, `recheck_owner_agent_ids`, `unresolved_all` |
| `runtime_db/repositories.rs` | `QueueEntryRepository<'_>` | `latest`, `latest_all`, `recent`, `has_queued_for_agent`, `recovery_candidate_agent_ids`, `has_interrupted_for_agent`, `queued_for_agent` |
| `runtime_db/repositories.rs` | `TimerRepository<'_>` | `pending_wake`, `active_owner_agent_ids`, `latest`, `latest_all`, `recent`, `recent_for_agent` |
| `runtime_db/repositories.rs` | `TurnRecordRepository<'_>` | `by_id`, `recent_for_agent`, `recent_for_owner`, `recent` |
| `runtime_db/repositories.rs` | `MessageRepository<'_>` | `recent`, `from`, `supervised_invocation_sources`, `all`, `by_id`, `latest_trusted_operator`, `for_turn`, `count`, `max_message_seq` |
| `runtime_db/repositories.rs` | `TranscriptRepository<'_>` | `recent`, `all`, `for_turn_ids`, `by_id`, `max_transcript_seq` |
| `runtime_db/repositories.rs` | `EvidenceRepository<'_>` | `query`, `recent_payloads`, `payloads_for_turn`, `payload_by_id`, `payloads_by_ids`, `latest_delivery_summary`, `count_briefs` |
| `runtime_db/repositories.rs` | `AuditEventSink<'_>` | `has_event_by_id`, `recent`, `latest_event_seq`, `range`, `page_after` |
| `runtime_db/task_result_settlement.rs` | `TaskResultSettlementRepository<'_>` | `admitted_for_activation`, `due_deferred`, `unsettled_for_agent`, `unsettled_for_owner`, `next_recheck_at`, `latest_for_message` |
| `runtime_db/task_result_settlement/recovery_budget.rs` | `TaskResultSettlementRepository<'_>` | `unreported_paused` |
| `runtime_db/transitions/execution_protocol_repository.rs` | `RuntimeTransitionRepository<'_>` | `execution_attempt_by_id`, `load_execution_authority_fences`, `load_execution_protocol_state_if_initialized` |
| `runtime_db/unread.rs` | `crate::runtime_db::RuntimeDb` | `brief_read_states_with_scope` |

## Explicit raw exceptions

| Location | Retained purpose |
| --- | --- |
| `runtime_db/mod.rs` | Open/migration gates, verified backup (`VACUUM INTO`), rollback planning, schema/cutover and retired-scheduler maintenance diagnostics. |
| `runtime_db/index_outbox.rs::delete_all_for_agent` | Existing raw DELETE; must not use a query-only reader. Writer refactoring is outside this change. |
| `runtime_db/observer_sync.rs` self-heal verification | Verification includes write/rollback probes; not a pure SELECT. |
| `runtime_db/agent_relations.rs` backfill | Explicit maintenance workflow; retain its independent discovery connection. |
| `runtime_db/retention.rs` | Retention planning and mixed retention/compaction/checkpoint maintenance; includes VACUUM, connection settings and WAL checkpoints. |
| `runtime_db/audit.rs` | Explicit offline/maintenance integrity report, not runtime polling. |
| `runtime_db/{brief_publication_repair,turn_settlement_repair}.rs` | Explicit repair-plan/backup workflows, independent source and plan connections. |
| Test fixtures | Intentional raw SQL mutations/adversarial probes, not runtime readers. |

`model_config_migration::inspect_agent_state` and the scheduler-acceptance COUNT
fixture also use short reader closures, but neither is presented as a hot-path
performance improvement. Independent RuntimeDb owners and raw maintenance
connections remain outside another owner's pool capacity.

## Async and transient failure boundary

Blocking evidence collection for invocation monitoring runs off Tokio worker
threads and returns owned state before async result projection. Delivery and
message monitors retain their responsibility on `RuntimeDbRetryableError` and
retry at the existing 100 ms interval; saturation is not a business failure or
permission to clean up a child/worktree. Legacy recovery retains its existing
pending/recovery-blocked cadence. Foreground task/wait reads propagate storage
errors before interpreting missing records or consumed results. Daemon outbox
discovery uses the pool's asynchronous admission API.

This does not optimize the Linux sidecar checks: physical open still performs
both checks, and warm readers retain all execution/acceptance protection gates.
The synchronous coverage benchmark is not evidence for async responsiveness,
large canonical invocation graphs, or production traffic. Those boundaries
require their own correctness/resource evidence before final acceptance.

## Local validation and latency-budget decision

The fixed-work synthetic A/B/A run used the baseline above and the P1
candidate: five workloads, two agent counts, three worker counts, two write
counts, and five repetitions (300 read groups per phase). Each group keeps its
256 raw read samples; comparisons are per repetition against both A phases,
not pooled or selected by the better baseline.

After inspecting the existing results, the operator approved a revised read
p95 budget: `B - A <= max(5 ms, 0.10 * A)`. This is an engineering tolerance,
not a measured noise bound or production SLO. The 300 existing read groups
pass this revised budget. This is a post-hoc reassessment, not a newly
preregistered experiment, proof of no regression, or overall acceptance.
The original read rule (at most +1 ms below a 1 ms baseline, otherwise +10%)
failed 13 `agent_list_control` groups; those results remain valid under that
rule, and their cause is unresolved. The newly covered reads passed all
240 groups under the original rule. Writer p95 retains the original rule;
all 150 nonempty writer groups pass against both A phases.

The synthetic newly covered workloads showed approximately 99.4–99.7% lower
process CPU and zero additional physical opens after warm-up. These results
do not predict production improvement. Finite FD diagnostics passed after
moving raw-connection warm-up before the FD baseline; later WAL descriptor
tracking observed closure, but does not establish the cause of every earlier
FD increment or prove long-term leak freedom. Pool wait/lease duration,
physical close counts, and writer queue timing were not exported.

Local correctness evidence includes reader-pool and async saturation,
cancellation/recovery/monitor regressions, 19 startup recovery tests, and
71 binary tests. The unfiltered library suite is **4465 passed, 2 failed,
6 ignored**, not green: `http::tailscale_serve::tests::` tests
`ipv6_wildcard_listener_accepts_selected_backend_target` and
`localhost_legacy_rule_can_migrate_to_resolved_listener` fail with IPv6
`AddrNotAvailable` in this environment, also reproduced on the fixed baseline.
Formatting and warning-free all-target checks passed. Cross-platform CI is
still required; no deployment, production load test, sidecar optimization,
or automatic merge is included.
