import i18next from "i18next";

import type { AgentTimelineActivity, AgentTimelineItem } from "../../runtime/types";

export interface TimelineTurn {
  id: string;
  label: string;
  kind: "operator" | "runtime";
  timestamp: string;
  items: AgentTimelineItem[];
}

export function collectWorkingActivitiesForCurrentTurn(timeline: AgentTimelineItem[]): AgentTimelineActivity[] {
  let currentTurnStart = -1;
  for (let index = timeline.length - 1; index >= 0; index -= 1) {
    if (timeline[index]?.kind === "operator" || isTurnStartedItem(timeline[index]!)) {
      currentTurnStart = index;
      break;
    }
  }
  return collectWorkingActivities(currentTurnStart >= 0 ? timeline.slice(currentTurnStart + 1) : timeline);
}

function collectWorkingActivities(timeline: AgentTimelineItem[]): AgentTimelineActivity[] {
  const byId = new Map<string, AgentTimelineActivity>();
  for (const item of timeline) {
    if (isLiveWorkingActivity(item)) {
      byId.set(item.id, timelineItemToWorkingActivity(item));
    }
    for (const activity of item.activities ?? []) {
      if (!isLiveWorkingActivity(activity)) continue;
      byId.set(activity.id, activity);
    }
  }
  const latestBySlot = new Map<"assistant" | "action", AgentTimelineActivity>();
  for (const activity of byId.values()) {
    const slot = activity.kind === "assistant" ? "assistant" : "action";
    const current = latestBySlot.get(slot);
    if (!current || sortableActivityTime(activity.timestamp) >= sortableActivityTime(current.timestamp)) {
      latestBySlot.set(slot, activity);
    }
  }
  return Array.from(latestBySlot.values()).sort(
    (left, right) => sortableActivityTime(left.timestamp) - sortableActivityTime(right.timestamp),
  );
}

function isLiveWorkingActivity(activity: Pick<AgentTimelineActivity, "label" | "meta" | "minDisplayLevel">): boolean {
  if (activity.minDisplayLevel === "info") return false;
  const eventType = activity.meta.split(" · ")[0];
  return (
    eventType === "assistant_round_recorded" ||
    eventType === "text_only_round_observed" ||
    eventType === "message_processing_started" ||
    eventType === "tool_executed" ||
    eventType === "tool_execution_failed"
  );
}

export function timelineItemToWorkingActivity(item: AgentTimelineItem): AgentTimelineActivity {
  return {
    id: item.id,
    kind: item.kind,
    label: item.label,
    body: item.body,
    timestamp: item.timestamp,
    meta: item.meta,
    minDisplayLevel: item.minDisplayLevel,
    sourceIds: item.sourceIds,
    stateObjectRef: item.stateObjectRef,
    relatedStateObjectRef: item.relatedStateObjectRef,
    detail: item.detail,
    rawEvent: item.rawEvent,
    debug: item.debug,
  };
}

export function groupTimelineTurns(timeline: AgentTimelineItem[]): TimelineTurn[] {
  const turns: TimelineTurn[] = [];
  let current: TimelineTurn | undefined;

  for (const item of timeline) {
    const isTurnBoundary = isTurnStartedItem(item);
    const isOperatorBoundary = item.kind === "operator";
    if (!current || isOperatorBoundary || isTurnBoundary) {
      const triggerLabel = isTurnBoundary ? item.body : undefined;
      current = {
        id: isOperatorBoundary || isTurnBoundary ? `turn:${item.id}` : `activity:${item.id}`,
        kind: isOperatorBoundary ? "operator" : "runtime",
        label: isOperatorBoundary
          ? i18next.t("agent.operatorTurn")
          : isTurnBoundary
            ? triggerLabel || i18next.t("agent.turn")
            : i18next.t("agent.runtimeActivity"),
        timestamp: item.timestamp,
        items: isTurnBoundary ? [] : [item],
      };
      turns.push(current);
      continue;
    }
    if (isTurnStartedItem(item)) continue;
    current.items.push(item);
  }

  const nonEmpty = turns.filter((turn) => turn.items.length > 0);
  return nonEmpty.length === turns.length ? turns : nonEmpty;
}

function isTurnStartedItem(item: AgentTimelineItem): boolean {
  return item.meta.startsWith("turn_started");
}

export function itemHasEventSeq(item: AgentTimelineItem, eventSeq: number | undefined): boolean {
  if (eventSeq == null) return false;
  if (rawEventSeq(item.rawEvent) === eventSeq) return true;
  return (item.activities ?? []).some((activity) => rawEventSeq(activity.rawEvent) === eventSeq);
}

function rawEventSeq(rawEvent: unknown): number | undefined {
  return typeof rawEvent === "object" && rawEvent !== null && "event_seq" in rawEvent && typeof rawEvent.event_seq === "number"
    ? rawEvent.event_seq
    : undefined;
}

export function sortableActivityTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}

