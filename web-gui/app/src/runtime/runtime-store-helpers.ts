/**
 * Shared type definitions extracted from runtime-store.ts to avoid
 * circular imports when other modules (like session-cache) need
 * AgentSessionState.
 */

import type {
  AgentDetail,
  RuntimeBriefRecord,
  RuntimeMessageEnvelope,
  RuntimeTranscriptEntry,
  WorkItemDetailState,
  TaskDetailState,
  ToolExecutionDetailState,
} from "./types";
import type { SessionProjectionState } from "./session-projection";
import type { SessionEventEnvelope } from "./session-events";

export type { WorkItemDetailState, TaskDetailState, ToolExecutionDetailState };

export type AgentLiveStatus = "idle" | "connecting" | "streaming" | "reconnecting" | "recovering" | "stale" | "error";
export type AgentCacheStatus = "unchecked" | "loading" | "hit" | "miss" | "unavailable";
export type AgentContentStatus = "unknown" | "available" | "confirmed-empty";
export type AgentSyncStatus = "idle" | "refreshing" | "streaming" | "recovering" | "stale" | "error";

export interface TimelineEventsState {
  eventLogEpoch?: string;
  eventsBySeq: Record<number, SessionEventEnvelope>;
  eventSeqs: number[];
  oldestSeq?: number;
  newestSeq?: number;
  hasOlder: boolean;
  loading: boolean;
  loadingOlder: boolean;
  error?: string;
}

export interface AgentSessionState extends SessionProjectionState {
  loading: boolean;
  loadingOlder: boolean;
  liveStatus: AgentLiveStatus;
  cacheStatus: AgentCacheStatus;
  contentStatus: AgentContentStatus;
  syncStatus: AgentSyncStatus;
  lastValidatedAt?: number;
  detailValidatedAt?: number;
  eventsValidatedAt?: number;
  sendingPrompt: boolean;
  detail: AgentDetail | null;
  hasOlder?: boolean;
  targetEventSeq?: number;
  lastStreamActivityAt?: string;
  reconnectAttempt?: number;
  error?: string;
  historyError?: string;
  promptError?: string;
  modelError?: string;
  workItemDetailsById: Record<string, WorkItemDetailState>;
  taskDetailsById: Record<string, TaskDetailState>;
  toolExecutionDetailsById: Record<string, ToolExecutionDetailState>;
}
