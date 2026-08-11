import type { StreamEventEnvelopeDto } from "./client";
import {
  compactAgentTimelineItems,
  filterTimelineByDisplayLevel,
} from "./session-reducer";
import {
  createSessionProjectionState,
  deriveSessionTimeline,
  eventIdentityConflicts,
  reduceSessionProjection,
  type SessionProjectionAction,
  type SessionProjectionState,
} from "./session-projection";
import type {
  AgentSessionState,
  SemanticHistoryState,
} from "./runtime-store-helpers";
import type {
  AgentDetail,
  AgentTimelineItem,
  DisplayLevel,
} from "./types";

export const OPTIMISTIC_OPERATOR_PROMPT_SOURCE = "pending-operator-prompt";
export const OPTIMISTIC_OPERATOR_MESSAGE_PREFIX = "operator-prompt-message:";

export interface MergeConversationEventPageOptions {
  newestSeq?: number;
  eventLogEpoch?: string;
  historyDisplayLevel?: DisplayLevel;
  historyLoading?: boolean;
}

export function emptyAgentSession(): AgentSessionState {
  return {
    ...createSessionProjectionState(),
    loading: false,
    semanticHistoryByDisplayLevel: {},
    targetEventLoading: false,
    liveStatus: "idle",
    cacheStatus: "unchecked",
    contentStatus: "unknown",
    syncStatus: "idle",
    sendingPrompt: false,
    detail: null,
    workItemDetailsById: {},
    taskDetailsById: {},
    toolExecutionDetailsById: {},
  };
}

export function mergeCachedSessionIntoCurrent(
  current: AgentSessionState,
  cached: Partial<AgentSessionState>,
): AgentSessionState {
  if (
    current.detail ||
    current.eventSeqs.length > 0 ||
    Object.keys(current.messagesById).length > 0 ||
    Object.keys(current.transcriptEntriesById).length > 0 ||
    Object.keys(current.briefRecordsById).length > 0
  ) {
    return current;
  }
  return {
    ...current,
    ...cached,
    loading: current.loading,
    semanticHistoryByDisplayLevel:
      Object.keys(current.semanticHistoryByDisplayLevel).length > 0
        ? current.semanticHistoryByDisplayLevel
        : (cached.semanticHistoryByDisplayLevel ?? {}),
    targetEventLoading: current.targetEventLoading,
    liveStatus: current.liveStatus,
    sendingPrompt: current.sendingPrompt,
  };
}

export function applyProjectionAction(
  current: AgentSessionState,
  action: SessionProjectionAction,
  displayLevel: DisplayLevel = "debug",
  detailBase: AgentDetail | null = current.detail,
): AgentSessionState {
  const projection = reduceSessionProjection(current, action);
  return {
    ...current,
    ...projection,
    detail: materializeProjectionDetail(detailBase, projection, displayLevel),
  };
}

export function materializeProjectionDetail(
  detail: AgentDetail | null,
  projection: SessionProjectionState,
  displayLevel: DisplayLevel,
): AgentDetail | null {
  if (!detail) return null;
  const projectedTimeline = deriveSessionTimeline(projection, displayLevel);
  const projectedMessageIds = new Set(
    projectedTimeline.flatMap((item) =>
      item.kind === "operator" && item.id.startsWith("message:")
        ? [item.id.slice("message:".length)]
        : [],
    ),
  );
  const optimisticItems = detail.timeline.filter((item) => {
    if (!item.sourceIds.includes(OPTIMISTIC_OPERATOR_PROMPT_SOURCE)) return false;
    const canonicalMessageId = item.sourceIds
      .find((sourceId) => sourceId.startsWith(OPTIMISTIC_OPERATOR_MESSAGE_PREFIX))
      ?.slice(OPTIMISTIC_OPERATOR_MESSAGE_PREFIX.length);
    return !canonicalMessageId || !projectedMessageIds.has(canonicalMessageId);
  });
  const timeline = compactAgentTimelineItems([
    ...projectedTimeline,
    ...optimisticItems,
  ]).sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
  return {
    ...detail,
    timeline,
    events: projection.eventSeqs.map((seq) => projection.eventsBySeq[seq]),
    eventLogEpoch: projection.eventLogEpoch,
    newestEventSeq: projection.newestSeq,
    oldestEventSeq: projection.oldestSeq,
    briefRecordsById: projection.briefRecordsById,
  };
}

export function eventLogEpochFromEvents(events: StreamEventEnvelopeDto[]): string | undefined {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const epoch = events[index]?.event_log_epoch;
    if (epoch) return epoch;
  }
  return undefined;
}

export function shouldResetForEventLogEpoch(
  current: AgentSessionState,
  incomingEpoch: string | undefined,
): boolean {
  if (!incomingEpoch) return false;
  return (
    (current.eventLogEpoch != null || current.eventSeqs.length > 0) &&
    current.eventLogEpoch !== incomingEpoch
  );
}

export function sessionForEventLogEpoch(
  current: AgentSessionState,
  incomingEpoch: string | undefined,
): AgentSessionState {
  if (!incomingEpoch) return current;
  if (!shouldResetForEventLogEpoch(current, incomingEpoch)) {
    return current.eventLogEpoch === incomingEpoch
      ? current
      : { ...current, eventLogEpoch: incomingEpoch };
  }
  const reset = applyProjectionAction(current, { type: "reset", eventLogEpoch: incomingEpoch });
  return {
    ...reset,
    semanticHistoryByDisplayLevel: {},
    targetEventLoading: false,
    targetEventError: undefined,
    detail: reset.detail
      ? {
          ...reset.detail,
          eventCursorSeq: undefined,
          hasOlderEvents: undefined,
        }
      : null,
  };
}

export function hasEventIdentityConflict(
  current: AgentSessionState,
  incomingEvents: StreamEventEnvelopeDto[],
): boolean {
  return eventIdentityConflicts(current, incomingEvents);
}

export function resetSessionForEventConflict(
  current: AgentSessionState,
  eventLogEpoch?: string,
): AgentSessionState {
  return {
    ...applyProjectionAction(current, {
      type: "reset",
      eventLogEpoch: eventLogEpoch ?? current.eventLogEpoch,
      reason: "event_identity_conflict",
    }),
    liveStatus: "stale",
    semanticHistoryByDisplayLevel: {},
    targetEventLoading: false,
    error: "runtime event identity conflict; refreshing projection",
  };
}

export function semanticHistoryState(
  session: AgentSessionState | undefined,
  displayLevel: DisplayLevel,
): SemanticHistoryState {
  return session?.semanticHistoryByDisplayLevel[displayLevel] ?? {
    eventLogEpoch: session?.eventLogEpoch,
    cursorSeq: undefined,
    hasOlder: false,
    loading: false,
  };
}

export function withSemanticHistoryState(
  session: AgentSessionState | undefined,
  displayLevel: DisplayLevel,
  history: SemanticHistoryState,
): AgentSessionState {
  const current = session ?? emptyAgentSession();
  return {
    ...current,
    semanticHistoryByDisplayLevel: {
      ...current.semanticHistoryByDisplayLevel,
      [displayLevel]: history,
    },
  };
}

export function semanticTimelineItemIds(
  session: AgentSessionState | undefined,
  displayLevel: DisplayLevel,
): Set<string> {
  return new Set(semanticTimeline(session, displayLevel).map((item) => item.id));
}

export function semanticTimelineHasNewItem(
  session: AgentSessionState | undefined,
  displayLevel: DisplayLevel,
  initialItemIds: Set<string>,
): boolean {
  return semanticTimeline(session, displayLevel).some((item) => !initialItemIds.has(item.id));
}

export function semanticTimeline(
  session: AgentSessionState | undefined,
  displayLevel: DisplayLevel,
): AgentTimelineItem[] {
  return session
    ? filterTimelineByDisplayLevel(
        deriveSessionTimeline(session, displayLevel),
        displayLevel,
        { itemLimit: Number.MAX_SAFE_INTEGER },
      )
    : [];
}

export function mergeEventPageIntoConversation(
  session: AgentSessionState | undefined,
  pageEvents: StreamEventEnvelopeDto[],
  pageOldestSeq: number | undefined,
  pageHasOlder: boolean | undefined,
  displayLevel: DisplayLevel,
  options: MergeConversationEventPageOptions = {},
): AgentSessionState {
  const epochSession = sessionForEventLogEpoch(
    session ?? emptyAgentSession(),
    options.eventLogEpoch,
  );
  const current = hasEventIdentityConflict(epochSession, pageEvents)
    ? resetSessionForEventConflict(epochSession, options.eventLogEpoch)
    : epochSession;
  const projected = applyProjectionAction(current, {
    type: "events_received",
    events: pageEvents,
    eventLogEpoch: options.eventLogEpoch,
  }, displayLevel, current.detail);
  const historyDisplayLevel = options.historyDisplayLevel;
  const semanticHistoryByDisplayLevel = historyDisplayLevel
    ? {
        ...projected.semanticHistoryByDisplayLevel,
        [historyDisplayLevel]: {
          eventLogEpoch: options.eventLogEpoch ?? projected.eventLogEpoch,
          cursorSeq: pageOldestSeq,
          hasOlder: pageHasOlder ?? false,
          loading: options.historyLoading ?? false,
        },
      }
    : projected.semanticHistoryByDisplayLevel;

  return {
    ...projected,
    newestSeq: Math.max(options.newestSeq ?? 0, projected.newestSeq ?? 0) || undefined,
    oldestSeq:
      pageOldestSeq != null && projected.oldestSeq != null
        ? Math.min(pageOldestSeq, projected.oldestSeq)
        : (pageOldestSeq ?? projected.oldestSeq),
    semanticHistoryByDisplayLevel,
  };
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}
