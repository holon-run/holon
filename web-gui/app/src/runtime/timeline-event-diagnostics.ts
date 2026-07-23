import { canApplySessionEvent, type SessionEventEnvelope } from "./session-events";
import type { SessionProjectionState } from "./session-projection";
import type { AgentTimelineActivity, AgentTimelineItem } from "./types";

export type TimelineEventFamily =
  | "message"
  | "assistant"
  | "tool"
  | "task"
  | "work_item"
  | "scheduler"
  | "provider"
  | "other";

export type TimelineEventDisposition = "shown" | "merged" | "hidden" | "rejected" | "unhandled";

export interface TimelineEventDiagnostic {
  disposition: TimelineEventDisposition;
  reason: string;
  timelineItemIds: string[];
}

export function diagnoseTimelineEvent(
  event: SessionEventEnvelope,
  projection: SessionProjectionState | undefined,
  timeline: AgentTimelineItem[],
): TimelineEventDiagnostic {
  const eventId = event.id ?? (event.event_seq == null ? undefined : `event-${event.event_seq}`);
  if (!canApplySessionEvent(event)) {
    return { disposition: "rejected", reason: "Event contract or payload schema is not supported.", timelineItemIds: [] };
  }
  if (!eventId || event.event_seq == null || !projection?.eventsBySeq[event.event_seq]) {
    return { disposition: "unhandled", reason: "Event is outside the currently loaded semantic projection.", timelineItemIds: [] };
  }
  const matches = timeline.filter((item) => timelineEntryIncludesSource(item, eventId));
  if (matches.length === 0) {
    return { disposition: "hidden", reason: "Event was accepted but produced no visible semantic timeline item.", timelineItemIds: [] };
  }
  const merged = matches.some((item) => timelineEntrySourceIds(item).length > 1);
  return {
    disposition: merged ? "merged" : "shown",
    reason: merged
      ? "Event contributes to a semantic item with other source events."
      : "Event is represented by a semantic timeline item.",
    timelineItemIds: matches.map((item) => item.id),
  };
}

export function filterTimelineEvents(
  events: SessionEventEnvelope[],
  diagnostics: Map<number | undefined, TimelineEventDiagnostic>,
  filter: { query: string; family: TimelineEventFamily | "all"; status: TimelineEventDisposition | "all" },
): SessionEventEnvelope[] {
  const query = filter.query.trim().toLocaleLowerCase();
  return events.filter((event) => {
    if (filter.family !== "all" && timelineEventFamily(event.type) !== filter.family) return false;
    if (filter.status !== "all" && diagnostics.get(event.event_seq)?.disposition !== filter.status) return false;
    if (!query) return true;
    return timelineEventSearchText(event).includes(query);
  });
}

export function timelineEventFamily(eventType: string | undefined): TimelineEventFamily {
  const type = eventType ?? "";
  if (type.startsWith("message_")) return "message";
  if (type.startsWith("assistant_") || type.startsWith("brief_") || type.startsWith("turn_")) return "assistant";
  if (type.startsWith("tool_")) return "tool";
  if (type.startsWith("task_")) return "task";
  if (type.startsWith("work_item_") || type.startsWith("workitem_")) return "work_item";
  if (type.startsWith("provider_")) return "provider";
  if (type.startsWith("scheduler_") || type.startsWith("continuation_") || type.startsWith("wait_") || type.startsWith("agent_")) return "scheduler";
  return "other";
}

export function timelineEventGapCount(events: SessionEventEnvelope[]): number {
  const seqs = events.flatMap((event) => event.event_seq == null ? [] : [event.event_seq]).sort((left, right) => left - right);
  return seqs.slice(1).filter((seq, index) => seq > seqs[index] + 1).length;
}

export function timelineEventsFilename(agentId: string, exportedAt = new Date()): string {
  const timestamp = exportedAt.toISOString().replaceAll(":", "-");
  return `holon-timeline-events-${agentId.replaceAll(/[^a-zA-Z0-9._-]/g, "_")}-${timestamp}.json`;
}

export function buildTimelineEventsBundle(input: {
  agentId: string;
  eventLogEpoch?: string;
  oldestSeq?: number;
  newestSeq?: number;
  hasOlder: boolean;
  events: SessionEventEnvelope[];
  diagnostics: Map<number | undefined, TimelineEventDiagnostic>;
  filters: { query: string; family: TimelineEventFamily | "all"; status: TimelineEventDisposition | "all" };
  includeRawPayload: boolean;
  exportedAt: string;
  guiVersion: string;
}) {
  return {
    schema: "holon.web_gui.timeline_events",
    version: 1,
    exportedAt: input.exportedAt,
    guiVersion: input.guiVersion,
    agentId: input.agentId,
    eventLogEpoch: input.eventLogEpoch,
    loadedRange: { oldestSeq: input.oldestSeq, newestSeq: input.newestSeq },
    hasOlder: input.hasOlder,
    filters: input.filters,
    rawPayloadIncluded: input.includeRawPayload,
    events: input.events.map((event) => ({
      ...(input.includeRawPayload ? event : redactTimelineEventPayload(event)),
      diagnostic: input.diagnostics.get(event.event_seq),
    })),
  };
}

export function redactTimelineEventPayload(event: SessionEventEnvelope): SessionEventEnvelope {
  return { ...event, payload: { redacted: true } };
}

function timelineEntryIncludesSource(item: AgentTimelineItem, sourceId: string): boolean {
  return item.sourceIds.includes(sourceId)
    || (item.activities ?? []).some((activity) => activity.sourceIds.includes(sourceId));
}

function timelineEntrySourceIds(item: AgentTimelineItem): string[] {
  return Array.from(new Set([
    ...item.sourceIds,
    ...(item.activities ?? []).flatMap((activity: AgentTimelineActivity) => activity.sourceIds),
  ]));
}

function timelineEventSearchText(event: SessionEventEnvelope): string {
  return [
    event.event_seq,
    event.id,
    event.type,
    event.payload_schema,
    summarizeTimelineEvent(event),
    JSON.stringify(event.provenance ?? {}),
  ].join(" ").toLocaleLowerCase();
}

function summarizeTimelineEvent(event: SessionEventEnvelope): string {
  const payload = event.payload && typeof event.payload === "object" && !Array.isArray(event.payload)
    ? event.payload as Record<string, unknown>
    : undefined;
  const summary = ["summary", "message", "status", "objective", "tool_name", "task_id", "work_item_id"]
    .map((key) => payload?.[key])
    .find((value) => typeof value === "string" && value.trim());
  return typeof summary === "string" ? summary : event.ts ?? "";
}
