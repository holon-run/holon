import type {
  AssistantRoundObject,
  DomainObject,
  InsertionEntry,
  MessageObject,
  RuntimeActivityObject,
  SessionObjectType,
  TaskObject,
  ToolExecutionObject,
  WorkItemObject,
} from "./session-object-types";
import {
  mergeSourceIds,
  sessionItemPriority,
} from "./timeline-display";
import type { AgentTimelineItem } from "./types";

/**
 * Normalized session state built by applying events incrementally.
 *
 * Objects are organized into typed maps by domain type. Each map is keyed
 * by the object identity (message_id, task_id, etc.). `insertionOrder`
 * preserves first-insertion order across all types for stable iteration.
 *
 * During Phase 3 Step 3a, each object carries a `viewDraft` that caches the
 * projection result. Step 3b will replace these drafts with proper
 * per-object renderers that derive the view from typed fields.
 */
export interface SessionState {
  messages: Map<string, MessageObject>;
  toolExecutions: Map<string, ToolExecutionObject>;
  tasks: Map<string, TaskObject>;
  workItems: Map<string, WorkItemObject>;
  rounds: Map<string, AssistantRoundObject>;
  activitiesById: Map<string, RuntimeActivityObject>;
  insertionOrder: InsertionEntry[];
}

/**
 * Create an empty {@link SessionState}.
 */
export function createSessionState(): SessionState {
  return {
    messages: new Map(),
    toolExecutions: new Map(),
    tasks: new Map(),
    workItems: new Map(),
    rounds: new Map(),
    activitiesById: new Map(),
    insertionOrder: [],
  };
}

/**
 * Insert or update a domain object in the state.
 *
 * If an object with the same key already exists in its typed map, the two
 * are merged using the same priority and sourceId logic as the previous
 * `mergeAgentTimelineItems` pass. The view draft from the higher-priority
 * projection wins; sourceEventIds are accumulated.
 */
export function upsertObject(
  state: SessionState,
  objectType: SessionObjectType,
  id: string,
  obj: DomainObject,
): void {
  const map = objectMap(state, objectType);
  const existing = map.get(id);
  if (!existing) {
    map.set(id, obj);
    state.insertionOrder.push({ objectType, id });
    return;
  }
  mergeObjectFields(existing, obj);
}

/**
 * Look up an object by type and id.
 */
export function getObject(state: SessionState, objectType: SessionObjectType, id: string): DomainObject | undefined {
  return objectMap(state, objectType).get(id);
}

function objectMap(state: SessionState, objectType: SessionObjectType): Map<string, DomainObject> {
  switch (objectType) {
    case "message": return state.messages as Map<string, DomainObject>;
    case "tool_execution": return state.toolExecutions as Map<string, DomainObject>;
    case "task": return state.tasks as Map<string, DomainObject>;
    case "work_item": return state.workItems as Map<string, DomainObject>;
    case "assistant_round": return state.rounds as Map<string, DomainObject>;
    case "activity": return state.activitiesById as Map<string, DomainObject>;
  }
}

/**
 * Merge a new object into an existing one. Display fields (viewDraft) from
 * the higher-priority projection win; sourceEventIds are accumulated in
 * insertion order; activities are merged.
 */
function mergeObjectFields(existing: DomainObject, incoming: DomainObject): void {
  const existingPriority = sessionItemPriority(draftToItem(existing));
  const incomingPriority = sessionItemPriority(draftToItem(incoming));

  const winner = incomingPriority >= existingPriority ? incoming : existing;
  const loser = incomingPriority >= existingPriority ? existing : incoming;

  Object.assign(existing, {
    status: winner.status,
    updatedAt: winner.updatedAt,
    viewDraft: { ...winner.viewDraft },
    sourceEventIds: mergeSourceIds([...loser.sourceEventIds, ...winner.sourceEventIds]),
  });
}

/**
 * Build a minimal AgentTimelineItem from a view draft for priority comparison.
 * Only the fields used by {@link sessionItemPriority} are needed.
 */
function draftToItem(obj: DomainObject): AgentTimelineItem {
  const draft = obj.viewDraft;
  return {
    id: obj.sourceEventIds[0] ?? obj.id,
    kind: draft.kind,
    label: draft.label,
    body: draft.body,
    timestamp: draft.timestamp,
    meta: draft.meta,
    minDisplayLevel: draft.minDisplayLevel,
    sourceIds: obj.sourceEventIds,
  };
}
