import type { AgentTimelineItem, AgentTimelineItemKind, DisplayLevel } from "./types";

export interface SessionTranscriptEntry {
  id?: string;
  created_at?: string;
  kind?: string;
  round?: number | null;
  stop_reason?: string | null;
  input_tokens?: number | null;
  output_tokens?: number | null;
  data?: unknown;
}

export interface SessionBriefRecord {
  id?: string;
  created_at?: string;
  text?: string;
  kind?: string;
}

export interface SessionEventEnvelope {
  id?: string;
  event_seq?: number;
  ts?: string;
  type?: string;
  payload?: unknown;
}

export interface ReduceAgentSessionInput {
  transcript: SessionTranscriptEntry[];
  briefs: SessionBriefRecord[];
  events: {
    events?: SessionEventEnvelope[];
  };
}

interface SessionItemDraft {
  id: string;
  kind: AgentTimelineItemKind;
  label: string;
  body: string;
  timestamp: string;
  meta: string;
  minDisplayLevel: DisplayLevel;
  sourceIds: string[];
  debug?: string;
}

const displayLevelRank: Record<DisplayLevel, number> = {
  info: 0,
  verbose: 1,
  debug: 2,
};

export function reduceAgentSessionTimeline(input: ReduceAgentSessionInput): AgentTimelineItem[] {
  const transcriptItems = input.transcript.map(projectTranscriptEntry);
  const briefItems = input.briefs.map(projectBriefRecord);
  const eventItems = (input.events.events ?? []).slice(0, 12).map(projectEventEnvelope);

  return [...transcriptItems, ...briefItems, ...eventItems]
    .filter((item): item is AgentTimelineItem => Boolean(item))
    .sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
}

export function filterTimelineByDisplayLevel(
  items: AgentTimelineItem[],
  displayLevel: DisplayLevel,
): AgentTimelineItem[] {
  const rank = displayLevelRank[displayLevel];
  const filtered = items.filter((item) => displayLevelRank[item.minDisplayLevel] <= rank);
  return displayLevel === "info" ? filtered.slice(-12) : filtered;
}

function projectTranscriptEntry(entry: SessionTranscriptEntry): AgentTimelineItem | undefined {
  if (!entry.id) return undefined;
  const kind = entry.kind ?? "transcript";
  const data = asRecord(entry.data);
  const timestamp = entry.created_at ?? "";

  if (kind === "incoming_message" || kind === "continuation_prompt" || kind === "subagent_prompt") {
    return item({
      id: entry.id,
      kind: "operator",
      label: labelForTranscriptKind(kind),
      body: readableText(entry.data) || "Operator input.",
      timestamp,
      meta: compactJoin([kind, roundMeta(entry.round)]),
      minDisplayLevel: "info",
      sourceIds: [entry.id],
      debug: debugJson(entry),
    });
  }

  if (kind === "assistant_round") {
    const text = textFromAssistantBlocks(data?.blocks);
    const toolNames = toolNamesFromAssistantBlocks(data?.blocks);
    return item({
      id: entry.id,
      kind: "assistant",
      label: "Assistant progress",
      body: text || summarizeAssistantRound(toolNames),
      timestamp,
      meta: compactJoin([
        "assistant round",
        roundMeta(entry.round),
        entry.stop_reason ?? undefined,
        toolNames.length ? `tools: ${toolNames.join(", ")}` : undefined,
      ]),
      minDisplayLevel: "verbose",
      sourceIds: [entry.id],
      debug: debugJson(entry),
    });
  }

  if (kind === "tool_results") {
    return item({
      id: entry.id,
      kind: "tool",
      label: "Tool result",
      body: summarizeToolResults(data?.results),
      timestamp,
      meta: compactJoin(["tool results", roundMeta(entry.round)]),
      minDisplayLevel: "verbose",
      sourceIds: [entry.id],
      debug: debugJson(entry),
    });
  }

  if (kind === "runtime_failure") {
    return item({
      id: entry.id,
      kind: "system",
      label: "Runtime failure",
      body: readableText(entry.data) || "Runtime failure recorded.",
      timestamp,
      meta: compactJoin([kind, roundMeta(entry.round)]),
      minDisplayLevel: "info",
      sourceIds: [entry.id],
      debug: debugJson(entry),
    });
  }

  return item({
    id: entry.id,
    kind: "system",
    label: labelForTranscriptKind(kind),
    body: readableText(entry.data) || "Transcript entry recorded.",
    timestamp,
    meta: compactJoin([kind, roundMeta(entry.round)]),
    minDisplayLevel: "debug",
    sourceIds: [entry.id],
    debug: debugJson(entry),
  });
}

function projectBriefRecord(brief: SessionBriefRecord): AgentTimelineItem | undefined {
  if (!brief.id && !brief.text) return undefined;
  const id = brief.id ?? `brief-${brief.created_at ?? brief.text}`;
  return item({
    id,
    kind: "assistant",
    label: brief.kind === "result" ? "Result" : brief.kind ?? "Brief",
    body: brief.text ?? "Brief text unavailable.",
    timestamp: brief.created_at ?? "",
    meta: compactJoin(["brief", brief.kind]),
    minDisplayLevel: "info",
    sourceIds: [id],
    debug: debugJson(brief),
  });
}

function projectEventEnvelope(event: SessionEventEnvelope): AgentTimelineItem | undefined {
  if (!event.id && event.event_seq == null) return undefined;
  const id = event.id ?? `event-${event.event_seq}`;
  const payload = asRecord(event.payload);
  const eventType = event.type ?? "runtime_event";
  const projection = projectRuntimeEvent(eventType, payload);

  return item({
    id,
    kind: projection.kind,
    label: projection.label,
    body: projection.body,
    timestamp: event.ts ?? "",
    meta: event.event_seq == null ? eventType : `${eventType} · event #${event.event_seq}`,
    minDisplayLevel: projection.minDisplayLevel,
    sourceIds: [id],
    debug: debugJson(event),
  });
}

function projectRuntimeEvent(
  eventType: string,
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel"> {
  if (eventType === "tool_executed") {
    return {
      kind: "tool",
      label: "Tool executed",
      body: summarizeToolExecution(payload),
      minDisplayLevel: "verbose",
    };
  }

  if (eventType.startsWith("work_item_")) {
    return {
      kind: "system",
      label: "Work item",
      body: summarizeWorkItemEvent(eventType, payload),
      minDisplayLevel: "verbose",
    };
  }

  if (eventType === "assistant_round_recorded") {
    return {
      kind: "assistant",
      label: "Assistant round",
      body: compactJoin([
        stringField(payload, "text_preview"),
        numberField(payload, "tool_call_count") == null ? undefined : `${numberField(payload, "tool_call_count")} tool calls`,
        stringField(payload, "stop_reason"),
      ]),
      minDisplayLevel: "verbose",
    };
  }

  if (eventType === "wait_condition_registered" || eventType === "agent_waiting") {
    return {
      kind: "system",
      label: "Waiting",
      body: readableText(payload) || "Agent is waiting for an external condition.",
      minDisplayLevel: "info",
    };
  }

  if (eventType.includes("failed") || eventType.includes("error")) {
    return {
      kind: "system",
      label: "Runtime alert",
      body: readableText(payload) || humanizeEventType(eventType),
      minDisplayLevel: "info",
    };
  }

  if (eventType.startsWith("provider_") || eventType.startsWith("task_")) {
    return {
      kind: "event",
      label: humanizeEventType(eventType),
      body: summarizeDebugEvent(eventType, payload),
      minDisplayLevel: "debug",
    };
  }

  return {
    kind: "event",
    label: humanizeEventType(eventType),
    body: readableText(payload) || humanizeEventType(eventType),
    minDisplayLevel: "debug",
  };
}

function item(draft: SessionItemDraft): AgentTimelineItem {
  return draft;
}

function labelForTranscriptKind(kind: string): string {
  if (kind === "incoming_message") return "Operator input";
  if (kind === "continuation_prompt") return "Continuation";
  if (kind === "subagent_prompt") return "Delegation";
  if (kind === "assistant_round") return "Assistant progress";
  if (kind === "tool_results") return "Tool results";
  if (kind === "runtime_failure") return "Runtime failure";
  return humanizeEventType(kind);
}

function summarizeAssistantRound(toolNames: string[]): string {
  if (toolNames.length === 0) return "Assistant round completed.";
  return `Assistant requested ${toolNames.length} tool call${toolNames.length === 1 ? "" : "s"}: ${toolNames.join(", ")}.`;
}

function summarizeToolResults(value: unknown): string {
  if (!Array.isArray(value)) return "Tool results recorded.";
  const errorCount = value.filter((result) => asRecord(result)?.is_error === true).length;
  const successCount = value.length - errorCount;
  return compactJoin([
    `${value.length} tool result${value.length === 1 ? "" : "s"} recorded`,
    successCount ? `${successCount} ok` : undefined,
    errorCount ? `${errorCount} error${errorCount === 1 ? "" : "s"}` : undefined,
  ]);
}

function summarizeToolExecution(payload: Record<string, unknown> | undefined): string {
  const toolName = stringField(payload, "tool_name") ?? "tool";
  const status = stringField(payload, "status") ?? (payload?.error ? "failed" : "completed");
  const summary = stringField(payload, "summary");
  const display = stringField(payload, "exec_command_display");
  const batchItems = arrayField(payload, "exec_command_batch_items");
  const applyPatch = asRecord(payload?.apply_patch_result);

  if (summary) return `${toolName}: ${summary}`;
  if (display) return `${toolName}: ${display}`;
  if (batchItems?.length) return `${toolName}: ${batchItems.length} command batch item${batchItems.length === 1 ? "" : "s"} ${status}`;
  if (applyPatch) return `${toolName}: patch ${status}`;
  return `${toolName} ${status}.`;
}

function summarizeWorkItemEvent(eventType: string, payload: Record<string, unknown> | undefined): string {
  const action = stringField(payload, "action") ?? eventType.replace(/^work_item_/, "");
  const record = asRecord(payload?.record);
  const objective = stringField(record, "objective");
  const id = stringField(record, "id");
  return compactJoin([humanizeEventType(`work_item_${action}`), objective, id]);
}

function summarizeDebugEvent(eventType: string, payload: Record<string, unknown> | undefined): string {
  const model = stringField(payload, "active_model");
  const stopReason = stringField(payload, "stop_reason");
  const tokens = numberField(payload, "input_tokens") == null && numberField(payload, "output_tokens") == null
    ? undefined
    : `${numberField(payload, "input_tokens") ?? 0} in / ${numberField(payload, "output_tokens") ?? 0} out`;
  return compactJoin([humanizeEventType(eventType), model, stopReason, tokens]);
}

function readableText(value: unknown): string {
  if (typeof value === "string") return value;
  const record = asRecord(value);
  if (!record) return "";

  for (const key of ["text", "content", "summary", "brief", "message", "reason", "text_preview"]) {
    const candidate = record[key];
    if (typeof candidate === "string" && candidate.trim()) return candidate;
  }

  return "";
}

function textFromAssistantBlocks(value: unknown): string {
  if (!Array.isArray(value)) return "";
  return value
    .map((block) => {
      const record = asRecord(block);
      return record?.type === "text" ? stringField(record, "text") : undefined;
    })
    .filter((text): text is string => Boolean(text?.trim()))
    .join("\n\n");
}

function toolNamesFromAssistantBlocks(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .map((block) => {
      const record = asRecord(block);
      return record?.type === "tool_use" ? stringField(record, "name") : undefined;
    })
    .filter((name): name is string => Boolean(name?.trim()));
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value != null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : undefined;
}

function stringField(value: Record<string, unknown> | undefined, key: string): string | undefined {
  const candidate = value?.[key];
  return typeof candidate === "string" && candidate.trim() ? candidate : undefined;
}

function numberField(value: Record<string, unknown> | undefined, key: string): number | undefined {
  const candidate = value?.[key];
  return typeof candidate === "number" ? candidate : undefined;
}

function arrayField(value: Record<string, unknown> | undefined, key: string): unknown[] | undefined {
  const candidate = value?.[key];
  return Array.isArray(candidate) ? candidate : undefined;
}

function roundMeta(round: number | null | undefined): string | undefined {
  return round == null ? undefined : `round ${round}`;
}

function humanizeEventType(value: string): string {
  return value
    .replace(/_/g, " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function compactJoin(parts: Array<string | undefined | null>): string {
  return parts.filter(Boolean).join(" · ");
}

function sortableTime(value: string): number {
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function debugJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
