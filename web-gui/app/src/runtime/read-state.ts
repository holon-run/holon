import type { StreamEventEnvelopeDto } from "./client";
import type { CachedAgentReadState } from "./idb-cache";
import { briefIdsForProjectionHydration } from "./session-projection";
import { canApplySessionEvent } from "./session-events";
import type { AgentSessionState } from "./runtime-store-helpers";

const ROSTER_ACTIVITY_STORAGE_KEY = "holon.webGui.rosterActivityByRemote.v1";

export interface AgentRosterActivity {
  operatorAt?: string;
  briefAt?: string;
  unreadCount?: number;
  lastUnreadDeliverySeq?: number;
  lastReadDeliverySeq?: number;
}

export interface ConversationReadContext {
  route: string;
  selectedAgentId: string;
  documentVisible: boolean;
  session: AgentSessionState | undefined;
}

export function readStoredRosterActivity(
  remoteKey: string,
  storage: Storage | undefined = typeof window === "undefined" ? undefined : window.localStorage,
): Record<string, AgentRosterActivity> {
  if (!storage) return {};
  try {
    const parsed = readStoredJson(storage, ROSTER_ACTIVITY_STORAGE_KEY);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const rawActivity = (parsed as Record<string, unknown>)[remoteKey];
    if (!rawActivity || typeof rawActivity !== "object" || Array.isArray(rawActivity)) return {};
    const activityByAgentId: Record<string, AgentRosterActivity> = {};
    for (const [agentId, value] of Object.entries(rawActivity)) {
      if (!agentId || !value || typeof value !== "object" || Array.isArray(value)) continue;
      const activity = coerceRosterActivity(value);
      if (activity) activityByAgentId[agentId] = activity;
    }
    return activityByAgentId;
  } catch {
    return {};
  }
}

export function writeStoredRosterActivity(
  remoteKey: string,
  activityByAgentId: Record<string, AgentRosterActivity>,
  storage: Storage | undefined = typeof window === "undefined" ? undefined : window.localStorage,
): void {
  if (!storage) return;
  try {
    const parsed = readStoredJson(storage, ROSTER_ACTIVITY_STORAGE_KEY);
    const byRemote =
      parsed && typeof parsed === "object" && !Array.isArray(parsed)
        ? (parsed as Record<string, Record<string, AgentRosterActivity>>)
        : {};
    byRemote[remoteKey] = activityByAgentId;
    storage.setItem(ROSTER_ACTIVITY_STORAGE_KEY, JSON.stringify(byRemote));
  } catch {
    // Ignore storage failures; unread state falls back to memory-only.
  }
}

export function mergeCachedReadState(
  current: AgentRosterActivity | undefined,
  cached: CachedAgentReadState | undefined,
): AgentRosterActivity | undefined {
  if (!cached) return current;
  const currentHasReadState =
    current?.unreadCount != null ||
    current?.lastUnreadDeliverySeq != null ||
    current?.lastReadDeliverySeq != null;
  if (currentHasReadState) return current;
  return {
    ...current,
    ...cached,
  };
}

export function mergeCachedReadStates(
  current: Record<string, AgentRosterActivity>,
  cached: Record<string, CachedAgentReadState>,
): Record<string, AgentRosterActivity> {
  let merged = current;
  for (const [agentId, readState] of Object.entries(cached)) {
    const activity = mergeCachedReadState(merged[agentId], readState);
    if (!activity || activity === merged[agentId]) continue;
    if (merged === current) merged = { ...current };
    merged[agentId] = activity;
  }
  return merged;
}

export function cachedReadState(
  activity: AgentRosterActivity | undefined,
): CachedAgentReadState | undefined {
  if (!activity) return undefined;
  const readState: CachedAgentReadState = {};
  if (activity.unreadCount != null) readState.unreadCount = activity.unreadCount;
  if (activity.lastUnreadDeliverySeq != null) {
    readState.lastUnreadDeliverySeq = activity.lastUnreadDeliverySeq;
  }
  if (activity.lastReadDeliverySeq != null) {
    readState.lastReadDeliverySeq = activity.lastReadDeliverySeq;
  }
  return Object.keys(readState).length ? readState : undefined;
}

export function touchRosterActivity(
  current: Record<string, AgentRosterActivity>,
  agentId: string,
  kind: "operator" | "brief",
  timestamp: string | undefined,
): Record<string, AgentRosterActivity> {
  if (!timestamp) return current;
  const existing = current[agentId];
  const field = kind === "operator" ? "operatorAt" : "briefAt";
  if (sortableTime(existing?.[field] ?? "") >= sortableTime(timestamp)) return current;
  return {
    ...current,
    [agentId]: {
      ...existing,
      [field]: timestamp,
    },
  };
}

export function markAgentDeliveriesRead(
  current: Record<string, AgentRosterActivity>,
  agentId: string,
  deliverySeq: number,
): Record<string, AgentRosterActivity> {
  const existing = current[agentId];
  const lastReadDeliverySeq = Math.max(
    deliverySeq,
    existing?.lastUnreadDeliverySeq ?? 0,
    existing?.lastReadDeliverySeq ?? 0,
  );
  if (!existing?.unreadCount && existing?.lastReadDeliverySeq === lastReadDeliverySeq) return current;
  return {
    ...current,
    [agentId]: {
      ...existing,
      unreadCount: 0,
      lastReadDeliverySeq,
    },
  };
}

export function touchRosterActivityFromEvent(
  current: Record<string, AgentRosterActivity>,
  agentId: string,
  event: StreamEventEnvelopeDto,
  _selectedAgentId: string,
): Record<string, AgentRosterActivity> {
  if (!canApplySessionEvent(event)) return current;
  let next = current;
  if (event.type === "brief_created") {
    next = touchRosterActivity(next, agentId, "brief", eventTimestamp(event));
  }
  if (event.type === "message_enqueued" && messageOrigin(event.payload) === "operator") {
    next = touchRosterActivity(next, agentId, "operator", eventTimestamp(event));
  }
  if (event.type === "brief_created") {
    next = incrementUnreadFromEvent(next, agentId, event);
  }
  return next;
}

export function latestBriefDeliverySeq(session: AgentSessionState): number | undefined {
  for (let index = session.eventSeqs.length - 1; index >= 0; index -= 1) {
    const seq = session.eventSeqs[index];
    if (session.eventsBySeq[seq]?.type === "brief_created") return seq;
  }
  return undefined;
}

export function canMarkConversationRead({
  route,
  selectedAgentId,
  documentVisible,
  session,
}: ConversationReadContext, agentId: string): boolean {
  return Boolean(
    route === "agent" &&
    selectedAgentId === agentId &&
    documentVisible &&
    session &&
    !session.loading &&
    session.gaps.length === 0 &&
    session.syncStatus !== "refreshing" &&
    session.syncStatus !== "recovering" &&
    session.syncStatus !== "stale" &&
    session.syncStatus !== "error" &&
    session.liveStatus !== "connecting" &&
    session.liveStatus !== "reconnecting" &&
    session.liveStatus !== "recovering" &&
    session.liveStatus !== "stale" &&
    session.liveStatus !== "error" &&
    briefIdsForProjectionHydration(session).length === 0
  );
}

function coerceRosterActivity(value: unknown): AgentRosterActivity | undefined {
  const parsed = value as Partial<AgentRosterActivity> & {
    lastUnreadSeq?: number;
    lastReadSeq?: number;
  };
  const activity: AgentRosterActivity = {};
  if (typeof parsed.operatorAt === "string") activity.operatorAt = parsed.operatorAt;
  if (typeof parsed.briefAt === "string") activity.briefAt = parsed.briefAt;
  if (typeof parsed.unreadCount === "number" && Number.isFinite(parsed.unreadCount) && parsed.unreadCount > 0) {
    activity.unreadCount = Math.floor(parsed.unreadCount);
  }
  const lastUnreadDeliverySeq = parsed.lastUnreadDeliverySeq ?? parsed.lastUnreadSeq;
  if (typeof lastUnreadDeliverySeq === "number" && Number.isFinite(lastUnreadDeliverySeq)) {
    activity.lastUnreadDeliverySeq = Math.floor(lastUnreadDeliverySeq);
  }
  const lastReadDeliverySeq = parsed.lastReadDeliverySeq ?? parsed.lastReadSeq;
  if (typeof lastReadDeliverySeq === "number" && Number.isFinite(lastReadDeliverySeq)) {
    activity.lastReadDeliverySeq = Math.floor(lastReadDeliverySeq);
  }
  return Object.keys(activity).length ? activity : undefined;
}

function incrementUnreadFromEvent(
  current: Record<string, AgentRosterActivity>,
  agentId: string,
  event: StreamEventEnvelopeDto,
): Record<string, AgentRosterActivity> {
  const existing = current[agentId];
  const seq = event.event_seq;
  if (seq != null && existing?.lastReadDeliverySeq != null && seq <= existing.lastReadDeliverySeq) return current;
  if (seq != null && existing?.lastUnreadDeliverySeq != null && seq <= existing.lastUnreadDeliverySeq) return current;
  return {
    ...current,
    [agentId]: {
      ...existing,
      unreadCount: (existing?.unreadCount ?? 0) + 1,
      lastUnreadDeliverySeq: seq ?? existing?.lastUnreadDeliverySeq,
    },
  };
}

function eventTimestamp(event: StreamEventEnvelopeDto): string | undefined {
  const payload = asRecord(event.payload);
  return stringField(payload, "created_at") ?? event.ts;
}

function messageOrigin(payload: unknown): string | undefined {
  const origin = asRecord(asRecord(payload)?.origin);
  return stringField(origin, "kind") ?? stringField(origin, "role") ?? stringField(asRecord(payload), "origin");
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : undefined;
}

function stringField(record: Record<string, unknown> | undefined, key: string): string | undefined {
  const value = record?.[key];
  return typeof value === "string" && value.trim() ? value : undefined;
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function readStoredJson(storage: Storage, key: string): unknown {
  const raw = storage.getItem(key);
  return raw ? JSON.parse(raw) : undefined;
}
