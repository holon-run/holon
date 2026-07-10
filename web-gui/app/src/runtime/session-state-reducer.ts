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
import { mergeSourceIds } from "./timeline-display";

/**
 * Normalized session state built by applying events incrementally.
 *
 * Objects are organized into typed maps by domain type. Each map is keyed
 * by the object identity (message_id, task_id, etc.). `insertionOrder`
 * preserves first-insertion order across all types for stable iteration.
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
 * are merged using priority logic: the winning event's render data and
 * domain fields are kept; sourceEventIds are accumulated.
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
 * Merge a new object into an existing one. The higher-priority event's render
 * data and domain fields win; sourceEventIds are accumulated in insertion order.
 *
 * Priority mirrors the original `timelineItemPriority` logic:
 * - `operator-prompt:pending:*` ids or `pending-operator-prompt` sourceIds → 0
 * - fallback `event-*` ids or meta containing `event #` → 1
 * - everything else → 2
 */
function mergeObjectFields(existing: DomainObject, incoming: DomainObject): void {
  const existingPriority = objectPriority(existing);
  const incomingPriority = objectPriority(incoming);

  const winner = incomingPriority >= existingPriority ? incoming : existing;
  const loser = incomingPriority >= existingPriority ? existing : incoming;
  const activityIds = mergeActivityIds(existing, incoming);

  // Copy all render and domain fields from the winner, preserving
  // earliest createdAt and accumulating sourceEventIds.
  const { id: _id, createdAt: _createdAt, sourceEventIds: _sourceIds, ...winnerFields } = winner;
  Object.assign(existing, winnerFields, {
    sourceEventIds: mergeSourceIds([...loser.sourceEventIds, ...winner.sourceEventIds]),
  });
  if (activityIds.length) {
    (existing as { activityIds?: string[] }).activityIds = activityIds;
  }
}

function mergeActivityIds(existing: DomainObject, incoming: DomainObject): string[] {
  const ids = [
    ...("activityIds" in existing ? (existing.activityIds ?? []) : []),
    ...("activityIds" in incoming ? (incoming.activityIds ?? []) : []),
  ];
  return Array.from(new Set(ids));
}

function objectPriority(obj: DomainObject): number {
  const eventId = obj.render.eventId;
  if (eventId.startsWith("operator-prompt:pending:")) return 0;
  if (obj.sourceEventIds.includes("pending-operator-prompt")) return 0;
  if (eventId.startsWith("event-") || obj.render.meta.includes("event #")) return 1;
  return 2;
}
