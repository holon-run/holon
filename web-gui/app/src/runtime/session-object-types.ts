/**
 * Domain object types for the normalized {@link SessionState}.
 *
 * Each domain object represents a typed entity in the session (message, tool
 * execution, task, work item, assistant round, or runtime activity) with an
 * explicit status field and lifecycle semantics.
 *
 * During the Phase 3 migration, each object also carries a `_viewDraft` that
 * caches the projection result so `deriveTimelineView` can produce identical
 * output. Step 3b will replace this draft with proper per-object renderers
 * that derive the view from typed fields.
 */
import type {
  AgentTimelineItemDetail,
  AgentTimelineItemKind,
  DisplayLevel,
} from "./types";

export type SessionObjectType =
  | "message"
  | "tool_execution"
  | "task"
  | "work_item"
  | "assistant_round"
  | "activity";

/**
 * Cached projection of an event into display-ready fields.
 *
 * Step 3a uses this to bridge the new domain-object state with the existing
 * projection logic. Step 3b will remove it in favour of per-object renderers.
 */
export interface ViewDraft {
  kind: AgentTimelineItemKind;
  label: string;
  body: string;
  timestamp: string;
  meta: string;
  minDisplayLevel: DisplayLevel;
  detail?: AgentTimelineItemDetail;
  rawEvent?: unknown;
  debug?: string;
}

interface BaseObject {
  id: string;
  status: string;
  sourceEventIds: string[];
  createdAt: string;
  updatedAt: string;
  viewDraft: ViewDraft;
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
}

export type AssistantRoundStatus = "recorded" | "brief_promoted";

export interface AssistantRoundObject extends BaseObject {
  status: AssistantRoundStatus;
}

export interface RuntimeActivityObject {
  id: string;
  eventType: string;
  sourceEventIds: string[];
  createdAt: string;
  updatedAt: string;
  status: string;
  viewDraft: ViewDraft;
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
