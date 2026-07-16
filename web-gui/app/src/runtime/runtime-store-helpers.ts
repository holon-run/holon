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

export type { WorkItemDetailState, TaskDetailState, ToolExecutionDetailState };

export type AgentLiveStatus = "idle" | "connecting" | "streaming" | "reconnecting" | "recovering" | "stale" | "error";

export interface AgentSessionState extends SessionProjectionState {
  loading: boolean;
  loadingOlder: boolean;
  liveStatus: AgentLiveStatus;
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
