import {
  ConversationDecodeError,
  type ConversationHttpErrorBody,
} from "./errors.js";
import {
  CONVERSATION_QUERY_VERSION,
  CONVERSATION_SCHEMA_VERSION,
  type ActivityKey,
  type Attention,
  type BriefContentSource,
  type BriefRecord,
  type ConversationActivity,
  type ConversationActivityResponse,
  type ConversationCheckpoint,
  type ConversationDetailCursor,
  type ConversationHandshake,
  type ConversationHistoryCursor,
  type ConversationResetReason,
  type ConversationStreamMessage,
  type ConversationSummaryResponse,
  type ConversationTurnSummary,
  type TurnInputSummary,
  type DetailCoverage,
  type DetailCoverageReason,
  type ExecutionState,
  type NoBriefReason,
  type PendingInput,
  type PresentationClass,
  type ResultState,
  type ResultUnavailableReason,
  type TerminalOutcome,
  type TurnKey,
} from "./types.js";

type JsonRecord = Record<string, unknown>;

const terminalOutcomes = new Set<TerminalOutcome>([
  "completed",
  "aborted",
  "interrupted",
  "baseline_over_budget",
  "deferred_to_fallback",
  "provider_failed_needs_recovery",
]);
const resultUnavailableReasons = new Set<ResultUnavailableReason>([
  "missing_canonical_linkage",
  "retention_gap",
  "legacy_coverage",
]);
const detailCoverageReasons = new Set<DetailCoverageReason>([
  "retention_gap",
  "legacy_ownership",
  "unknown_activity_type",
  "missing_canonical_linkage",
]);
const presentationClasses = new Set<PresentationClass>([
  "operator",
  "task",
  "external",
  "timer",
  "internal",
  "system",
  "operational",
]);
const resetReasons = new Set<ConversationResetReason>([
  "retention_expired",
  "cursor_ahead",
  "replay_limit_exceeded",
  "schema_version_mismatch",
  "query_version_mismatch",
  "event_log_epoch_mismatch",
  "cursor_rejected",
  "agent_not_found",
  "stream_recovery_failed",
  "slow_consumer",
]);

function fail(path: string, message: string): never {
  throw new ConversationDecodeError(path, message);
}

function record(value: unknown, path: string): JsonRecord {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    fail(path, "expected object");
  }
  return value as JsonRecord;
}

function stringValue(value: unknown, path: string): string {
  if (typeof value !== "string") {
    fail(path, "expected string");
  }
  return value;
}

function nonEmptyString(value: unknown, path: string): string {
  const decoded = stringValue(value, path);
  if (decoded.length === 0) {
    fail(path, "expected non-empty string");
  }
  return decoded;
}

function booleanValue(value: unknown, path: string): boolean {
  if (typeof value !== "boolean") {
    fail(path, "expected boolean");
  }
  return value;
}

function safeInteger(value: unknown, path: string): number {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < 0
  ) {
    fail(path, "expected non-negative safe integer");
  }
  return value;
}

function nullableSafeInteger(value: unknown, path: string): number | null {
  return value === null ? null : safeInteger(value, path);
}

function arrayValue(value: unknown, path: string): readonly unknown[] {
  if (!Array.isArray(value)) {
    fail(path, "expected array");
  }
  return value;
}

function nullableString(value: unknown, path: string): string | null {
  return value === null ? null : stringValue(value, path);
}

function optionalString(
  source: JsonRecord,
  key: string,
  path: string,
): string | undefined {
  const value = source[key];
  if (value === undefined || value === null) {
    return undefined;
  }
  return stringValue(value, `${path}.${key}`);
}

function enumValue<T extends string>(
  value: unknown,
  allowed: ReadonlySet<T>,
  path: string,
): T {
  const decoded = stringValue(value, path) as T;
  if (!allowed.has(decoded)) {
    fail(path, `unsupported value ${JSON.stringify(decoded)}`);
  }
  return decoded;
}

function opaque<Name extends string>(
  value: unknown,
  path: string,
): string & { readonly __opaque?: Name } {
  return nonEmptyString(value, path);
}

function decodeTurnKey(value: unknown, path: string): TurnKey {
  const source = record(value, path);
  return {
    turn_index: safeInteger(source.turn_index, `${path}.turn_index`),
    turn_id: nonEmptyString(source.turn_id, `${path}.turn_id`),
  };
}

function decodeActivityKey(value: unknown, path: string): ActivityKey {
  const source = record(value, path);
  return {
    event_seq: safeInteger(source.event_seq, `${path}.event_seq`),
    activity_id: nonEmptyString(source.activity_id, `${path}.activity_id`),
  };
}

function decodeTerminalOutcome(value: unknown, path: string): TerminalOutcome {
  return enumValue(value, terminalOutcomes, path);
}

function decodeExecutionState(value: unknown, path: string): ExecutionState {
  const source = record(value, path);
  const kind = stringValue(source.kind, `${path}.kind`);
  if (kind === "active") {
    return { kind };
  }
  if (kind === "terminal") {
    return {
      kind,
      outcome: decodeTerminalOutcome(source.outcome, `${path}.outcome`),
    };
  }
  return fail(`${path}.kind`, `unsupported value ${JSON.stringify(kind)}`);
}

function decodeNoBriefReason(value: unknown, path: string): NoBriefReason {
  const source = record(value, path);
  const kind = stringValue(source.kind, `${path}.kind`);
  switch (kind) {
    case "reducer_only":
      return {
        kind,
        reason: stringValue(source.reason, `${path}.reason`),
      };
    case "aborted":
    case "interrupted":
    case "tool_only_wait":
      return { kind };
    default:
      return fail(`${path}.kind`, `unsupported value ${JSON.stringify(kind)}`);
  }
}

function decodeResultState(value: unknown, path: string): ResultState {
  const source = record(value, path);
  const kind = stringValue(source.kind, `${path}.kind`);
  switch (kind) {
    case "pending":
    case "available":
      return { kind };
    case "none":
      return {
        kind,
        reason: decodeNoBriefReason(source.reason, `${path}.reason`),
      };
    case "unavailable":
      return {
        kind,
        reason: enumValue(
          source.reason,
          resultUnavailableReasons,
          `${path}.reason`,
        ),
        retryable: booleanValue(source.retryable, `${path}.retryable`),
      };
    default:
      return fail(`${path}.kind`, `unsupported value ${JSON.stringify(kind)}`);
  }
}

function decodeAttention(value: unknown, path: string): Attention | null {
  if (value === null) {
    return null;
  }
  const source = record(value, path);
  const kind = stringValue(source.kind, `${path}.kind`);
  switch (kind) {
    case "failed":
      return {
        kind,
        outcome: decodeTerminalOutcome(source.outcome, `${path}.outcome`),
      };
    case "interrupted":
    case "waiting":
      return { kind };
    default:
      return fail(`${path}.kind`, `unsupported value ${JSON.stringify(kind)}`);
  }
}

export function decodeDetailCoverage(
  value: unknown,
  path = "$",
): DetailCoverage {
  const source = record(value, path);
  const kind = stringValue(source.kind, `${path}.kind`);
  switch (kind) {
    case "complete":
    case "unknown":
      return { kind };
    case "partial":
    case "unavailable":
      return {
        kind,
        reason: enumValue(
          source.reason,
          detailCoverageReasons,
          `${path}.reason`,
        ),
      };
    default:
      return fail(`${path}.kind`, `unsupported value ${JSON.stringify(kind)}`);
  }
}

export function decodePendingInput(
  value: unknown,
  path = "$",
): PendingInput {
  const source = record(value, path);
  const state = stringValue(source.state, `${path}.state`);
  if (state !== "queued" && state !== "assigning") {
    fail(`${path}.state`, `unsupported value ${JSON.stringify(state)}`);
  }
  return {
    message_id: nonEmptyString(source.message_id, `${path}.message_id`),
    revision: safeInteger(source.revision, `${path}.revision`),
    state,
    ...(source.presentation_class === undefined ? {} : {
      presentation_class: enumValue(source.presentation_class, presentationClasses, `${path}.presentation_class`),
    }),
    ...(source.created_at === undefined ? {} : {
      created_at: optionalTimestamp(source.created_at, `${path}.created_at`),
    }),
    preview:
      source.preview === undefined || source.preview === null
        ? ""
        : stringValue(source.preview, `${path}.preview`),
  };
}

export function decodeTurnInputSummary(
  value: unknown,
  path = "$",
): TurnInputSummary {
  const source = record(value, path);
  return {
    message_id: nonEmptyString(source.message_id, `${path}.message_id`),
    preview:
      source.preview === undefined || source.preview === null
        ? ""
        : stringValue(source.preview, `${path}.preview`),
  };
}

function optionalTimestamp(value: unknown, path: string): string | null {
  if (value == null) return null;
  const decoded = stringValue(value, path);
  if (!Number.isFinite(Date.parse(decoded))) fail(path, "expected timestamp");
  return decoded;
}

export function decodeTurnSummary(
  value: unknown,
  path = "$",
): ConversationTurnSummary {
  const source = record(value, path);
  const turnId = nonEmptyString(source.turn_id, `${path}.turn_id`);
  const key = decodeTurnKey(source.key, `${path}.key`);
  if (key.turn_id !== turnId) {
    fail(`${path}.key.turn_id`, "must match turn_id");
  }
  return {
    turn_id: turnId,
    key,
    revision: safeInteger(source.revision, `${path}.revision`),
    presentation_class: enumValue(
      source.presentation_class,
      presentationClasses,
      `${path}.presentation_class`,
    ),
    inputs: arrayValue(source.inputs, `${path}.inputs`).map(
      (input, index) => decodeTurnInputSummary(input, `${path}.inputs[${index}]`),
    ),
    execution: decodeExecutionState(source.execution, `${path}.execution`),
    started_at: optionalTimestamp(source.started_at, `${path}.started_at`),
    completed_at: optionalTimestamp(source.completed_at, `${path}.completed_at`),
    duration_ms: source.duration_ms == null ? null : safeInteger(source.duration_ms, `${path}.duration_ms`),
    result: decodeResultState(source.result, `${path}.result`),
    settled: booleanValue(source.settled, `${path}.settled`),
    attention: decodeAttention(source.attention, `${path}.attention`),
    detail_coverage: decodeDetailCoverage(
      source.detail_coverage,
      `${path}.detail_coverage`,
    ),
    brief_ids: arrayValue(source.brief_ids, `${path}.brief_ids`).map(
      (briefId, index) =>
        nonEmptyString(briefId, `${path}.brief_ids[${index}]`),
    ),
  };
}

export function decodeConversationActivity(
  value: unknown,
  path = "$",
): ConversationActivity {
  const source = record(value, path);
  const id = nonEmptyString(source.id, `${path}.id`);
  const key = decodeActivityKey(source.key, `${path}.key`);
  if (key.activity_id !== id) {
    fail(`${path}.key.activity_id`, "must match id");
  }
  const kind = stringValue(source.kind, `${path}.kind`);
  if (
    kind !== "operator" &&
    kind !== "assistant" &&
    kind !== "tool" &&
    kind !== "wait" &&
    kind !== "error"
  ) {
    fail(`${path}.kind`, `unsupported value ${JSON.stringify(kind)}`);
  }
  return {
    kind,
    id,
    key,
    revision: safeInteger(source.revision, `${path}.revision`),
    summary: stringValue(source.summary, `${path}.summary`),
  };
}

function decodeBoundary(source: JsonRecord, path: string) {
  const schemaVersion = safeInteger(
    source.schema_version,
    `${path}.schema_version`,
  );
  if (schemaVersion !== CONVERSATION_SCHEMA_VERSION) {
    fail(
      `${path}.schema_version`,
      `expected ${CONVERSATION_SCHEMA_VERSION}, received ${schemaVersion}`,
    );
  }
  const queryVersion = safeInteger(
    source.query_version,
    `${path}.query_version`,
  );
  if (queryVersion !== CONVERSATION_QUERY_VERSION) {
    fail(
      `${path}.query_version`,
      `expected ${CONVERSATION_QUERY_VERSION}, received ${queryVersion}`,
    );
  }
  return {
    schema_version: schemaVersion,
    query_version: queryVersion,
    runtime_id: nonEmptyString(source.runtime_id, `${path}.runtime_id`),
    event_log_epoch: nonEmptyString(
      source.event_log_epoch,
      `${path}.event_log_epoch`,
    ),
    visibility_scope_id: nonEmptyString(
      source.visibility_scope_id,
      `${path}.visibility_scope_id`,
    ),
    snapshot_through_seq: safeInteger(
      source.snapshot_through_seq,
      `${path}.snapshot_through_seq`,
    ),
    event_head_seq: safeInteger(
      source.event_head_seq,
      `${path}.event_head_seq`,
    ),
    oldest_retained_seq: safeInteger(
      source.oldest_retained_seq,
      `${path}.oldest_retained_seq`,
    ),
    snapshot_cursor: opaque<"conversation-checkpoint">(
      source.snapshot_cursor,
      `${path}.snapshot_cursor`,
    ) as ConversationCheckpoint,
  };
}

export function decodeConversationSummaryResponse(
  value: unknown,
  path = "$",
): ConversationSummaryResponse {
  const source = record(value, path);
  return {
    ...decodeBoundary(source, path),
    turns: arrayValue(source.turns, `${path}.turns`).map((turn, index) =>
      decodeTurnSummary(turn, `${path}.turns[${index}]`),
    ),
    active_turns: arrayValue(
      source.active_turns,
      `${path}.active_turns`,
    ).map((turn, index) =>
      decodeTurnSummary(turn, `${path}.active_turns[${index}]`),
    ),
    pending_inputs: arrayValue(
      source.pending_inputs,
      `${path}.pending_inputs`,
    ).map((input, index) =>
      decodePendingInput(input, `${path}.pending_inputs[${index}]`),
    ),
    next_before_cursor:
      source.next_before_cursor === null
        ? null
        : (opaque<"conversation-history-cursor">(
            source.next_before_cursor,
            `${path}.next_before_cursor`,
          ) as ConversationHistoryCursor),
    has_more: booleanValue(source.has_more, `${path}.has_more`),
  };
}

export function decodeConversationActivityResponse(
  value: unknown,
  path = "$",
): ConversationActivityResponse {
  const source = record(value, path);
  return {
    ...decodeBoundary(source, path),
    turn: decodeTurnSummary(source.turn, `${path}.turn`),
    detail_revision: safeInteger(
      source.detail_revision,
      `${path}.detail_revision`,
    ),
    activities: arrayValue(source.activities, `${path}.activities`).map(
      (activity, index) =>
        decodeConversationActivity(
          activity,
          `${path}.activities[${index}]`,
        ),
    ),
    coverage: decodeDetailCoverage(source.coverage, `${path}.coverage`),
    next_before_cursor:
      source.next_before_cursor === null
        ? null
        : (opaque<"conversation-detail-cursor">(
            source.next_before_cursor,
            `${path}.next_before_cursor`,
          ) as ConversationDetailCursor),
    has_more: booleanValue(source.has_more, `${path}.has_more`),
  };
}

function decodeStreamScope(source: JsonRecord, path: string) {
  return {
    event_log_epoch: nonEmptyString(
      source.event_log_epoch,
      `${path}.event_log_epoch`,
    ),
    visibility_scope_id: nonEmptyString(
      source.visibility_scope_id,
      `${path}.visibility_scope_id`,
    ),
  };
}

function decodeResetReason(
  value: unknown,
  path: string,
): ConversationResetReason {
  return enumValue(value, resetReasons, path);
}

export function decodeConversationStreamMessage(
  value: unknown,
  path = "$",
): ConversationStreamMessage {
  const source = record(value, path);
  const type = stringValue(source.type, `${path}.type`);
  switch (type) {
    case "batch_begin": {
      const schemaVersion = safeInteger(
        source.schema_version,
        `${path}.schema_version`,
      );
      const queryVersion = safeInteger(
        source.query_version,
        `${path}.query_version`,
      );
      if (schemaVersion !== CONVERSATION_SCHEMA_VERSION) {
        fail(`${path}.schema_version`, "unsupported schema version");
      }
      if (queryVersion !== CONVERSATION_QUERY_VERSION) {
        fail(`${path}.query_version`, "unsupported query version");
      }
      return {
        type,
        batch_id: nonEmptyString(source.batch_id, `${path}.batch_id`),
        schema_version: schemaVersion,
        query_version: queryVersion,
        runtime_id: nonEmptyString(source.runtime_id, `${path}.runtime_id`),
        ...decodeStreamScope(source, path),
        from_seq: safeInteger(source.from_seq, `${path}.from_seq`),
        through_seq: safeInteger(source.through_seq, `${path}.through_seq`),
      };
    }
    case "operator_upsert":
      return {
        type,
        ...decodeStreamScope(source, path),
        input: decodePendingInput(source.input, `${path}.input`),
      };
    case "operator_remove":
      return {
        type,
        ...decodeStreamScope(source, path),
        message_id: nonEmptyString(source.message_id, `${path}.message_id`),
        revision: safeInteger(source.revision, `${path}.revision`),
      };
    case "turn_summary_upsert":
      return {
        type,
        ...decodeStreamScope(source, path),
        turn: decodeTurnSummary(source.turn, `${path}.turn`),
      };
    case "activity_upsert":
      return {
        type,
        ...decodeStreamScope(source, path),
        turn_id: nonEmptyString(source.turn_id, `${path}.turn_id`),
        activity: decodeConversationActivity(
          source.activity,
          `${path}.activity`,
        ),
      };
    case "detail_invalidated":
      return {
        type,
        ...decodeStreamScope(source, path),
        turn_id: nonEmptyString(source.turn_id, `${path}.turn_id`),
        detail_revision: safeInteger(
          source.detail_revision,
          `${path}.detail_revision`,
        ),
      };
    case "checkpoint":
      return {
        type,
        batch_id: nonEmptyString(source.batch_id, `${path}.batch_id`),
        ...decodeStreamScope(source, path),
        through_seq: safeInteger(source.through_seq, `${path}.through_seq`),
        checkpoint: opaque<"conversation-checkpoint">(
          source.checkpoint,
          `${path}.checkpoint`,
        ) as ConversationCheckpoint,
      };
    case "reset_required":
      return {
        type,
        reason: decodeResetReason(source.reason, `${path}.reason`),
        oldest_retained_seq: nullableSafeInteger(
          source.oldest_retained_seq,
          `${path}.oldest_retained_seq`,
        ),
        event_head_seq: nullableSafeInteger(
          source.event_head_seq,
          `${path}.event_head_seq`,
        ),
        hint: stringValue(source.hint, `${path}.hint`),
      };
    default:
      return fail(`${path}.type`, `unsupported value ${JSON.stringify(type)}`);
  }
}

function decodeBriefContentSource(
  value: unknown,
  path: string,
): BriefContentSource {
  if (value === undefined || value === null) {
    return { kind: "inline" };
  }
  const source = record(value, path);
  const kind = stringValue(source.kind, `${path}.kind`);
  if (kind === "inline") {
    return { kind };
  }
  if (kind === "transcript_entry") {
    const relation =
      source.relation === undefined
        ? "derived_from"
        : stringValue(source.relation, `${path}.relation`);
    if (
      relation !== "derived_from" &&
      relation !== "finalizes" &&
      relation !== "excerpt"
    ) {
      fail(`${path}.relation`, `unsupported value ${JSON.stringify(relation)}`);
    }
    return {
      kind,
      entry_id: nonEmptyString(source.entry_id, `${path}.entry_id`),
      relation,
    };
  }
  return fail(`${path}.kind`, `unsupported value ${JSON.stringify(kind)}`);
}

export function decodeBriefRecord(value: unknown, path = "$"): BriefRecord {
  const source = record(value, path);
  const kind = stringValue(source.kind, `${path}.kind`);
  if (kind !== "ack" && kind !== "result" && kind !== "failure") {
    fail(`${path}.kind`, `unsupported value ${JSON.stringify(kind)}`);
  }
  const citations =
    source.citations === undefined || source.citations === null
      ? undefined
      : arrayValue(source.citations, `${path}.citations`).map(
          (citation, index) => {
            const citationPath = `${path}.citations[${index}]`;
            const decoded = record(citation, citationPath);
            const title = optionalString(decoded, "title", citationPath);
            return {
              url: nonEmptyString(decoded.url, `${citationPath}.url`),
              ...(title === undefined ? {} : { title }),
            };
          },
        );
  const attachments =
    source.attachments === null || source.attachments === undefined
      ? null
      : [...arrayValue(source.attachments, `${path}.attachments`)];
  const workItemId = optionalString(source, "work_item_id", path);
  const turnId = optionalString(source, "turn_id", path);
  const finalizesAssistantRoundId = optionalString(
    source,
    "finalizes_assistant_round_id",
    path,
  );
  const createdEventSeq =
    source.created_event_seq === undefined || source.created_event_seq === null
      ? undefined
      : safeInteger(source.created_event_seq, `${path}.created_event_seq`);
  const turnIndex =
    source.turn_index === undefined || source.turn_index === null
      ? undefined
      : safeInteger(source.turn_index, `${path}.turn_index`);
  return {
    id: nonEmptyString(source.id, `${path}.id`),
    agent_id: nonEmptyString(source.agent_id, `${path}.agent_id`),
    workspace_id: nonEmptyString(source.workspace_id, `${path}.workspace_id`),
    ...(workItemId === undefined ? {} : { work_item_id: workItemId }),
    ...(turnIndex === undefined ? {} : { turn_index: turnIndex }),
    ...(turnId === undefined ? {} : { turn_id: turnId }),
    kind,
    created_at: nonEmptyString(source.created_at, `${path}.created_at`),
    content_source: decodeBriefContentSource(
      source.content_source,
      `${path}.content_source`,
    ),
    ...(finalizesAssistantRoundId === undefined
      ? {}
      : { finalizes_assistant_round_id: finalizesAssistantRoundId }),
    text: stringValue(source.text, `${path}.text`),
    ...(citations === undefined ? {} : { citations }),
    ...(createdEventSeq === undefined
      ? {}
      : { created_event_seq: createdEventSeq }),
    attachments,
    related_message_id: nullableString(
      source.related_message_id,
      `${path}.related_message_id`,
    ),
    related_task_id: nullableString(
      source.related_task_id,
      `${path}.related_task_id`,
    ),
  };
}

export function decodeConversationHandshake(
  value: unknown,
  path = "$",
): ConversationHandshake {
  const source = record(value, path);
  if (source.ok !== true) {
    fail(`${path}.ok`, "expected true");
  }
  const protocol = record(source.protocol, `${path}.protocol`);
  const auth = record(source.auth, `${path}.auth`);
  return {
    ok: true,
    protocol: {
      name: nonEmptyString(protocol.name, `${path}.protocol.name`),
      version: safeInteger(protocol.version, `${path}.protocol.version`),
    },
    auth: {
      mode: nonEmptyString(auth.mode, `${path}.auth.mode`),
      required: booleanValue(auth.required, `${path}.auth.required`),
    },
    capabilities: arrayValue(
      source.capabilities,
      `${path}.capabilities`,
    ).map((capability, index) =>
      nonEmptyString(capability, `${path}.capabilities[${index}]`),
    ),
    runtime: record(source.runtime, `${path}.runtime`),
  };
}

export function decodeConversationHttpError(
  value: unknown,
  path = "$",
): ConversationHttpErrorBody {
  const source = record(value, path);
  if (source.ok !== false) {
    fail(`${path}.ok`, "expected false");
  }
  const code = optionalString(source, "code", path);
  const hint = optionalString(source, "hint", path);
  const retryable =
    source.retryable === undefined
      ? undefined
      : booleanValue(source.retryable, `${path}.retryable`);
  return {
    ...source,
    ok: false,
    error: nonEmptyString(source.error, `${path}.error`),
    ...(code === undefined ? {} : { code }),
    ...(hint === undefined ? {} : { hint }),
    ...(retryable === undefined ? {} : { retryable }),
  };
}

export function isConversationResetReason(
  value: unknown,
): value is ConversationResetReason {
  return typeof value === "string" && resetReasons.has(value as ConversationResetReason);
}
