import type { StreamEventEnvelopeDto } from "./client";
import { canApplySessionEvent } from "./session-events";

const ROSTER_ACTIVITY_STORAGE_KEY = "holon.webGui.rosterActivityByRemote.v1";

export interface AgentRosterActivity {
  operatorAt?: string;
  briefAt?: string;
}

export interface ConversationReadContext {
  route: string;
  selectedAgentId: string;
  documentVisible: boolean;
  /** The conversation read-model scope has a loaded view for this agent. */
  conversationReady: boolean;
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
  return next;
}

function coerceRosterActivity(value: unknown): AgentRosterActivity | undefined {
  const parsed = value as Partial<AgentRosterActivity>;
  const activity: AgentRosterActivity = {};
  if (typeof parsed.operatorAt === "string") activity.operatorAt = parsed.operatorAt;
  if (typeof parsed.briefAt === "string") activity.briefAt = parsed.briefAt;
  return Object.keys(activity).length ? activity : undefined;
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
