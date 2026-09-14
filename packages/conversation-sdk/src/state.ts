import {
  ConversationProtocolError,
  ConversationStaleResponseError,
  ConversationStateLimitError,
} from "./errors.js";
import type {
  ConversationActivity,
  ConversationActivityResponse,
  ConversationCheckpoint,
  ConversationDetailCursor,
  ConversationHistoryCursor,
  ConversationMutationMessage,
  ConversationRequestIdentity,
  ConversationResetReason,
  ConversationScope,
  ConversationStateLimits,
  ConversationStreamBatch,
  ConversationSummaryResponse,
  ConversationTurnSummary,
  DetailCoverage,
  PendingInput,
} from "./types.js";

export const DEFAULT_CONVERSATION_STATE_LIMITS: ConversationStateLimits = {
  max_turns: 300,
  max_live_turns: 100,
  max_pending_inputs: 200,
  max_detail_turns: 32,
  max_activities_per_turn: 400,
};

export interface ConversationDetailView {
  readonly turn_id: string;
  readonly detail_revision: number;
  readonly activities: readonly ConversationActivity[];
  readonly coverage: DetailCoverage;
  readonly next_before_cursor: ConversationDetailCursor | null;
  readonly has_more: boolean;
  readonly invalidated: boolean;
  readonly truncated: boolean;
}

export interface ConversationStateView {
  readonly scope: ConversationScope | null;
  readonly turns: readonly ConversationTurnSummary[];
  readonly pending_inputs: readonly PendingInput[];
  readonly details: readonly ConversationDetailView[];
  readonly next_before_cursor: ConversationHistoryCursor | null;
  readonly has_more: boolean;
  readonly checkpoint: ConversationCheckpoint | null;
  readonly through_seq: number | null;
  readonly reset_reason: ConversationResetReason | null;
}

interface MutableDetail {
  turnId: string;
  detailRevision: number;
  activities: Map<string, ConversationActivity>;
  coverage: DetailCoverage;
  nextBeforeCursor: ConversationDetailCursor | null;
  hasMore: boolean;
  invalidated: boolean;
  truncated: boolean;
}

interface AppliedBatchBoundary {
  batchId: string;
  fromSeq: number;
  throughSeq: number;
  checkpoint: ConversationCheckpoint;
}

interface MutableConversationState {
  scope: ConversationScope | null;
  turns: Map<string, ConversationTurnSummary>;
  loadedTurnIds: Set<string>;
  liveTurnOrder: string[];
  pendingInputs: Map<string, PendingInput>;
  pendingInputTombstones: Map<string, number>;
  details: Map<string, MutableDetail>;
  detailOrder: string[];
  nextBeforeCursor: ConversationHistoryCursor | null;
  hasMore: boolean;
  checkpoint: ConversationCheckpoint | null;
  throughSeq: number | null;
  lastAppliedBatch: AppliedBatchBoundary | null;
  resetReason: ConversationResetReason | null;
}

export class ConversationProtocolState {
  readonly limits: ConversationStateLimits;
  #state: MutableConversationState;

  constructor(
    limits: Partial<ConversationStateLimits> = {},
  ) {
    this.limits = validateLimits({
      ...DEFAULT_CONVERSATION_STATE_LIMITS,
      ...limits,
    });
    this.#state = emptyState();
  }

  bootstrap(
    identity: ConversationRequestIdentity,
    snapshot: ConversationSummaryResponse,
  ): void {
    const next = emptyState();
    next.scope = scopeFromSnapshot(identity, snapshot);
    next.nextBeforeCursor = snapshot.next_before_cursor;
    next.hasMore = snapshot.has_more;
    next.checkpoint = snapshot.snapshot_cursor;
    next.throughSeq = snapshot.snapshot_through_seq;

    for (const turn of snapshot.turns) {
      next.loadedTurnIds.add(turn.turn_id);
      mergeTurn(next, turn, true, this.limits);
    }
    for (const turn of snapshot.active_turns) {
      mergeTurn(next, turn, false, this.limits);
      if (!next.loadedTurnIds.has(turn.turn_id)) {
        addLiveTurn(next, turn.turn_id, this.limits);
      }
    }
    for (const input of snapshot.pending_inputs) {
      upsertPendingInput(next, input, this.limits);
    }
    ensureTurnLimit(next, this.limits);
    this.#state = next;
  }

  applyOlderPage(
    identity: ConversationRequestIdentity,
    before: ConversationHistoryCursor,
    page: ConversationSummaryResponse,
  ): boolean {
    const current = this.#requireScope(identity);
    if (this.#state.nextBeforeCursor !== before) {
      throw new ConversationStaleResponseError(
        "older-page cursor is no longer current",
      );
    }
    assertSnapshotScope(current, page);
    assertSnapshotFreshness(this.#state.throughSeq, page);
    const next = cloneState(this.#state);
    for (const turn of page.turns) {
      next.loadedTurnIds.add(turn.turn_id);
      mergeTurn(next, turn, true, this.limits);
    }
    for (const turn of page.active_turns) {
      mergeTurn(next, turn, false, this.limits);
      if (!next.loadedTurnIds.has(turn.turn_id)) {
        addLiveTurn(next, turn.turn_id, this.limits);
      }
    }
    next.nextBeforeCursor = page.next_before_cursor;
    next.hasMore = page.has_more;
    ensureTurnLimit(next, this.limits);
    this.#state = next;
    return true;
  }

  applyDetailPage(
    identity: ConversationRequestIdentity,
    turnId: string,
    before: ConversationDetailCursor | undefined,
    page: ConversationActivityResponse,
  ): boolean {
    const current = this.#requireScope(identity);
    assertSnapshotScope(current, page);
    assertSnapshotFreshness(this.#state.throughSeq, page);
    if (page.turn.turn_id !== turnId) {
      throw new ConversationStaleResponseError(
        "detail response belongs to a different turn",
      );
    }
    const existing = this.#state.details.get(turnId);
    if (before !== undefined) {
      if (existing === undefined || existing.nextBeforeCursor !== before) {
        throw new ConversationStaleResponseError(
          "detail-page cursor is no longer current",
        );
      }
      if (page.detail_revision !== existing.detailRevision) {
        return false;
      }
    } else if (
      existing !== undefined &&
      page.detail_revision < existing.detailRevision
    ) {
      return false;
    }

    const next = cloneState(this.#state);
    mergeTurn(next, page.turn, next.loadedTurnIds.has(turnId), this.limits);
    let detail = next.details.get(turnId);
    if (detail === undefined) {
      detail = newDetail(turnId, page.detail_revision);
      insertDetail(next, detail, this.limits);
    }
    if (before === undefined) {
      detail.activities.clear();
      detail.truncated = false;
    }
    detail.detailRevision = page.detail_revision;
    detail.coverage = page.coverage;
    detail.nextBeforeCursor = page.next_before_cursor;
    detail.hasMore = page.has_more;
    detail.invalidated = false;
    for (const activity of page.activities) {
      mergeActivity(detail, activity);
    }
    enforceActivityLimit(detail, this.limits);
    touchDetail(next, turnId);
    this.#state = next;
    return true;
  }

  applyBatch(
    identity: ConversationRequestIdentity,
    batch: ConversationStreamBatch,
  ): boolean {
    const scope = this.#requireScope(identity);
    assertBatchScope(scope, batch);
    assertBatchFraming(batch);
    if (batch.begin.through_seq < batch.begin.from_seq) {
      throw new ConversationProtocolError(
        "stream batch through_seq precedes from_seq",
      );
    }
    if (this.#state.checkpoint === batch.checkpoint.checkpoint) {
      if (
        isExactBatchReplay(this.#state.lastAppliedBatch, batch) ||
        isCurrentBoundaryAcknowledgement(this.#state, batch)
      ) {
        return false;
      }
      throw new ConversationProtocolError(
        `stream checkpoint ${batch.checkpoint.checkpoint} was reused for a different batch boundary`,
      );
    }
    if (this.#state.throughSeq !== batch.begin.from_seq) {
      throw new ConversationStaleResponseError(
        `stream batch starts at ${batch.begin.from_seq}, expected ${String(this.#state.throughSeq)}`,
      );
    }

    const next = cloneState(this.#state);
    for (const mutation of batch.mutations) {
      applyMutation(next, mutation, this.limits);
    }
    next.checkpoint = batch.checkpoint.checkpoint;
    next.throughSeq = batch.checkpoint.through_seq;
    next.lastAppliedBatch = batchBoundary(batch);
    ensureTurnLimit(next, this.limits);
    this.#state = next;
    return true;
  }

  reset(reason: ConversationResetReason): void {
    this.#state = emptyState();
    this.#state.resetReason = reason;
  }

  reconnectCheckpoint(): ConversationCheckpoint | null {
    return this.#state.resetReason === null ? this.#state.checkpoint : null;
  }

  view(): ConversationStateView {
    const turns = [...this.#state.turns.values()].sort(compareTurns);
    const pendingInputs = [...this.#state.pendingInputs.values()].sort((a, b) =>
      a.message_id.localeCompare(b.message_id),
    );
    const details = this.#state.detailOrder
      .map((turnId) => this.#state.details.get(turnId))
      .filter((detail): detail is MutableDetail => detail !== undefined)
      .map(detailView);
    return {
      scope: this.#state.scope,
      turns,
      pending_inputs: pendingInputs,
      details,
      next_before_cursor: this.#state.nextBeforeCursor,
      has_more: this.#state.hasMore,
      checkpoint: this.#state.checkpoint,
      through_seq: this.#state.throughSeq,
      reset_reason: this.#state.resetReason,
    };
  }

  #requireScope(identity: ConversationRequestIdentity): ConversationScope {
    const scope = this.#state.scope;
    if (scope === null || this.#state.resetReason !== null) {
      throw new ConversationStaleResponseError(
        "conversation state requires a fresh bootstrap",
      );
    }
    assertIdentity(scope, identity);
    return scope;
  }
}

function emptyState(): MutableConversationState {
  return {
    scope: null,
    turns: new Map(),
    loadedTurnIds: new Set(),
    liveTurnOrder: [],
    pendingInputs: new Map(),
    pendingInputTombstones: new Map(),
    details: new Map(),
    detailOrder: [],
    nextBeforeCursor: null,
    hasMore: false,
    checkpoint: null,
    throughSeq: null,
    lastAppliedBatch: null,
    resetReason: null,
  };
}

function cloneState(
  source: MutableConversationState,
): MutableConversationState {
  return {
    scope: source.scope,
    turns: new Map(source.turns),
    loadedTurnIds: new Set(source.loadedTurnIds),
    liveTurnOrder: [...source.liveTurnOrder],
    pendingInputs: new Map(source.pendingInputs),
    pendingInputTombstones: new Map(source.pendingInputTombstones),
    details: new Map(
      [...source.details].map(([turnId, detail]) => [
        turnId,
        {
          ...detail,
          activities: new Map(detail.activities),
        },
      ]),
    ),
    detailOrder: [...source.detailOrder],
    nextBeforeCursor: source.nextBeforeCursor,
    hasMore: source.hasMore,
    checkpoint: source.checkpoint,
    throughSeq: source.throughSeq,
    lastAppliedBatch: source.lastAppliedBatch,
    resetReason: source.resetReason,
  };
}

function validateLimits(
  limits: ConversationStateLimits,
): ConversationStateLimits {
  for (const [resource, limit] of Object.entries(limits)) {
    if (!Number.isSafeInteger(limit) || limit < 1) {
      throw new ConversationProtocolError(
        `${resource} must be a positive safe integer`,
      );
    }
  }
  return limits;
}

function scopeFromSnapshot(
  identity: ConversationRequestIdentity,
  snapshot: ConversationSummaryResponse,
): ConversationScope {
  return {
    ...identity,
    runtime_id: snapshot.runtime_id,
    event_log_epoch: snapshot.event_log_epoch,
    visibility_scope_id: snapshot.visibility_scope_id,
    schema_version: snapshot.schema_version,
    query_version: snapshot.query_version,
  };
}

function assertIdentity(
  scope: ConversationScope,
  identity: ConversationRequestIdentity,
): void {
  if (
    scope.remote_id !== identity.remote_id ||
    scope.agent_id !== identity.agent_id ||
    scope.generation !== identity.generation
  ) {
    throw new ConversationStaleResponseError(
      "response belongs to a stale remote, agent, or generation",
    );
  }
}

function assertSnapshotScope(
  scope: ConversationScope,
  response: ConversationSummaryResponse | ConversationActivityResponse,
): void {
  if (
    scope.runtime_id !== response.runtime_id ||
    scope.event_log_epoch !== response.event_log_epoch ||
    scope.visibility_scope_id !== response.visibility_scope_id ||
    scope.schema_version !== response.schema_version ||
    scope.query_version !== response.query_version
  ) {
    throw new ConversationStaleResponseError(
      "response scope does not match the bootstrapped conversation",
    );
  }
}

function assertSnapshotFreshness(
  throughSeq: number | null,
  response: ConversationSummaryResponse | ConversationActivityResponse,
): void {
  if (
    throughSeq !== null &&
    response.snapshot_through_seq < throughSeq
  ) {
    throw new ConversationStaleResponseError(
      `snapshot response ends at ${response.snapshot_through_seq}, before committed stream sequence ${throughSeq}`,
    );
  }
}

function assertBatchScope(
  scope: ConversationScope,
  batch: ConversationStreamBatch,
): void {
  if (
    batch.begin.runtime_id !== scope.runtime_id ||
    batch.begin.event_log_epoch !== scope.event_log_epoch ||
    batch.begin.visibility_scope_id !== scope.visibility_scope_id ||
    batch.begin.schema_version !== scope.schema_version ||
    batch.begin.query_version !== scope.query_version ||
    batch.checkpoint.event_log_epoch !== scope.event_log_epoch ||
    batch.checkpoint.visibility_scope_id !== scope.visibility_scope_id
  ) {
    throw new ConversationStaleResponseError(
      "stream batch scope does not match the bootstrapped conversation",
    );
  }
  for (const mutation of batch.mutations) {
    if (
      mutation.event_log_epoch !== scope.event_log_epoch ||
      mutation.visibility_scope_id !== scope.visibility_scope_id
    ) {
      throw new ConversationStaleResponseError(
        "stream mutation scope does not match the bootstrapped conversation",
      );
    }
  }
}

function assertBatchFraming(batch: ConversationStreamBatch): void {
  if (batch.checkpoint.batch_id !== batch.begin.batch_id) {
    throw new ConversationProtocolError(
      "stream checkpoint batch id does not match batch_begin",
    );
  }
  if (batch.checkpoint.through_seq !== batch.begin.through_seq) {
    throw new ConversationProtocolError(
      "stream checkpoint boundary does not match batch_begin",
    );
  }
}

function batchBoundary(
  batch: ConversationStreamBatch,
): AppliedBatchBoundary {
  return {
    batchId: batch.begin.batch_id,
    fromSeq: batch.begin.from_seq,
    throughSeq: batch.begin.through_seq,
    checkpoint: batch.checkpoint.checkpoint,
  };
}

function isExactBatchReplay(
  applied: AppliedBatchBoundary | null,
  batch: ConversationStreamBatch,
): boolean {
  return (
    applied !== null &&
    applied.batchId === batch.begin.batch_id &&
    applied.fromSeq === batch.begin.from_seq &&
    applied.throughSeq === batch.begin.through_seq &&
    applied.checkpoint === batch.checkpoint.checkpoint
  );
}

function isCurrentBoundaryAcknowledgement(
  state: MutableConversationState,
  batch: ConversationStreamBatch,
): boolean {
  return (
    state.throughSeq !== null &&
    batch.mutations.length === 0 &&
    batch.begin.from_seq === state.throughSeq &&
    batch.begin.through_seq === state.throughSeq
  );
}

function compareTurns(
  left: ConversationTurnSummary,
  right: ConversationTurnSummary,
): number {
  return (
    left.key.turn_index - right.key.turn_index ||
    left.key.turn_id.localeCompare(right.key.turn_id)
  );
}

function compareActivities(
  left: ConversationActivity,
  right: ConversationActivity,
): number {
  return (
    left.key.event_seq - right.key.event_seq ||
    left.key.activity_id.localeCompare(right.key.activity_id)
  );
}

function mergeTurn(
  state: MutableConversationState,
  incoming: ConversationTurnSummary,
  loaded: boolean,
  limits: ConversationStateLimits,
): boolean {
  const existing = state.turns.get(incoming.turn_id);
  if (existing !== undefined) {
    if (incoming.revision < existing.revision) {
      return false;
    }
    if (incoming.revision === existing.revision) {
      if (JSON.stringify(incoming) !== JSON.stringify(existing)) {
        throw new ConversationProtocolError(
          `turn ${incoming.turn_id} reused revision ${incoming.revision} with different content`,
        );
      }
      return false;
    }
    if (
      existing.execution.kind === "terminal" &&
      incoming.execution.kind === "active"
    ) {
      return false;
    }
  }
  state.turns.set(incoming.turn_id, incoming);
  if (loaded) {
    state.loadedTurnIds.add(incoming.turn_id);
  }
  if (
    incoming.execution.kind === "active" &&
    !state.loadedTurnIds.has(incoming.turn_id)
  ) {
    addLiveTurn(state, incoming.turn_id, limits);
  }
  return true;
}

function addLiveTurn(
  state: MutableConversationState,
  turnId: string,
  limits: ConversationStateLimits,
): void {
  state.liveTurnOrder = state.liveTurnOrder.filter((id) => id !== turnId);
  state.liveTurnOrder.push(turnId);
  while (state.liveTurnOrder.length > limits.max_live_turns) {
    const evicted = state.liveTurnOrder.shift();
    if (evicted === undefined) {
      break;
    }
    const turn = state.turns.get(evicted);
    if (
      turn !== undefined &&
      turn.execution.kind === "active" &&
      !state.loadedTurnIds.has(evicted)
    ) {
      throw new ConversationStateLimitError(
        "active live turns",
        limits.max_live_turns,
      );
    }
    if (!state.loadedTurnIds.has(evicted)) {
      state.turns.delete(evicted);
      state.details.delete(evicted);
      state.detailOrder = state.detailOrder.filter((id) => id !== evicted);
    }
  }
}

function ensureTurnLimit(
  state: MutableConversationState,
  limits: ConversationStateLimits,
): void {
  if (state.turns.size > limits.max_turns) {
    throw new ConversationStateLimitError("turns", limits.max_turns);
  }
}

function upsertPendingInput(
  state: MutableConversationState,
  input: PendingInput,
  limits: ConversationStateLimits,
): boolean {
  const removedRevision = state.pendingInputTombstones.get(input.message_id);
  if (removedRevision !== undefined && input.revision <= removedRevision) {
    return false;
  }
  const existing = state.pendingInputs.get(input.message_id);
  if (existing !== undefined) {
    if (input.revision < existing.revision) {
      return false;
    }
    if (input.revision === existing.revision) {
      if (JSON.stringify(input) !== JSON.stringify(existing)) {
        throw new ConversationProtocolError(
          `pending input ${input.message_id} reused revision ${input.revision} with different content`,
        );
      }
      return false;
    }
  } else if (state.pendingInputs.size >= limits.max_pending_inputs) {
    throw new ConversationStateLimitError(
      "pending inputs",
      limits.max_pending_inputs,
    );
  }
  state.pendingInputs.set(input.message_id, input);
  state.pendingInputTombstones.delete(input.message_id);
  return true;
}

function removePendingInput(
  state: MutableConversationState,
  messageId: string,
  revision: number,
  limits: ConversationStateLimits,
): boolean {
  const existing = state.pendingInputs.get(messageId);
  const removedRevision = state.pendingInputTombstones.get(messageId) ?? -1;
  if (
    revision < removedRevision ||
    (existing !== undefined && revision < existing.revision)
  ) {
    return false;
  }
  state.pendingInputs.delete(messageId);
  state.pendingInputTombstones.delete(messageId);
  state.pendingInputTombstones.set(messageId, revision);
  while (state.pendingInputTombstones.size > limits.max_pending_inputs) {
    const oldest = state.pendingInputTombstones.keys().next().value;
    if (oldest === undefined) {
      break;
    }
    state.pendingInputTombstones.delete(oldest);
  }
  return true;
}

function newDetail(turnId: string, detailRevision: number): MutableDetail {
  return {
    turnId,
    detailRevision,
    activities: new Map(),
    coverage: { kind: "unknown" },
    nextBeforeCursor: null,
    hasMore: false,
    invalidated: false,
    truncated: false,
  };
}

function insertDetail(
  state: MutableConversationState,
  detail: MutableDetail,
  limits: ConversationStateLimits,
): void {
  if (state.details.size >= limits.max_detail_turns) {
    const evictable = state.detailOrder.find((turnId) => {
      const turn = state.turns.get(turnId);
      return turn === undefined || turn.execution.kind === "terminal";
    });
    if (evictable === undefined) {
      throw new ConversationStateLimitError(
        "detail turns",
        limits.max_detail_turns,
      );
    }
    state.details.delete(evictable);
    state.detailOrder = state.detailOrder.filter((id) => id !== evictable);
  }
  state.details.set(detail.turnId, detail);
  touchDetail(state, detail.turnId);
}

function touchDetail(state: MutableConversationState, turnId: string): void {
  state.detailOrder = state.detailOrder.filter((id) => id !== turnId);
  state.detailOrder.push(turnId);
}

function mergeActivity(
  detail: MutableDetail,
  incoming: ConversationActivity,
): boolean {
  const existing = detail.activities.get(incoming.id);
  if (existing !== undefined) {
    if (incoming.revision < existing.revision) {
      return false;
    }
    if (incoming.revision === existing.revision) {
      if (JSON.stringify(incoming) !== JSON.stringify(existing)) {
        throw new ConversationProtocolError(
          `activity ${incoming.id} reused revision ${incoming.revision} with different content`,
        );
      }
      return false;
    }
  }
  detail.activities.set(incoming.id, incoming);
  return true;
}

function enforceActivityLimit(
  detail: MutableDetail,
  limits: ConversationStateLimits,
): void {
  if (detail.activities.size <= limits.max_activities_per_turn) {
    return;
  }
  const activities = [...detail.activities.values()].sort(compareActivities);
  const removeCount =
    activities.length - limits.max_activities_per_turn;
  for (const activity of activities.slice(0, removeCount)) {
    detail.activities.delete(activity.id);
  }
  detail.truncated = true;
  detail.hasMore = true;
}

function applyMutation(
  state: MutableConversationState,
  mutation: ConversationMutationMessage,
  limits: ConversationStateLimits,
): void {
  switch (mutation.type) {
    case "operator_upsert":
      upsertPendingInput(state, mutation.input, limits);
      return;
    case "operator_remove":
      removePendingInput(
        state,
        mutation.message_id,
        mutation.revision,
        limits,
      );
      return;
    case "turn_summary_upsert": {
      const known =
        state.loadedTurnIds.has(mutation.turn.turn_id) ||
        state.liveTurnOrder.includes(mutation.turn.turn_id) ||
        state.turns.has(mutation.turn.turn_id);
      if (!known && mutation.turn.execution.kind === "terminal") {
        return;
      }
      mergeTurn(
        state,
        mutation.turn,
        state.loadedTurnIds.has(mutation.turn.turn_id),
        limits,
      );
      return;
    }
    case "activity_upsert": {
      const turn = state.turns.get(mutation.turn_id);
      if (turn === undefined || turn.execution.kind !== "active") {
        return;
      }
      let detail = state.details.get(mutation.turn_id);
      if (detail === undefined) {
        detail = newDetail(mutation.turn_id, 0);
        insertDetail(state, detail, limits);
      }
      mergeActivity(detail, mutation.activity);
      enforceActivityLimit(detail, limits);
      touchDetail(state, mutation.turn_id);
      return;
    }
    case "detail_invalidated": {
      let detail = state.details.get(mutation.turn_id);
      if (detail === undefined) {
        if (!state.turns.has(mutation.turn_id)) {
          return;
        }
        detail = newDetail(mutation.turn_id, mutation.detail_revision);
        detail.invalidated = true;
        insertDetail(state, detail, limits);
      }
      if (mutation.detail_revision > detail.detailRevision) {
        detail.detailRevision = mutation.detail_revision;
        detail.invalidated = true;
        detail.nextBeforeCursor = null;
        detail.hasMore = false;
      }
      touchDetail(state, mutation.turn_id);
    }
  }
}

function detailView(detail: MutableDetail): ConversationDetailView {
  return {
    turn_id: detail.turnId,
    detail_revision: detail.detailRevision,
    activities: [...detail.activities.values()].sort(compareActivities),
    coverage: detail.coverage,
    next_before_cursor: detail.nextBeforeCursor,
    has_more: detail.hasMore,
    invalidated: detail.invalidated,
    truncated: detail.truncated,
  };
}
