declare const opaqueToken: unique symbol;

type OpaqueToken<Name extends string> = string & {
  readonly [opaqueToken]: Name;
};

export type ConversationHistoryCursor = OpaqueToken<"conversation-history-cursor">;
export type ConversationDetailCursor = OpaqueToken<"conversation-detail-cursor">;
export type ConversationCheckpoint = OpaqueToken<"conversation-checkpoint">;

export const CONVERSATION_CAPABILITY = "agents.conversation-read.v1";
export const CONVERSATION_SCHEMA_VERSION = 1;
export const CONVERSATION_QUERY_VERSION = 1;
export const HOLON_CONTROL_PROTOCOL_NAME = "holon-control";
export const HOLON_CONTROL_PROTOCOL_VERSION = 1;

export interface TurnKey {
  readonly turn_index: number;
  readonly turn_id: string;
}

export interface ActivityKey {
  readonly event_seq: number;
  readonly activity_id: string;
}

export type PendingInputState = "queued" | "assigning";

export interface PendingInput {
  readonly message_id: string;
  readonly revision: number;
  readonly state: PendingInputState;
  readonly preview: string;
}

export type TerminalOutcome =
  | "completed"
  | "aborted"
  | "baseline_over_budget"
  | "deferred_to_fallback"
  | "provider_failed_needs_recovery";

export type ExecutionState =
  | { readonly kind: "active" }
  | {
      readonly kind: "terminal";
      readonly outcome: TerminalOutcome;
    };

export type NoBriefReason =
  | {
      readonly kind: "reducer_only";
      readonly reason: string;
    }
  | { readonly kind: "aborted" }
  | { readonly kind: "tool_only_wait" };

export type ResultUnavailableReason =
  | "missing_canonical_linkage"
  | "retention_gap"
  | "legacy_coverage";

export type ResultState =
  | { readonly kind: "pending" }
  | { readonly kind: "available" }
  | {
      readonly kind: "none";
      readonly reason: NoBriefReason;
    }
  | {
      readonly kind: "unavailable";
      readonly reason: ResultUnavailableReason;
      readonly retryable: boolean;
    };

export type Attention =
  | {
      readonly kind: "failed";
      readonly outcome: TerminalOutcome;
    }
  | { readonly kind: "interrupted" }
  | { readonly kind: "waiting" };

export type DetailCoverageReason =
  | "retention_gap"
  | "legacy_ownership"
  | "unknown_activity_type"
  | "missing_canonical_linkage";

export type DetailCoverage =
  | { readonly kind: "complete" }
  | {
      readonly kind: "partial";
      readonly reason: DetailCoverageReason;
    }
  | {
      readonly kind: "unavailable";
      readonly reason: DetailCoverageReason;
    }
  | { readonly kind: "unknown" };

export type PresentationClass =
  | "operator"
  | "task"
  | "external"
  | "timer"
  | "internal"
  | "system"
  | "operational";

export interface TurnInputSummary {
  readonly message_id: string;
  readonly preview: string;
}

export interface ConversationTurnSummary {
  readonly turn_id: string;
  readonly key: TurnKey;
  readonly revision: number;
  readonly presentation_class: PresentationClass;
  readonly inputs: readonly TurnInputSummary[];
  readonly execution: ExecutionState;
  readonly result: ResultState;
  readonly settled: boolean;
  readonly attention: Attention | null;
  readonly detail_coverage: DetailCoverage;
  readonly brief_ids: readonly string[];
}

export interface ActivityItem {
  readonly id: string;
  readonly key: ActivityKey;
  readonly revision: number;
  readonly summary: string;
}

export type ConversationActivity =
  | {
      readonly kind: "operator";
      readonly id: string;
      readonly key: ActivityKey;
      readonly revision: number;
      readonly summary: string;
    }
  | {
      readonly kind: "assistant";
      readonly id: string;
      readonly key: ActivityKey;
      readonly revision: number;
      readonly summary: string;
    }
  | {
      readonly kind: "tool";
      readonly id: string;
      readonly key: ActivityKey;
      readonly revision: number;
      readonly summary: string;
    }
  | {
      readonly kind: "wait";
      readonly id: string;
      readonly key: ActivityKey;
      readonly revision: number;
      readonly summary: string;
    }
  | {
      readonly kind: "error";
      readonly id: string;
      readonly key: ActivityKey;
      readonly revision: number;
      readonly summary: string;
    };

export interface ConversationBoundary {
  readonly schema_version: number;
  readonly query_version: number;
  readonly runtime_id: string;
  readonly event_log_epoch: string;
  readonly visibility_scope_id: string;
  readonly snapshot_through_seq: number;
  readonly event_head_seq: number;
  readonly oldest_retained_seq: number;
  readonly snapshot_cursor: ConversationCheckpoint;
}

export interface ConversationSummaryResponse extends ConversationBoundary {
  readonly turns: readonly ConversationTurnSummary[];
  readonly active_turns: readonly ConversationTurnSummary[];
  readonly pending_inputs: readonly PendingInput[];
  readonly next_before_cursor: ConversationHistoryCursor | null;
  readonly has_more: boolean;
}

export interface ConversationActivityResponse extends ConversationBoundary {
  readonly turn: ConversationTurnSummary;
  readonly detail_revision: number;
  readonly activities: readonly ConversationActivity[];
  readonly coverage: DetailCoverage;
  readonly next_before_cursor: ConversationDetailCursor | null;
  readonly has_more: boolean;
}

export type ConversationResetReason =
  | "retention_expired"
  | "cursor_ahead"
  | "replay_limit_exceeded"
  | "schema_version_mismatch"
  | "query_version_mismatch"
  | "event_log_epoch_mismatch"
  | "cursor_rejected"
  | "agent_not_found"
  | "stream_recovery_failed"
  | "slow_consumer";

export interface BatchBeginMessage {
  readonly type: "batch_begin";
  readonly batch_id: string;
  readonly schema_version: number;
  readonly query_version: number;
  readonly runtime_id: string;
  readonly event_log_epoch: string;
  readonly visibility_scope_id: string;
  readonly from_seq: number;
  readonly through_seq: number;
}

export interface OperatorUpsertMessage {
  readonly type: "operator_upsert";
  readonly event_log_epoch: string;
  readonly visibility_scope_id: string;
  readonly input: PendingInput;
}

export interface OperatorRemoveMessage {
  readonly type: "operator_remove";
  readonly event_log_epoch: string;
  readonly visibility_scope_id: string;
  readonly message_id: string;
  readonly revision: number;
}

export interface TurnSummaryUpsertMessage {
  readonly type: "turn_summary_upsert";
  readonly event_log_epoch: string;
  readonly visibility_scope_id: string;
  readonly turn: ConversationTurnSummary;
}

export interface ActivityUpsertMessage {
  readonly type: "activity_upsert";
  readonly event_log_epoch: string;
  readonly visibility_scope_id: string;
  readonly turn_id: string;
  readonly activity: ConversationActivity;
}

export interface DetailInvalidatedMessage {
  readonly type: "detail_invalidated";
  readonly event_log_epoch: string;
  readonly visibility_scope_id: string;
  readonly turn_id: string;
  readonly detail_revision: number;
}

export interface CheckpointMessage {
  readonly type: "checkpoint";
  readonly batch_id: string;
  readonly event_log_epoch: string;
  readonly visibility_scope_id: string;
  readonly through_seq: number;
  readonly checkpoint: ConversationCheckpoint;
}

export interface ResetRequiredMessage {
  readonly type: "reset_required";
  readonly reason: ConversationResetReason;
  readonly oldest_retained_seq: number | null;
  readonly event_head_seq: number | null;
  readonly hint: string;
}

export type ConversationMutationMessage =
  | OperatorUpsertMessage
  | OperatorRemoveMessage
  | TurnSummaryUpsertMessage
  | ActivityUpsertMessage
  | DetailInvalidatedMessage;

export type ConversationStreamMessage =
  | BatchBeginMessage
  | ConversationMutationMessage
  | CheckpointMessage
  | ResetRequiredMessage;

export interface ConversationStreamBatch {
  readonly begin: BatchBeginMessage;
  readonly mutations: readonly ConversationMutationMessage[];
  readonly checkpoint: CheckpointMessage;
}

export type ConversationStreamItem =
  | {
      readonly type: "batch";
      readonly batch: ConversationStreamBatch;
    }
  | {
      readonly type: "reset_required";
      readonly reset: ResetRequiredMessage;
    };

export type BriefKind = "ack" | "result" | "failure";

export type BriefContentSource =
  | { readonly kind: "inline" }
  | {
      readonly kind: "transcript_entry";
      readonly entry_id: string;
      readonly relation: "derived_from" | "finalizes" | "excerpt";
    };

export interface BriefRecord {
  readonly id: string;
  readonly agent_id: string;
  readonly workspace_id: string;
  readonly work_item_id?: string;
  readonly turn_index?: number;
  readonly turn_id?: string;
  readonly kind: BriefKind;
  readonly created_at: string;
  readonly content_source: BriefContentSource;
  readonly finalizes_assistant_round_id?: string;
  readonly text: string;
  readonly citations?: readonly {
    readonly url: string;
    readonly title?: string;
  }[];
  readonly created_event_seq?: number;
  readonly attachments: readonly unknown[] | null;
  readonly related_message_id: string | null;
  readonly related_task_id: string | null;
}

export interface ConversationHandshake {
  readonly ok: true;
  readonly protocol: {
    readonly name: string;
    readonly version: number;
  };
  readonly auth: {
    readonly mode: string;
    readonly required: boolean;
  };
  readonly capabilities: readonly string[];
  readonly runtime: Readonly<Record<string, unknown>>;
}

export interface ConversationRequestIdentity {
  readonly remote_id: string;
  readonly agent_id: string;
  readonly generation: number;
}

export interface ConversationScope extends ConversationRequestIdentity {
  readonly runtime_id: string;
  readonly event_log_epoch: string;
  readonly visibility_scope_id: string;
  readonly schema_version: number;
  readonly query_version: number;
}

export interface ConversationStateLimits {
  readonly max_turns: number;
  readonly max_live_turns: number;
  readonly max_pending_inputs: number;
  readonly max_detail_turns: number;
  readonly max_activities_per_turn: number;
}
