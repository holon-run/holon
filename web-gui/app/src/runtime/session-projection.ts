import {
  applySessionEvent,
  briefIdForPayload,
  buildSessionState,
  transcriptEntryIdForPayload,
} from "./session-reducer-core";
import { canApplySessionEvent, type SessionEventEnvelope } from "./session-events";
import { cloneSessionState, createSessionState, type SessionState } from "./session-state-reducer";
import { deriveTimelineView } from "./timeline-view-model";
import type {
  AgentTimelineItem,
  BriefHydrationViewState,
  DisplayLevel,
  RuntimeBriefRecord,
  RuntimeMessageEnvelope,
  RuntimeTranscriptEntry,
} from "./types";

export type ProjectionEvent = SessionEventEnvelope;

export const SESSION_PROJECTION_GENERATION = 1;

export interface EventGap {
  afterSeq: number;
  beforeSeq: number;
}

export interface SessionProjectionState {
  generation: number;
  eventLogEpoch?: string;
  eventsBySeq: Record<number, ProjectionEvent>;
  eventSeqs: number[];
  eventsById: Record<string, ProjectionEvent>;
  domain: SessionState;
  unknownEventIds: string[];
  gaps: EventGap[];
  messagesById: Record<string, RuntimeMessageEnvelope>;
  missingMessageIds: Record<string, true>;
  transcriptEntriesById: Record<string, RuntimeTranscriptEntry>;
  missingTranscriptEntryIds: Record<string, true>;
  briefRecordsById: Record<string, RuntimeBriefRecord>;
  missingBriefIds: Record<string, true>;
  briefHydrationById: Record<string, BriefHydrationViewState>;
  referencedMessageIds: Record<string, true>;
  referencedTranscriptEntryIds: Record<string, true>;
  referencedBriefIds: Record<string, true>;
  newestSeq?: number;
  oldestSeq?: number;
  invalidatedReason?: "event_identity_conflict" | "cache_generation_mismatch";
}

export type SessionProjectionAction =
  | {
      type: "events_received";
      events: ProjectionEvent[];
      eventLogEpoch?: string;
    }
  | {
      type: "cache_restored";
      generation: number;
      eventLogEpoch?: string;
      eventsBySeq: Record<number, ProjectionEvent>;
      eventSeqs: number[];
      messagesById: Record<string, RuntimeMessageEnvelope>;
      transcriptEntriesById: Record<string, RuntimeTranscriptEntry>;
      briefRecordsById: Record<string, RuntimeBriefRecord>;
      newestSeq?: number;
      oldestSeq?: number;
    }
  | {
      type: "messages_hydrated";
      messages: RuntimeMessageEnvelope[];
      missingIds: string[];
    }
  | {
      type: "transcripts_hydrated";
      entries: RuntimeTranscriptEntry[];
      missingIds: string[];
    }
  | {
      type: "briefs_hydrated";
      recordsById: Record<string, RuntimeBriefRecord>;
      missingIds: string[];
    }
  | {
      type: "briefs_hydration_started";
      briefIds: string[];
    }
  | {
      type: "briefs_hydration_failed";
      briefIds: string[];
      errorKind: string;
    }
  | {
      type: "reset";
      eventLogEpoch?: string;
      reason?: SessionProjectionState["invalidatedReason"];
    };

export function createSessionProjectionState(eventLogEpoch?: string): SessionProjectionState {
  return {
    generation: SESSION_PROJECTION_GENERATION,
    eventLogEpoch,
    eventsBySeq: {},
    eventSeqs: [],
    eventsById: {},
    domain: createSessionState(),
    unknownEventIds: [],
    gaps: [],
    messagesById: {},
    missingMessageIds: {},
    transcriptEntriesById: {},
    missingTranscriptEntryIds: {},
    briefRecordsById: {},
    missingBriefIds: {},
    briefHydrationById: {},
    referencedMessageIds: {},
    referencedTranscriptEntryIds: {},
    referencedBriefIds: {},
    newestSeq: undefined,
    oldestSeq: undefined,
    invalidatedReason: undefined,
  };
}

export function reduceSessionProjection(
  current: SessionProjectionState,
  action: SessionProjectionAction,
): SessionProjectionState {
  switch (action.type) {
    case "reset":
      return {
        ...createSessionProjectionState(action.eventLogEpoch),
        invalidatedReason: action.reason,
      };
    case "cache_restored": {
      if (action.generation !== SESSION_PROJECTION_GENERATION) {
        return reduceSessionProjection(current, {
          type: "reset",
          eventLogEpoch: action.eventLogEpoch,
          reason: "cache_generation_mismatch",
        });
      }
      const eventsBySeq = normalizeEventsBySeq(action.eventsBySeq);
      return rebuildProjection({
        ...createSessionProjectionState(action.eventLogEpoch),
        eventsBySeq,
        eventSeqs: action.eventSeqs,
        messagesById: action.messagesById,
        transcriptEntriesById: action.transcriptEntriesById,
        briefRecordsById: action.briefRecordsById,
        newestSeq: action.newestSeq,
        oldestSeq: action.oldestSeq,
      });
    }
    case "events_received":
      return reduceEvents(current, action.events, action.eventLogEpoch);
    case "messages_hydrated": {
      const messagesById = { ...current.messagesById };
      for (const message of action.messages) {
        if (message.id) messagesById[message.id] = message;
      }
      return {
        ...current,
        messagesById,
        missingMessageIds: mergeMissingIds(current.missingMessageIds, action.missingIds, Object.keys(messagesById)),
      };
    }
    case "transcripts_hydrated": {
      const transcriptEntriesById = { ...current.transcriptEntriesById };
      for (const entry of action.entries) {
        if (entry.id) transcriptEntriesById[entry.id] = entry;
      }
      return {
        ...current,
        transcriptEntriesById,
        missingTranscriptEntryIds: mergeMissingIds(
          current.missingTranscriptEntryIds,
          action.missingIds,
          Object.keys(transcriptEntriesById),
        ),
      };
    }
    case "briefs_hydrated": {
      const briefRecordsById = { ...current.briefRecordsById, ...action.recordsById };
      const briefHydrationById = { ...current.briefHydrationById };
      for (const briefId of Object.keys(action.recordsById)) {
        briefHydrationById[briefId] = {
          briefId,
          status: "resolved",
          attempt: briefHydrationById[briefId]?.attempt ?? 1,
        };
      }
      for (const briefId of action.missingIds) {
        briefHydrationById[briefId] = {
          briefId,
          status: "not_found",
          attempt: briefHydrationById[briefId]?.attempt ?? 1,
        };
      }
      return {
        ...current,
        briefRecordsById,
        briefHydrationById,
        missingBriefIds: mergeMissingIds(current.missingBriefIds, action.missingIds, Object.keys(briefRecordsById)),
      };
    }
    case "briefs_hydration_started": {
      const briefHydrationById = { ...current.briefHydrationById };
      const missingBriefIds = { ...current.missingBriefIds };
      for (const briefId of action.briefIds) {
        const previous = briefHydrationById[briefId];
        briefHydrationById[briefId] = {
          briefId,
          status: "loading",
          attempt: (previous?.attempt ?? 0) + 1,
        };
        delete missingBriefIds[briefId];
      }
      return { ...current, briefHydrationById, missingBriefIds };
    }
    case "briefs_hydration_failed": {
      const briefHydrationById = { ...current.briefHydrationById };
      for (const briefId of action.briefIds) {
        briefHydrationById[briefId] = {
          briefId,
          status: "failed",
          attempt: briefHydrationById[briefId]?.attempt ?? 1,
          errorKind: action.errorKind,
        };
      }
      return { ...current, briefHydrationById };
    }
  }
}

export function deriveSessionTimeline(
  projection: SessionProjectionState,
  eventDisplayLevel: DisplayLevel = "debug",
  includeDebug = false,
): AgentTimelineItem[] {
  return deriveTimelineView(projection.domain, {
    eventDisplayLevel,
    includeDebug,
    activitiesById: projection.domain.activitiesById,
    eventsById: projection.eventsById,
    messagesById: projection.messagesById,
    transcriptEntriesById: projection.transcriptEntriesById,
    briefRecordsById: projection.briefRecordsById,
    briefHydrationById: projection.briefHydrationById,
  });
}

export function projectionEvents(projection: SessionProjectionState): ProjectionEvent[] {
  return projection.eventSeqs
    .map((eventSeq) => projection.eventsBySeq[eventSeq])
    .filter((event): event is ProjectionEvent => Boolean(event));
}

export function messageIdsForProjectionHydration(
  projection: SessionProjectionState,
): string[] {
  return unresolvedReferenceIds(
    projection.referencedMessageIds,
    projection.messagesById,
    projection.missingMessageIds,
  );
}

export function transcriptEntryIdsForProjectionHydration(
  projection: SessionProjectionState,
): string[] {
  return unresolvedReferenceIds(
    projection.referencedTranscriptEntryIds,
    projection.transcriptEntriesById,
    projection.missingTranscriptEntryIds,
  );
}

export function briefIdsForProjectionHydration(
  projection: SessionProjectionState,
): string[] {
  return unresolvedReferenceIds(
    projection.referencedBriefIds,
    projection.briefRecordsById,
    projection.missingBriefIds,
  );
}

export function eventIdentityConflicts(
  projection: SessionProjectionState,
  incomingEvents: ProjectionEvent[],
): boolean {
  return incomingEvents.some((event) => {
    if (event.event_seq == null) return false;
    const existing = projection.eventsBySeq[event.event_seq];
    return existing != null && eventIdentityFingerprint(existing) !== eventIdentityFingerprint(event);
  });
}

function reduceEvents(
  current: SessionProjectionState,
  incomingEvents: ProjectionEvent[],
  eventLogEpoch?: string,
): SessionProjectionState {
  const incomingEpoch = eventLogEpoch ?? lastEventEpoch(incomingEvents);
  const base =
    incomingEpoch &&
    (current.eventLogEpoch != null || current.eventSeqs.length > 0) &&
    current.eventLogEpoch !== incomingEpoch
      ? createSessionProjectionState(incomingEpoch)
      : current.eventLogEpoch || !incomingEpoch
        ? current
        : { ...current, eventLogEpoch: incomingEpoch };

  if (eventIdentityConflicts(base, incomingEvents)) {
    return reduceSessionProjection(base, {
      type: "reset",
      eventLogEpoch: incomingEpoch ?? base.eventLogEpoch,
      reason: "event_identity_conflict",
    });
  }

  const eventsBySeq = { ...base.eventsBySeq };
  const eventsById = { ...base.eventsById };
  const domain = cloneSessionState(base.domain);
  const references = {
    messageIds: { ...base.referencedMessageIds },
    transcriptEntryIds: { ...base.referencedTranscriptEntryIds },
    briefIds: { ...base.referencedBriefIds },
  };
  for (const event of [...incomingEvents].sort(compareEvents)) {
    if (event.event_seq == null) continue;
    if (incomingEpoch && event.event_log_epoch && event.event_log_epoch !== incomingEpoch) continue;
    const existing = eventsBySeq[event.event_seq];
    if (existing) {
      if (eventIdentityFingerprint(existing) !== eventIdentityFingerprint(event)) {
        return reduceSessionProjection(base, {
          type: "reset",
          eventLogEpoch: incomingEpoch ?? base.eventLogEpoch,
          reason: "event_identity_conflict",
        });
      }
      continue;
    }
    eventsBySeq[event.event_seq] = event;
    const eventId = event.id ?? `event-${event.event_seq}`;
    eventsById[eventId] = event;
    if (canApplySessionEvent(event)) {
      applySessionEvent(domain, event);
      indexEventReferences(references, event);
    }
  }
  const eventSeqs = Object.keys(eventsBySeq).map(Number).sort((left, right) => left - right);
  const events = eventSeqs.map((eventSeq) => eventsBySeq[eventSeq]);
  return {
    ...base,
    eventLogEpoch: incomingEpoch ?? base.eventLogEpoch,
    eventsBySeq,
    eventSeqs,
    eventsById,
    domain,
    referencedMessageIds: references.messageIds,
    referencedTranscriptEntryIds: references.transcriptEntryIds,
    referencedBriefIds: references.briefIds,
    unknownEventIds: events
      .filter((event) => !canApplySessionEvent(event))
      .map((event) => event.id ?? `event-${event.event_seq}`),
    gaps: eventGaps(events),
    newestSeq: eventSeqs[eventSeqs.length - 1],
    oldestSeq: eventSeqs[0],
    invalidatedReason: undefined,
  };
}

function rebuildProjection(projection: SessionProjectionState): SessionProjectionState {
  const events = projectionEvents(projection);
  const eventsById: Record<string, ProjectionEvent> = {};
  const applicableEvents: ProjectionEvent[] = [];
  const unknownEventIds: string[] = [];
  const references = {
    messageIds: {} as Record<string, true>,
    transcriptEntryIds: {} as Record<string, true>,
    briefIds: {} as Record<string, true>,
  };
  for (const event of events) {
    const eventId = event.id ?? `event-${event.event_seq}`;
    eventsById[eventId] = event;
    if (canApplySessionEvent(event)) {
      applicableEvents.push(event);
      indexEventReferences(references, event);
    } else {
      unknownEventIds.push(eventId);
    }
  }
  return {
    ...projection,
    eventSeqs: events.flatMap((event) => event.event_seq == null ? [] : [event.event_seq]),
    eventsById,
    domain: buildSessionState(applicableEvents),
    unknownEventIds,
    referencedMessageIds: references.messageIds,
    referencedTranscriptEntryIds: references.transcriptEntryIds,
    referencedBriefIds: references.briefIds,
    gaps: eventGaps(events),
    newestSeq: projection.newestSeq ?? events.at(-1)?.event_seq,
    oldestSeq: projection.oldestSeq ?? events[0]?.event_seq,
  };
}

function eventGaps(events: ProjectionEvent[]): EventGap[] {
  const gaps: EventGap[] = [];
  for (let index = 1; index < events.length; index += 1) {
    const previous = events[index - 1].event_seq;
    const current = events[index].event_seq;
    if (previous == null || current == null) continue;
    if (current > previous + 1) gaps.push({ afterSeq: previous, beforeSeq: current });
  }
  return gaps;
}

function mergeMissingIds(
  current: Record<string, true>,
  missingIds: string[],
  hydratedIds: string[],
): Record<string, true> {
  const next = { ...current };
  for (const id of missingIds) {
    if (id) next[id] = true;
  }
  for (const id of hydratedIds) delete next[id];
  return next;
}

function lastEventEpoch(events: ProjectionEvent[]): string | undefined {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    if (events[index].event_log_epoch) return events[index].event_log_epoch;
  }
  return undefined;
}

function eventIdentityFingerprint(event: ProjectionEvent): string {
  return JSON.stringify({
    id: event.id,
    ts: event.ts,
    type: event.type,
    contract_version: event.contract_version,
    payload_schema: event.payload_schema,
    payload_schema_version: event.payload_schema_version,
    provenance: event.provenance,
    payload: event.payload,
  });
}

function compareEvents(left: ProjectionEvent, right: ProjectionEvent): number {
  const leftSeq = left.event_seq;
  const rightSeq = right.event_seq;
  if (leftSeq != null && rightSeq != null && leftSeq !== rightSeq) return leftSeq - rightSeq;
  const leftTime = Date.parse(left.ts ?? "");
  const rightTime = Date.parse(right.ts ?? "");
  if (Number.isFinite(leftTime) && Number.isFinite(rightTime) && leftTime !== rightTime) {
    return leftTime - rightTime;
  }
  return 0;
}

function normalizeEventsBySeq(
  eventsBySeq: Record<number, ProjectionEvent>,
): Record<number, ProjectionEvent> {
  return Object.fromEntries(
    Object.entries(eventsBySeq).map(([seq, event]) => [
      Number(seq),
      event.event_seq == null ? { ...event, event_seq: Number(seq) } : event,
    ]),
  );
}

function indexEventReferences(
  references: {
    messageIds: Record<string, true>;
    transcriptEntryIds: Record<string, true>;
    briefIds: Record<string, true>;
  },
  event: ProjectionEvent,
): void {
  const payload = asRecord(event.payload);
  if (event.type === "message_enqueued") {
    const messageId = stringField(payload, "message_id");
    if (messageId) references.messageIds[messageId] = true;
  }
  const transcriptEntryId = transcriptEntryIdForPayload(payload);
  if (transcriptEntryId) references.transcriptEntryIds[transcriptEntryId] = true;
  if (event.type === "brief_created" && !stringField(payload, "text")) {
    const briefId = briefIdForPayload(payload);
    if (briefId) references.briefIds[briefId] = true;
  }
}

function unresolvedReferenceIds(
  referencedIds: Record<string, true>,
  hydratedById: Record<string, unknown>,
  missingIds: Record<string, true>,
): string[] {
  return Object.keys(referencedIds).filter(
    (id) => !hydratedById[id] && !missingIds[id],
  );
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function stringField(
  record: Record<string, unknown> | undefined,
  key: string,
): string | undefined {
  const value = record?.[key];
  return typeof value === "string" && value.trim() ? value : undefined;
}
