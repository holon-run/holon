import type {
  BriefRecord,
  ConversationBriefLoadState,
  ConversationDetailView,
  ConversationDetailLoadState,
  ConversationHistoryLoadState,
  ConversationStateView,
  ConversationStatus,
  ConversationTurnSummary,
  ExecutionState,
  NoBriefReason,
  PendingInput,
  PresentationClass,
  ResultState,
  ResultUnavailableReason,
  TerminalOutcome,
  Attention,
  TurnInputSummary,
} from "@holon/conversation-sdk";

/**
 * UI-facing projection of one canonical conversation turn. Protocol decoding
 * stays in the SDK; this shape is what rendering components consume.
 */
export interface ConversationTurnGroup {
  readonly turnId: string;
  readonly turnIndex: number;
  readonly revision: number;
  readonly presentationClass: PresentationClass;
  readonly inputs: readonly TurnInputSummary[];
  readonly execution: ExecutionState;
  readonly result: ResultState;
  readonly settled: boolean;
  readonly attention: Attention | null;
  readonly detailCoverage: ConversationTurnSummary["detail_coverage"];
  readonly briefIds: readonly string[];
  readonly detail: ConversationDetailView | null;
}

export interface ConversationSessionModel {
  readonly status: ConversationStatus;
  readonly view: ConversationStateView | null;
  readonly turns: readonly ConversationTurnGroup[];
  readonly pendingInputs: readonly PendingInput[];
  readonly hasMoreHistory: boolean;
  readonly historyState: ConversationHistoryLoadState;
  readonly briefs: ReadonlyMap<string, BriefRecord>;
  readonly briefLoadStates: ReadonlyMap<string, ConversationBriefLoadState>;
  readonly detailLoadStates: ReadonlyMap<string, ConversationDetailLoadState>;
  /** Turn currently executing, if any. */
  readonly activeTurn: ConversationTurnGroup | null;
  /** True while the turn list itself is not yet materialized. */
  readonly bootstrapLoading: boolean;
}

export interface BuildConversationSessionModelInput {
  readonly status: ConversationStatus;
  readonly view: ConversationStateView | null;
  readonly historyState: ConversationHistoryLoadState;
  readonly briefs: ReadonlyMap<string, BriefRecord>;
  readonly briefLoadStates: ReadonlyMap<string, ConversationBriefLoadState>;
  readonly detailLoadStates: ReadonlyMap<string, ConversationDetailLoadState>;
}

export function buildConversationSessionModel(
  input: BuildConversationSessionModelInput,
): ConversationSessionModel {
  const view = input.view;
  const turns: ConversationTurnGroup[] =
    view?.turns.map((turn) => ({
      turnId: turn.turn_id,
      turnIndex: turn.key.turn_index,
      revision: turn.revision,
      presentationClass: turn.presentation_class,
      inputs: turn.inputs,
      execution: turn.execution,
      result: turn.result,
      settled: turn.settled,
      attention: turn.attention,
      detailCoverage: turn.detail_coverage,
      briefIds: turn.brief_ids,
      detail: view.details.find((detail) => detail.turn_id === turn.turn_id) ?? null,
    })) ?? [];
  turns.sort((left, right) =>
    left.turnIndex - right.turnIndex ||
    left.turnId.localeCompare(right.turnId),
  );
  const activeTurn =
    [...turns].reverse().find((turn) => turn.execution.kind === "active") ?? null;
  return {
    status: input.status,
    view,
    turns,
    pendingInputs: (view?.pending_inputs ?? []).filter(
      (input) => !turns.some((turn) => turn.inputs.some((assigned) => assigned.message_id === input.message_id)),
    ),
    hasMoreHistory: view?.has_more ?? false,
    historyState: input.historyState,
    briefs: input.briefs,
    briefLoadStates: input.briefLoadStates,
    detailLoadStates: input.detailLoadStates,
    activeTurn,
    bootstrapLoading:
      (input.status.kind === "loading" || input.status.kind === "idle") &&
      view === null,
  };
}

/**
 * Result presentation classifier shared by turn rendering: separates "a
 * deliverable result exists" from terminal-without-result states that need
 * distinct affordances.
 */
export type TurnResultPresentation =
  | { readonly kind: "pending" }
  | { readonly kind: "available"; readonly briefIds: readonly string[] }
  | {
      readonly kind: "terminal_without_result";
      readonly outcome: TerminalOutcome;
      readonly reason?: NoBriefReason;
    }
  | {
      readonly kind: "unavailable";
      readonly reason: ResultUnavailableReason;
      readonly retryable: boolean;
    };

export function turnResultPresentation(
  turn: ConversationTurnGroup,
): TurnResultPresentation {
  if (turn.briefIds.length > 0) {
    return { kind: "available", briefIds: turn.briefIds };
  }
  if (turn.result.kind === "unavailable") {
    return {
      kind: "unavailable",
      reason: turn.result.reason,
      retryable: turn.result.retryable,
    };
  }
  if (turn.result.kind === "pending") {
    // Terminal execution with a still-pending result record: the brief may
    // arrive shortly after terminal; render an explicit waiting state.
    return { kind: "pending" };
  }
  if (turn.execution.kind === "active") return { kind: "pending" };
  if (turn.result.kind === "none") {
    return {
      kind: "terminal_without_result",
      outcome: turn.execution.outcome,
      reason: turn.result.reason,
    };
  }
  return { kind: "terminal_without_result", outcome: turn.execution.outcome };
}

/** Execution state is independent of Brief arrival and loading. */
export function turnExecutionPresentation(turn: ConversationTurnGroup) {
  if (turn.execution.kind === "active") return "running";
  if (turn.execution.outcome === "aborted" || turn.attention?.kind === "interrupted") return "interrupted";
  if (turn.execution.outcome === "deferred_to_fallback") return "recovering";
  if (turn.attention?.kind === "failed" || turn.execution.outcome !== "completed") return "failed";
  if (turn.attention?.kind === "waiting") return "waiting";
  if (turn.result.kind !== "unavailable" && (turn.result.kind === "pending" || !turn.settled)) return "waitingResult";
  return "completed";
}
