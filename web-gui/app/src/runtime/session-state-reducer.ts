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

export function cloneSessionState(state: SessionState): SessionState {
  return {
    messages: cloneObjectMap(state.messages),
    toolExecutions: cloneObjectMap(state.toolExecutions),
    tasks: cloneObjectMap(state.tasks),
    workItems: cloneObjectMap(state.workItems),
    rounds: cloneObjectMap(state.rounds),
    activitiesById: cloneObjectMap(state.activitiesById),
    insertionOrder: state.insertionOrder.map((entry) => ({ ...entry })),
  };
}

/**
 * Insert or update a domain object in the state.
 *
 * If an object with the same key already exists in its typed map, the two
 * are merged by canonical event order and sourceEventIds are accumulated.
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
 * Merge a new object into an existing one. The latest semantic event owns
 * lifecycle fields, while older events can still fill missing context.
 */
function mergeObjectFields(existing: DomainObject, incoming: DomainObject): void {
  const incomingIsNewer = compareObjectOrder(incoming, existing) >= 0;
  const newer = incomingIsNewer ? incoming : existing;
  const older = incomingIsNewer ? existing : incoming;
  const olderInitialStatus = "initialStatus" in older ? older.initialStatus : undefined;
  const olderRole = "role" in older ? older.role : undefined;
  const olderStatus = older.status;
  const olderPrimaryEventId = older.primaryEventId;
  const olderPrimaryEventSeq = older.primaryEventSeq;
  const activityIds = mergeActivityIds(existing, incoming);
  const sourceEventIds = Array.from(new Set([...existing.sourceEventIds, ...incoming.sourceEventIds]));
  const merged = mergeDefinedFields(older, newer);
  Object.assign(existing, merged, {
    id: existing.id,
    sourceEventIds,
    primaryEventId: newer.primaryEventId,
    primaryEventSeq: newer.primaryEventSeq,
    createdAt: compareObjectOrder(existing, incoming) <= 0 ? existing.createdAt : incoming.createdAt,
    updatedAt: newer.updatedAt,
  });
  if ("initialStatus" in existing && olderInitialStatus) {
    existing.initialStatus = olderInitialStatus;
  }
  if ("role" in existing && existing.role === "unknown" && olderRole && olderRole !== "unknown") {
    existing.role = olderRole;
  }
  if ("role" in existing && olderStatus === "enqueued") {
    existing.primaryEventId = olderPrimaryEventId;
    existing.primaryEventSeq = olderPrimaryEventSeq;
  }
  if ("objective" in existing && existing.status === "unknown" && olderStatus !== "unknown") {
    existing.status = olderStatus as WorkItemObject["status"];
  }
  if (activityIds.length) {
    (existing as { activityIds?: string[] }).activityIds = activityIds;
  }
}

function cloneObjectMap<T extends DomainObject>(map: Map<string, T>): Map<string, T> {
  return new Map(
    Array.from(map, ([id, object]) => [
      id,
      {
        ...object,
        sourceEventIds: [...object.sourceEventIds],
        ...("activityIds" in object && object.activityIds
          ? { activityIds: [...object.activityIds] }
          : {}),
      } as T,
    ]),
  );
}

function mergeDefinedFields(older: DomainObject, newer: DomainObject): DomainObject {
  const merged = { ...older } as Record<string, unknown>;
  for (const [key, value] of Object.entries(newer)) {
    if (value !== undefined) merged[key] = value;
  }
  return merged as unknown as DomainObject;
}

function compareObjectOrder(left: DomainObject, right: DomainObject): number {
  if (left.primaryEventSeq != null && right.primaryEventSeq != null) {
    if (left.primaryEventSeq !== right.primaryEventSeq) {
      return left.primaryEventSeq - right.primaryEventSeq;
    }
  } else if (left.primaryEventSeq != null) {
    return 1;
  } else if (right.primaryEventSeq != null) {
    return -1;
  }
  const timeOrder = sortableTime(left.updatedAt) - sortableTime(right.updatedAt);
  if (timeOrder !== 0) return timeOrder;
  return left.primaryEventId.localeCompare(right.primaryEventId);
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function mergeActivityIds(existing: DomainObject, incoming: DomainObject): string[] {
  const ids = [
    ...("activityIds" in existing ? (existing.activityIds ?? []) : []),
    ...("activityIds" in incoming ? (incoming.activityIds ?? []) : []),
  ];
  return Array.from(new Set(ids));
}
