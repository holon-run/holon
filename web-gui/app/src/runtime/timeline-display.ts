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
  const sorted = deduped.sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
  return mergeConsecutiveStateObjectActivities(sorted);
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
    relatedStateObjectRef: activity.relatedStateObjectRef,
    detail: activity.detail,
    executionMeta: activity.executionMeta,
    statusTrail: activity.statusTrail,
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

/**
 * Merge consecutive state-object lifecycle items from the same object into a single
 * evolving entry. Items are only merged when they are adjacent in the sorted
 * timeline (no non-matching items between them), preserving time-order context.
 *
 * State-object lifecycle items are identified by:
 * - `stateObjectRef.kind` in mergeable kinds (the card from the creation event)
 * - `relatedStateObjectRef.kind` in mergeable kinds (flattened lifecycle activities)
 *
 * The merged item keeps the earliest timestamp, the latest label/body, and
 * accumulates semantic status steps into `statusTrail`.
 */
function mergeConsecutiveStateObjectActivities(items: AgentTimelineItem[]): AgentTimelineItem[] {
  if (!items.length) return items;
  const result: AgentTimelineItem[] = [];
  let i = 0;
  while (i < items.length) {
    const refId = stateObjectRefId(items[i]);
    if (!refId) {
      result.push(items[i]);
      i++;
      continue;
    }
    // Collect consecutive items from the same state object
    const group: AgentTimelineItem[] = [items[i]];
    let j = i + 1;
    while (j < items.length && stateObjectRefId(items[j]) === refId) {
      group.push(items[j]);
      j++;
    }
    if (group.length === 1) {
      result.push(group[0]);
    } else {
      result.push(mergeStateObjectGroup(group));
    }
    i = j;
  }
  return result;
}

/**
 * Kinds whose consecutive lifecycle activities should be merged.
 */
const mergeableStateObjectKinds = new Set(["task", "work_item"]);

/**
 * Extract the state object id if the item is a lifecycle item for a mergeable kind.
 * Returns undefined for non-mergeable items.
 */
function stateObjectRefId(item: AgentTimelineItem): string | undefined {
  if (item.stateObjectRef && mergeableStateObjectKinds.has(item.stateObjectRef.kind)) return item.stateObjectRef.id;
  // A command promoted to a background task is the launch event for that task.
  // Treat only that promoted tool execution as part of the task lifecycle so it
  // does not split the task card from its queued/running/completed updates.
  if (
    item.stateObjectRef?.kind === "tool_execution"
    && item.relatedStateObjectRef?.kind === "task"
    && item.executionMeta?.outcome === "promoted"
  ) {
    return item.relatedStateObjectRef.id;
  }
  // Only match flattened activities (no own stateObjectRef) to avoid merging tool executions
  // that have relatedStateObjectRef pointing to a state object but are separate objects.
  if (!item.stateObjectRef && item.relatedStateObjectRef && mergeableStateObjectKinds.has(item.relatedStateObjectRef.kind)) return item.relatedStateObjectRef.id;
  return undefined;
}

/**
 * Merge a group of consecutive state-object lifecycle items into one entry.
 * - id: prefer the card's id (stateObjectRef) for stable React keys
 * - timestamp: earliest (group start)
 * - label/body/detail: from the last item (latest state)
 * - statusTrail: accumulated semantic statuses from all items
 * - stateObjectRef: from the card if present
 * - sourceIds: merged from all
 */
function mergeStateObjectGroup(group: AgentTimelineItem[]): AgentTimelineItem {
  const first = group[0];
  const last = group[group.length - 1];
  const card = group.find((item) => item.stateObjectRef && mergeableStateObjectKinds.has(item.stateObjectRef.kind));

  const statusTrail: NonNullable<AgentTimelineItem["statusTrail"]> = [];
  for (const item of group) {
    for (const step of item.statusTrail ?? []) {
      if (!statusTrail.some((existing) => existing.status === step.status)) {
        statusTrail.push(step);
      }
    }
  }

  return {
    id: card?.id ?? first.id,
    kind: card?.kind ?? last.kind,
    label: last.label,
    body: last.body,
    timestamp: first.timestamp,
    meta: last.meta,
    minDisplayLevel: group.reduce(
      (min, item) => (displayLevelRank[item.minDisplayLevel] < displayLevelRank[min.minDisplayLevel] ? item : min),
      first,
    ).minDisplayLevel,
    sourceIds: mergeSourceIds(group.flatMap((item) => item.sourceIds)),
    stateObjectRef: card?.stateObjectRef ?? last.relatedStateObjectRef,
    relatedStateObjectRef: card ? undefined : last.relatedStateObjectRef,
    detail: last.detail,
    executionMeta: last.executionMeta,
    statusTrail: statusTrail.length ? statusTrail : undefined,
    activities: undefined,
    rawEvent: last.rawEvent,
    debug: last.debug,
  };
}
