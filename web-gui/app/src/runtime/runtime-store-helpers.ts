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

export type { WorkItemDetailState, TaskDetailState, ToolExecutionDetailState };

export type AgentLiveStatus = "idle" | "connecting" | "streaming" | "reconnecting" | "recovering" | "stale" | "error";

export interface AgentSessionState {
  loading: boolean;
  loadingOlder: boolean;
  liveStatus: AgentLiveStatus;
  sendingPrompt: boolean;
  detail: AgentDetail | null;
  eventLogEpoch?: string;
  eventsBySeq: Record<number, unknown>;
  eventSeqs: number[];
  messagesById: Record<string, RuntimeMessageEnvelope>;
  missingMessageIds: Record<string, true>;
  transcriptEntriesById: Record<string, RuntimeTranscriptEntry>;
  missingTranscriptEntryIds: Record<string, true>;
  briefRecordsById: Record<string, RuntimeBriefRecord>;
  missingBriefIds: Record<string, true>;
  newestSeq?: number;
  oldestSeq?: number;
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
