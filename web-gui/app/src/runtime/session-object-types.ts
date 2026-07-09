/**
 * Domain object types for the normalized {@link SessionState}.
 *
 * Each domain object represents a typed entity in the session (message, tool
 * execution, task, work item, assistant round, or runtime activity) with an
 * explicit status field and lifecycle semantics.
 *
 * Each object also carries render data from the winning event. The renderer
 * layer (`deriveTimelineView`) uses this to produce display-ready items,
 * keeping domain state separate from display formatting.
 */

export type SessionObjectType =
  | "message"
  | "tool_execution"
  | "task"
  | "work_item"
  | "assistant_round"
  | "activity";

/**
 * Rendering data carried by each domain object from the winning event.
 * The renderer reads these fields to produce display output.
 */
export interface RenderData {
  eventType: string;
  payload: Record<string, unknown> | undefined;
  timestamp: string;
  eventId: string;
  eventSeq: number | undefined;
  meta: string;
  debug?: string;
  rawEvent?: unknown;
}

interface BaseObject {
  id: string;
  status: string;
  sourceEventIds: string[];
  createdAt: string;
  updatedAt: string;
  render: RenderData;
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
}

export type WorkItemStatus =
  | "created"
  | "picked"
  | "yielded"
  | "blocked"
  | "completed"
  | "focus_released"
  | "report_promoted";

export interface WorkItemObject extends BaseObject {
  status: WorkItemStatus;
  objective?: string;
  state?: string;
}

export type AssistantRoundStatus = "recorded" | "brief_promoted";

export interface AssistantRoundObject extends BaseObject {
  status: AssistantRoundStatus;
}

export interface RuntimeActivityObject extends BaseObject {
  eventType: string;
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
