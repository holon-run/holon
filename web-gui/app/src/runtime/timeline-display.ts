import type { AgentTimelineActivity, AgentTimelineItem, DisplayLevel } from "./types";

/**
 * Semantic dedup key for a timeline item. Items with the same key are merged.
 * Exported so session-state-reducer can apply the same identity logic incrementally.
 */
export function sessionObjectKey(item: AgentTimelineItem): string {
  return timelineDedupeKey(item);
}

/**
 * Merge priority for a timeline item. Higher priority wins during dedup.
 * Exported so session-state-reducer can apply the same priority logic incrementally.
 */
export function sessionItemPriority(item: AgentTimelineItem): number {
  return timelineItemPriority(item);
}

/**
 * Merge two timeline items into one. `preferred` provides the display fields;
 * sourceIds and activities from both are combined.
 * Exported so session-state-reducer can apply the same merge semantics incrementally.
 */
export function mergeTimelineItemFields(
  preferred: AgentTimelineItem,
  fallback: AgentTimelineItem,
): AgentTimelineItem {
  return mergeTimelineItemActivities(preferred, fallback);
}

const displayLevelRank: Record<DisplayLevel, number> = {
  info: 0,
  verbose: 1,
  debug: 2,
};
const maxTimelineSourceIds = 200;

export function mergeAgentTimelineItems(
  existing: Array<AgentTimelineItem | undefined>,
  incoming: Array<AgentTimelineItem | undefined>,
): AgentTimelineItem[] {
  const bySemanticKey = new Map<string, AgentTimelineItem>();

  for (const candidate of [...existing, ...incoming]) {
    if (!candidate) continue;
    const key = timelineDedupeKey(candidate);
    const current = bySemanticKey.get(key);
    if (!current || timelineItemPriority(candidate) >= timelineItemPriority(current)) {
      bySemanticKey.set(key, current ? mergeTimelineItemActivities(current, candidate) : candidate);
    } else if (current) {
      bySemanticKey.set(key, mergeTimelineItemActivities(candidate, current));
    }
  }

  return Array.from(bySemanticKey.values());
}

export function filterTimelineByDisplayLevel(
  items: AgentTimelineItem[],
  displayLevel: DisplayLevel,
  options: { itemLimit?: number } = {},
): AgentTimelineItem[] {
  const rank = displayLevelRank[displayLevel];
  const filtered = items
    .map((item) => ({
      ...item,
      activities:
        displayLevel === "info"
          ? (item.activities ?? []).filter(isInfoActivity)
          : (item.activities ?? []).filter((activity) => displayLevelRank[activity.minDisplayLevel] <= rank),
    }))
    .filter((item) => displayLevelRank[item.minDisplayLevel] <= rank || Boolean(item.activities?.length));
  const limit = options.itemLimit ?? (displayLevel === "info" ? 12 : 160);
  return filtered.slice(-limit);
}

export function compactAgentTimelineItems(items: AgentTimelineItem[]): AgentTimelineItem[] {
  const flattened = flattenTimelineActivities(items);
  const finalBriefTexts = flattened.filter(isFinalBriefItem).map((item) => normalizeAssistantBriefText(item.body));
  const deduped = flattened.filter((candidate) => !isAssistantPreviewDuplicate(candidate, finalBriefTexts));
  return deduped.sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
}

function timelineDedupeKey(item: AgentTimelineItem): string {
  if (item.kind === "operator") {
    return `operator:${item.sourceIds[0] ?? item.id}`;
  }
  if (item.kind === "assistant") {
    return `assistant:${item.id}`;
  }
  return `item:${item.id}`;
}

function timelineItemPriority(item: AgentTimelineItem): number {
  if (item.id.startsWith("operator-prompt:pending:")) return 0;
  if (item.sourceIds.includes("pending-operator-prompt")) return 0;
  if (item.id.startsWith("event-") || item.meta.includes("event #")) return 1;
  return 2;
}

function isAssistantPreviewDuplicate(candidate: AgentTimelineItem, finalBriefTexts: string[]): boolean {
  if (candidate.kind !== "assistant" || candidate.label !== "Assistant round") return false;
  const candidateText = normalizeAssistantBriefText(candidate.body);
  if (!candidateText) return false;

  return finalBriefTexts.some((briefText) => isSameAssistantBriefText(candidateText, briefText));
}

function isFinalBriefItem(item: AgentTimelineItem): boolean {
  return item.kind === "assistant" && item.minDisplayLevel === "info" && (item.label === "Result" || item.label === "Brief Created");
}

function isSameAssistantBriefText(previewText: string, briefText: string): boolean {
  if (!previewText || !briefText) return false;
  if (previewText === briefText) return true;
  return briefText.startsWith(previewText);
}

function normalizeAssistantBriefText(text: string): string {
  return normalizeTextKey(text).replace(/(?:\s*(?:\.{3}|…))+$/u, "").trim();
}

function flattenTimelineActivities(items: AgentTimelineItem[]): AgentTimelineItem[] {
  return items.flatMap((item) => {
    const base: AgentTimelineItem = { ...item, activities: undefined };
    const activities = (item.activities ?? []).filter(shouldFlattenActivity).map(activityToTimelineItem);
    return [base, ...activities];
  });
}

function shouldFlattenActivity(activity: AgentTimelineActivity): boolean {
  if (isEphemeralRuntimeActivity(activity)) return false;
  return true;
}

function isEphemeralRuntimeActivity(activity: Pick<AgentTimelineActivity, "kind" | "meta">): boolean {
  if (activity.kind !== "tool" && activity.kind !== "event" && activity.kind !== "system") return false;
  return (
    activity.meta.startsWith("tool_executed") ||
    activity.meta.startsWith("tool_execution_failed") ||
    activity.meta.startsWith("wait_condition_registered") ||
    activity.meta.startsWith("agent_waiting")
  );
}

function activityToTimelineItem(activity: AgentTimelineActivity): AgentTimelineItem {
  return {
    id: activity.id,
    kind: activity.kind,
    label: activity.label,
    body: activity.body,
    timestamp: activity.timestamp,
    meta: activity.meta,
    minDisplayLevel: activity.minDisplayLevel,
    sourceIds: activity.sourceIds,
    detail: activity.detail,
    rawEvent: activity.rawEvent,
    debug: activity.debug,
  };
}

function isInfoActivity(activity: AgentTimelineActivity): boolean {
  if (activity.kind !== "tool") return false;
  return true;
}

function mergeTimelineItemActivities(preferred: AgentTimelineItem, fallback: AgentTimelineItem): AgentTimelineItem {
  return {
    ...preferred,
    sourceIds: mergeSourceIds([...fallback.sourceIds, ...preferred.sourceIds]),
    activities: mergeTimelineActivities(fallback.activities ?? [], preferred.activities ?? []),
  };
}

function mergeTimelineActivities(
  existing: AgentTimelineActivity[],
  incoming: AgentTimelineActivity[],
): AgentTimelineActivity[] {
  const byId = new Map<string, AgentTimelineActivity>();
  for (const activity of [...existing, ...incoming]) {
    byId.set(activity.id, activity);
  }
  return Array.from(byId.values()).sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
}

export function mergeSourceIds(sourceIds: string[]): string[] {
  return Array.from(new Set(sourceIds)).slice(0, maxTimelineSourceIds);
}

function normalizeTextKey(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}
