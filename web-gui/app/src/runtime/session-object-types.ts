/**
 * Domain object types for the normalized {@link SessionState}.
 *
 * Each domain object represents a typed entity in the session (message, tool
 * execution, task, work item, assistant round, or runtime activity) with an
 * explicit status field and lifecycle semantics.
 *
 * Domain objects retain only event references and lifecycle fields. The
 * renderer resolves the referenced canonical event from SessionProjectionState
 * when deriving display-ready items.
 */

import type { TimelineStateObjectRef } from "./types";

export type SessionObjectType =
  | "message"
  | "tool_execution"
  | "task"
  | "work_item"
  | "assistant_round"
  | "activity";

interface BaseObject {
  id: string;
  status: string;
  sourceEventIds: string[];
  primaryEventId: string;
  primaryEventSeq?: number;
  createdAt: string;
  updatedAt: string;
}

export type MessageStatus = "enqueued" | "processing" | "delivered";

export interface MessageObject extends BaseObject {
  status: MessageStatus;
  role: "operator" | "assistant" | "system" | "unknown";
}

export type ToolExecutionStatus = "completed" | "failed" | "promoted";

export interface ToolExecutionObject extends BaseObject {
  status: ToolExecutionStatus;
  toolName: string;
  taskId?: string;
  relatedStateObjectRef?: TimelineStateObjectRef;
}

export type TaskStatus =
  | "created"
  | "running"
  | "cancelling"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted";

export interface TaskObject extends BaseObject {
  status: TaskStatus;
  summary?: string;
  activityIds?: string[];
  initialStatus?: TaskStatus;
}

export type WorkItemStatus =
  | "unknown"
  | "open"
  | "runnable"
  | "queued"
  | "yielded"
  | "blocked"
  | "waiting_for_operator"
  | "draft"
  | "ready"
  | "needs_input"
  | "completed"
  | "cancelled";

export interface WorkItemObject extends BaseObject {
  status: WorkItemStatus;
  objective?: string;
  state?: string;
  activityIds?: string[];
}

export type AssistantRoundStatus = "recorded" | "brief_promoted";

export interface AssistantRoundObject extends BaseObject {
  status: AssistantRoundStatus;
}

export interface RuntimeActivityObject extends BaseObject {
  eventType: string;
  relatedStateObjectRef?: TimelineStateObjectRef;
}

/**
 * Ordered entry in `insertionOrder`, pointing to an object in the
 * corresponding typed map.
 */
export interface InsertionEntry {
  objectType: SessionObjectType;
  id: string;
}

export type DomainObject =
  | MessageObject
  | ToolExecutionObject
  | TaskObject
  | WorkItemObject
  | AssistantRoundObject
  | RuntimeActivityObject;
