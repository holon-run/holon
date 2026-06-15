import type {
  AgentTimelineActivity,
  AgentTimelineItem,
  AgentTimelineItemDetail,
  AgentTimelineItemKind,
  DisplayLevel,
} from "./types";

export interface SessionEventEnvelope {
  id?: string;
  event_seq?: number;
  ts?: string;
  type?: string;
  payload?: unknown;
}

export interface ReduceAgentSessionInput {
  events: {
    events?: SessionEventEnvelope[];
  };
  eventDisplayLevel?: DisplayLevel;
  includeDebug?: boolean;
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
  detail?: AgentTimelineItemDetail;
  rawEvent?: unknown;
  debug?: string;
}

const displayLevelRank: Record<DisplayLevel, number> = {
  info: 0,
  verbose: 1,
  debug: 2,
};
const maxTimelineSourceIds = 200;
const infoRuntimeEvents = new Set(["brief_created", "agent_waiting"]);
const verboseRuntimeEventPrefixes = ["work_item_"];
const debugRuntimeEventNames = new Set(["work_item_focus_released", "work_item_stale_reminder_injected"]);
const debugRuntimeEventPrefixes = ["provider_", "task_"];
const debugRuntimeEvents = new Set([
  "message_enqueued",
  "message_processing_started",
  "turn_local_checkpoint_resume_requested",
  "turn_local_checkpoint_requested",
  "turn_local_checkpoint_recorded",
  "continuation_trigger_received",
  "continuation_resolved",
  "closure_decided",
]);
const debugOnlyToolNames = new Set(["WaitFor"]);

export function reduceAgentSessionTimeline(input: ReduceAgentSessionInput): AgentTimelineItem[] {
  const eventDisplayLevel = input.eventDisplayLevel ?? "debug";
  const eventItems = (input.events.events ?? []).map((event) =>
    projectEventEnvelope(event, eventDisplayLevel, input.includeDebug ?? false),
  );

  const sorted = mergeAgentTimelineItems([], eventItems)
    .filter((item): item is AgentTimelineItem => Boolean(item))
    .sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));

  return compactAgentTimelineItems(sorted);
}

export function mergeAgentTimelineItems(
  existing: Array<AgentTimelineItem | undefined>,
  incoming: Array<AgentTimelineItem | undefined>,
): AgentTimelineItem[] {
  const bySemanticKey = new Map<string, AgentTimelineItem>();

  for (const candidate of [...existing, ...incoming]) {
    if (!candidate) continue;
    const key = timelineDedupeKey(candidate);
    const current = bySemanticKey.get(key);
    if (!current || timelineItemPriority(candidate) >= timelineItemPriority(current)) {
      bySemanticKey.set(key, current ? mergeTimelineItemActivities(current, candidate) : candidate);
    } else if (current) {
      bySemanticKey.set(key, mergeTimelineItemActivities(candidate, current));
    }
  }

  return Array.from(bySemanticKey.values());
}

export function filterTimelineByDisplayLevel(
  items: AgentTimelineItem[],
  displayLevel: DisplayLevel,
  options: { itemLimit?: number } = {},
): AgentTimelineItem[] {
  const rank = displayLevelRank[displayLevel];
  const filtered = items
    .map((item) => ({
      ...item,
      activities:
        displayLevel === "info"
          ? (item.activities ?? []).filter(isInfoActivity)
          : (item.activities ?? []).filter((activity) => displayLevelRank[activity.minDisplayLevel] <= rank),
    }))
    .filter((item) => displayLevelRank[item.minDisplayLevel] <= rank || Boolean(item.activities?.length));
  const limit = options.itemLimit ?? (displayLevel === "info" ? 12 : 160);
  return filtered.slice(-limit);
}

function projectEventEnvelope(
  event: SessionEventEnvelope,
  eventDisplayLevel: DisplayLevel,
  includeDebug: boolean,
): AgentTimelineItem | undefined {
  if (!event.id && event.event_seq == null) return undefined;
  const id = event.id ?? `event-${event.event_seq}`;
  const payload = asRecord(event.payload);
  const eventType = event.type ?? "runtime_event";
  const projection = projectRuntimeEvent(eventType, payload);
  if (!projection) return undefined;
  const meta = eventMeta(eventType, payload, event.event_seq);

  return item({
    id,
    kind: projection.kind,
    label: projection.label,
    body: projection.body,
    timestamp: projection.timestamp ?? event.ts ?? "",
    meta,
    minDisplayLevel: eventProjectionDisplayLevel(projection.minDisplayLevel, eventDisplayLevel),
    sourceIds: [id],
    detail: projection.detail,
    rawEvent: event,
    debug: includeDebug ? debugJson(event) : undefined,
  });
}

function eventMeta(eventType: string, payload: Record<string, unknown> | undefined, eventSeq: number | undefined): string {
  const eventRef = eventSeq == null ? undefined : `event #${eventSeq}`;
  if (eventType === "message_enqueued" && messageEnvelopeProjection(payload)?.origin === "operator") {
    return compactJoin(["Sent", eventRef]);
  }
  return eventRef == null ? eventType : `${eventType} · ${eventRef}`;
}

function eventProjectionDisplayLevel(level: DisplayLevel, eventDisplayLevel: DisplayLevel): DisplayLevel {
  // `eventDisplayLevel` describes the API page that supplied the event. It must
  // not promote or demote a semantic projection: display filtering is applied
  // later against each item's intrinsic `minDisplayLevel`.
  void eventDisplayLevel;
  return level;
}

function projectRuntimeEvent(
  eventType: string,
  payload: Record<string, unknown> | undefined,
): (Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> & { timestamp?: string }) | undefined {
  if (eventType === "message_enqueued") {
    const message = messageEnvelopeProjection(payload);
    if (message?.origin === "operator") {
      return {
        kind: "operator",
        label: "Operator input",
        body: message.body || "Operator input.",
        minDisplayLevel: "info",
      };
    }

    return {
      kind: "system",
      label: "Message queued",
      body: message?.body || readableText(payload) || "Runtime message queued.",
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType === "brief_created") {
    return {
      kind: "assistant",
      label: stringField(payload, "kind") === "result" ? "Result" : "Brief Created",
      body: stringField(payload, "text") ?? "Brief text unavailable.",
      timestamp: stringField(payload, "created_at"),
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType === "assistant_round_recorded") {
    return projectAssistantRoundRecorded(payload);
  }

  if (eventType === "tool_executed" || eventType === "tool_execution_failed") {
    return projectToolExecution(eventType, payload);
  }

  if (eventType === "message_processing_started") {
    return {
      kind: "system",
      label: "Started processing",
      body: compactJoin([stringField(payload, "origin") === "operator" ? "Operator input" : undefined, stringField(payload, "run_id")]) ||
        "Agent started processing input.",
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (debugRuntimeEvents.has(eventType)) {
    return {
      kind: "system",
      label: systemRuntimeLabel(eventType),
      body: summarizeSystemRuntimeEvent(eventType, payload),
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType.startsWith("work_item_")) {
    return {
      kind: "system",
      label: "Work item",
      body: summarizeWorkItemEvent(eventType, payload),
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  if (eventType === "wait_condition_registered" || eventType === "agent_waiting") {
    return {
      kind: "system",
      label: "Waiting",
      body: readableText(payload) || "Agent is waiting for an external condition.",
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
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
      minDisplayLevel: runtimeEventDisplayLevel(eventType),
    };
  }

  return {
    kind: "event",
    label: humanizeEventType(eventType),
    body: readableText(payload) || humanizeEventType(eventType),
    minDisplayLevel: "debug",
  };
}

function runtimeEventDisplayLevel(eventType: string): DisplayLevel {
  if (infoRuntimeEvents.has(eventType)) return "info";
  if (eventType.includes("failed") || eventType.includes("error")) return "info";
  if (debugRuntimeEventNames.has(eventType)) return "debug";
  if (debugRuntimeEvents.has(eventType)) return "debug";
  if (debugRuntimeEventPrefixes.some((prefix) => eventType.startsWith(prefix))) return "debug";
  if (verboseRuntimeEventPrefixes.some((prefix) => eventType.startsWith(prefix))) return "verbose";
  return "debug";
}

function projectAssistantRoundRecorded(
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> | undefined {
  const textPreview = stringField(payload, "text_preview");
  if (textPreview) {
    return {
      kind: "assistant",
      label: "Assistant round",
      body: textPreview,
      minDisplayLevel: "verbose",
    };
  }

  return undefined;
}

function timelineDedupeKey(item: AgentTimelineItem): string {
  if (item.kind === "operator") {
    return `operator:${normalizeTextKey(item.body)}`;
  }
  if (item.kind === "assistant") {
    return `assistant:${item.id}`;
  }
  return `item:${item.id}`;
}

function timelineItemPriority(item: AgentTimelineItem): number {
  if (item.id.startsWith("operator-prompt:pending:")) return 0;
  if (item.sourceIds.includes("pending-operator-prompt")) return 0;
  if (item.id.startsWith("event-") || item.meta.includes("event #")) return 1;
  return 2;
}

function item(draft: SessionItemDraft): AgentTimelineItem {
  return draft;
}

export function compactAgentTimelineItems(items: AgentTimelineItem[]): AgentTimelineItem[] {
  const flattened = flattenTimelineActivities(items);
  const finalBriefTexts = flattened.filter(isFinalBriefItem).map((item) => normalizeAssistantBriefText(item.body));
  const deduped = flattened.filter((candidate) => !isAssistantPreviewDuplicate(candidate, finalBriefTexts));
  return deduped.sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
}

function isAssistantPreviewDuplicate(candidate: AgentTimelineItem, finalBriefTexts: string[]): boolean {
  if (candidate.kind !== "assistant" || candidate.label !== "Assistant round") return false;
  const candidateText = normalizeAssistantBriefText(candidate.body);
  if (!candidateText) return false;

  return finalBriefTexts.some((briefText) => isSameAssistantBriefText(candidateText, briefText));
}

function isFinalBriefItem(item: AgentTimelineItem): boolean {
  return item.kind === "assistant" && item.minDisplayLevel === "info" && (item.label === "Result" || item.label === "Brief Created");
}

function isSameAssistantBriefText(previewText: string, briefText: string): boolean {
  if (!previewText || !briefText) return false;
  if (previewText === briefText) return true;
  return briefText.startsWith(previewText);
}

function normalizeAssistantBriefText(text: string): string {
  return normalizeTextKey(text).replace(/(?:\s*(?:\.{3}|…))+$/u, "").trim();
}

function flattenTimelineActivities(items: AgentTimelineItem[]): AgentTimelineItem[] {
  return items.flatMap((item) => {
    const base: AgentTimelineItem = { ...item, activities: undefined };
    const activities = (item.activities ?? []).filter(shouldFlattenActivity).map(activityToTimelineItem);
    return [base, ...activities];
  });
}

function shouldFlattenActivity(activity: AgentTimelineActivity): boolean {
  if (isEphemeralRuntimeActivity(activity)) return false;
  return true;
}

function isEphemeralRuntimeActivity(activity: Pick<AgentTimelineActivity, "kind" | "meta">): boolean {
  if (activity.kind !== "tool" && activity.kind !== "event" && activity.kind !== "system") return false;
  return (
    activity.meta.startsWith("tool_executed") ||
    activity.meta.startsWith("tool_execution_failed") ||
    activity.meta.startsWith("wait_condition_registered") ||
    activity.meta.startsWith("agent_waiting")
  );
}

function activityToTimelineItem(activity: AgentTimelineActivity): AgentTimelineItem {
  return {
    id: activity.id,
    kind: activity.kind,
    label: activity.label,
    body: activity.body,
    timestamp: activity.timestamp,
    meta: activity.meta,
    minDisplayLevel: activity.minDisplayLevel,
    sourceIds: activity.sourceIds,
    detail: activity.detail,
    rawEvent: activity.rawEvent,
    debug: activity.debug,
  };
}

function isInfoActivity(activity: AgentTimelineActivity): boolean {
  if (activity.kind !== "tool") return false;
  return true;
}

function mergeTimelineItemActivities(preferred: AgentTimelineItem, fallback: AgentTimelineItem): AgentTimelineItem {
  return {
    ...preferred,
    sourceIds: mergeSourceIds([...fallback.sourceIds, ...preferred.sourceIds]),
    activities: mergeTimelineActivities(fallback.activities ?? [], preferred.activities ?? []),
  };
}

function mergeTimelineActivities(
  existing: AgentTimelineActivity[],
  incoming: AgentTimelineActivity[],
): AgentTimelineActivity[] {
  const byId = new Map<string, AgentTimelineActivity>();
  for (const activity of [...existing, ...incoming]) {
    byId.set(activity.id, activity);
  }
  return Array.from(byId.values()).sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
}

function mergeSourceIds(sourceIds: string[]): string[] {
  return Array.from(new Set(sourceIds)).slice(0, maxTimelineSourceIds);
}

function projectToolExecution(
  eventType: string,
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> | undefined {
  const toolName = stringField(payload, "tool_name") ?? "tool";
  const failed = eventType === "tool_execution_failed" || Boolean(payload?.error);
  if (!failed && isWorkItemMutationTool(toolName)) return undefined;
  const projection = projectKnownToolExecution(toolName, payload);
  const label = toolFriendlyLabel(toolName, failed);
  const summary = stringField(payload, "summary");
  const commandPreview = execCommandPreview(payload);
  const result = asRecord(payload?.exec_command_result);
  const exitStatus = numberField(payload, "exit_status") ?? numberField(result, "exit_status");
  const durationMs = numberField(payload, "duration_ms") ?? numberField(result, "duration_ms");
  const error = toolErrorMessage(payload);
  const stringPreview = toolStringPreview(toolName, payload, commandPreview) || undefined;
  const toolSummary = projection?.body ?? stringPreview ?? summary ?? genericToolDescription(toolName, payload) ?? toolName;
  const body = compactJoin([
    toolSummary,
    exitStatus == null ? undefined : `exit ${exitStatus}`,
    durationMs == null ? undefined : formatDuration(durationMs),
    error,
  ]);
  const outputPreview = commandOutputPreview(payload);
  const detail = projection?.detail ?? toolExecutionDetail(toolName, payload, commandPreview, outputPreview, toolSummary, failed ? error : undefined);

  return {
    kind: "tool",
    label,
    body: body || (failed ? "Failed." : "Completed."),
    detail,
    minDisplayLevel: toolTimelineDisplayLevel(toolName),
  };
}

function toolTimelineDisplayLevel(toolName: string): DisplayLevel {
  if (debugOnlyToolNames.has(toolName)) return "debug";
  return "verbose";
}

function projectKnownToolExecution(
  toolName: string,
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  if (toolName === "ApplyPatch") return projectApplyPatchTool(payload);
  if (toolName === "ListWorkItems") return projectListWorkItemsTool(payload);
  if (toolName === "GetWorkItem") return projectGetWorkItemTool(payload);
  if (toolName === "ViewImage") return projectViewImageTool(payload);
  return undefined;
}

function isWorkItemMutationTool(toolName: string): boolean {
  return toolName === "CreateWorkItem" || toolName === "UpdateWorkItem" || toolName === "PickWorkItem" || toolName === "CompleteWorkItem";
}

function toolStringPreview(
  toolName: string,
  payload: Record<string, unknown> | undefined,
  commandPreview: string | undefined,
): string | undefined {
  if (commandPreview) return commandPreview;
  if (toolName === "WaitFor") return stringField(payload, "reason");
  return readableTextWithoutSummary(payload);
}

function genericToolDescription(toolName: string, payload: Record<string, unknown> | undefined): string {
  const waitReason = stringField(payload, "reason");
  if (waitReason) return waitReason;

  const readable = readableText(payload);
  if (readable) return readable;

  const objective = stringField(payload, "objective");
  if (objective) return objective;

  const workItemId = stringField(payload, "work_item_id");
  if (workItemId) return workItemId;

  const resource = stringField(payload, "resource");
  if (resource) return resource;

  return toolName;
}

function projectApplyPatchTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.apply_patch_result);
  if (!result) return undefined;

  const changedFiles = arrayField(result, "changed_files")
    ?.map(asRecord)
    .filter((file): file is Record<string, unknown> => Boolean(file));
  const changedPaths = stringArrayField(result, "changed_paths");
  const pathSummary =
    changedFiles?.map((file) => stringField(file, "path")).filter((path): path is string => Boolean(path)) ?? changedPaths;
  const fileCount = numberField(result, "changed_file_count") ?? pathSummary.length;
  const body = compactJoin([
    fileCount ? `${fileCount} file${fileCount === 1 ? "" : "s"}` : undefined,
    pathSummary.length ? pathSummary.join(", ") : stringField(result, "summary_text"),
  ]);
  const detailText = applyPatchDetailText(result, changedFiles, changedPaths);

  return {
    body,
    detail: detailText
      ? {
          label: "Patch diff",
          text: detailText,
          tone: "diff",
        }
      : undefined,
  };
}

function projectListWorkItemsTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.list_work_items_result) ?? asRecord(payload?.result);
  const items = arrayField(result, "work_items") ?? arrayField(result, "items");
  const total = numberField(result, "total") ?? numberField(result, "total_open") ?? items?.length;
  const filter = stringField(payload, "filter") ?? stringField(result, "filter");
  const itemSummaries = summarizeWorkItemRecords(items);
  return {
    body: compactJoin([
      total == null ? "Listed work items" : `${total} work item${total === 1 ? "" : "s"}`,
      filter ? `filter: ${filter}` : undefined,
      itemSummaries.length ? itemSummaries.slice(0, 3).join("; ") : undefined,
    ]),
    detail: itemSummaries.length
      ? { label: "Work items", text: itemSummaries.join("\n"), tone: "data" }
      : { label: "Result", text: debugJson(result ?? payload ?? {}), tone: "data" },
  };
}

function projectGetWorkItemTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.get_work_item_result) ?? asRecord(payload?.result);
  const workItem = asRecord(result?.work_item) ?? asRecord(result);
  const summary = summarizeWorkItemRecord(workItem);
  const workItemId = stringField(payload, "work_item_id") ?? stringField(workItem, "id");
  return {
    body: summary || compactJoin(["Loaded work item", workItemId]) || "Loaded work item",
    detail: { label: "Work item", text: debugJson(result ?? payload ?? {}), tone: "data" },
  };
}

function projectViewImageTool(payload: Record<string, unknown> | undefined): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  const result = asRecord(payload?.view_image_result) ?? asRecord(payload?.result);
  const dimensions = asRecord(result?.dimensions);
  const width = numberField(result, "width") ?? numberField(dimensions, "width");
  const height = numberField(result, "height") ?? numberField(dimensions, "height");
  const imagePath = firstStringField(payload, ["path", "image_path"]) ?? firstStringField(result, ["path", "image_path"]);
  const observation = firstStringField(result, ["visual_observation", "observation", "text_preview"]);
  const body = compactJoin([
    "Viewed image",
    imagePath ? basename(imagePath) : undefined,
    width != null && height != null ? `${width}×${height}` : undefined,
    observation ? truncateText(observation, 120) : undefined,
  ]);

  return {
    body,
    detail: observation
      ? { label: "Visual observation", text: observation, tone: "data" }
      : { label: "Result", text: debugJson(result ?? payload ?? {}), tone: "data" },
  };
}

function summarizeWorkItemRecords(items: unknown[] | undefined): string[] {
  return (items ?? [])
    .map(asRecord)
    .filter((item): item is Record<string, unknown> => Boolean(item))
    .map(summarizeWorkItemRecord)
    .filter(Boolean);
}

function summarizeWorkItemRecord(record: Record<string, unknown> | undefined): string {
  const id = stringField(record, "id") ?? stringField(record, "work_item_id");
  const objective = stringField(record, "objective");
  const lifecycle = stringField(record, "lifecycle") ?? stringField(record, "status");
  return compactJoin([objective, lifecycle, id]);
}

function applyPatchDetailText(
  result: Record<string, unknown>,
  changedFiles: Record<string, unknown>[] | undefined,
  changedPaths: string[],
): string {
  if (changedFiles?.length) {
    return changedFiles
      .map((file) => {
        const path = stringField(file, "path") ?? "unknown path";
        const action = stringField(file, "action");
        const diffPreview = stringField(file, "diff_preview");
        const hunks = arrayField(file, "hunks");
        const hunkSummary = hunks?.length ? `${hunks.length} hunk${hunks.length === 1 ? "" : "s"}` : undefined;
        const header = compactJoin([action, path, hunkSummary]);
        return diffPreview ? `${header}\n${diffPreview}` : header;
      })
      .join("\n\n");
  }

  const patch = stringField(result, "patch") ?? stringField(result, "diff_preview");
  if (patch) return patch;
  return changedPaths.length ? changedPaths.join("\n") : "";
}

function toolExecutionDetail(
  toolName: string,
  payload: Record<string, unknown> | undefined,
  commandPreview: string | undefined,
  outputPreview: string | undefined,
  summary: string | undefined,
  error: string | undefined,
): AgentTimelineItemDetail | undefined {
  if (toolName === "ExecCommandBatch") {
    const batchDetail = commandBatchDetail(payload);
    if (batchDetail) return { label: "Commands", text: batchDetail, tone: "command" };
  }

  if (error) return { label: "Error", text: error, tone: "data" };
  if (commandPreview && outputPreview) return { label: "Output", text: outputPreview, tone: "command" };
  if (commandPreview) {
    return { label: toolName === "ExecCommandBatch" ? "Commands" : "Command", text: commandPreview, tone: "command" };
  }
  if (outputPreview) return { label: "Output", text: outputPreview, tone: "output" };
  const readable = readableText(payload);
  const readableDuplicatesSummary = Boolean(readable && summary && normalizeTextKey(readable) === normalizeTextKey(summary));
  const detailText = readable && !readableDuplicatesSummary ? readable : debugJson(payload ?? {});
  if (detailText && detailText !== "{}") return { label: "Result", text: detailText, tone: "data" };
  return undefined;
}

function commandBatchDetail(payload: Record<string, unknown> | undefined): string | undefined {
  const batchItems = arrayField(payload, "exec_command_batch_items");
  if (!batchItems?.length) return undefined;

  const resultItems = arrayField(asRecord(payload?.exec_command_result), "items") ?? [];
  const lines = batchItems
    .map((item, index) => {
      const itemRecord = asRecord(item);
      const command = firstStringField(itemRecord, ["cmd_display", "cmd"]);
      if (!command) return undefined;
      const resultRecord = asRecord(resultItems[index]);
      return formatBatchCommandLine(index, command, resultRecord);
    })
    .filter((line): line is string => Boolean(line));

  return lines.length ? lines.join("\n") : undefined;
}

function formatBatchCommandLine(index: number, command: string, resultItem: Record<string, unknown> | undefined): string {
  const status = stringField(resultItem, "status");
  const result = asRecord(resultItem?.result);
  const exitStatus = numberField(result, "exit_status");
  const outputPreview = firstStringField(result, ["stdout_preview", "stderr_preview", "output_preview"]);
  const statusText = compactJoin([status, exitStatus == null ? undefined : `exit ${exitStatus}`]);
  const headline = `${index + 1}. ${statusText ? `[${statusText}] ` : ""}${command}`;
  if (!outputPreview) return headline;
  return `${headline}\n   ${indentPreview(outputPreview)}`;
}

function indentPreview(value: string): string {
  return value
    .trim()
    .split("\n")
    .slice(0, 6)
    .join("\n   ");
}

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function truncateText(value: string, maxLength: number): string {
  return value.length > maxLength ? `${value.slice(0, maxLength - 1)}…` : value;
}

function execCommandPreview(payload: Record<string, unknown> | undefined): string | undefined {
  const direct = firstStringField(payload, ["exec_command_display", "cmd_display"]);
  if (direct) return direct;

  const batchItems = arrayField(payload, "exec_command_batch_items");
  if (batchItems?.length) {
    const commands = batchItems
      .map((item) => {
        const record = asRecord(item);
        return firstStringField(record, ["cmd_display", "cmd"]);
      })
      .filter((command): command is string => Boolean(command));
    if (commands.length) return commands.join("\n");
  }

  const result = asRecord(payload?.exec_command_result);
  const resultItems = arrayField(result, "items");
  if (resultItems?.length) {
    const commands = resultItems
      .map((item) => {
        const record = asRecord(item);
        const itemResult = asRecord(record?.result);
        return (
          firstStringField(itemResult, ["cmd_display", "cmd", "cmd_preview"]) ??
          firstStringField(record, ["cmd_display", "cmd", "cmd_preview"])
        );
      })
      .filter((command): command is string => Boolean(command));
    if (commands.length) return commands.join("\n");
  }

  return (
    firstStringField(result, ["cmd_display", "cmd"]) ??
    firstStringField(payload, ["exec_command_cmd", "cmd", "cmd_preview"]) ??
    firstStringField(asRecord(payload?.command_cost), ["cmd_preview"]) ??
    firstStringField(asRecord(payload?.exec_command_cost), ["cmd_preview"])
  );
}

function commandOutputPreview(payload: Record<string, unknown> | undefined): string | undefined {
  const result = asRecord(payload?.exec_command_result);
  return (
    firstStringField(payload, ["stdout_preview", "stderr_preview", "output_preview"]) ??
    firstStringField(result, ["stdout_preview", "stderr_preview", "output_preview", "summary_text"])
  );
}

function toolFriendlyLabel(toolName: string, failed: boolean): string {
  if (toolName === "ApplyPatch") return failed ? "Patch failed" : "Applied patch";
  if (toolName === "ExecCommand") return failed ? "Command failed" : "Command finished";
  if (toolName === "ExecCommandBatch") return failed ? "Command batch failed" : "Command batch finished";
  if (toolName === "WaitFor") return failed ? "Wait failed" : "Waiting";
  if (toolName === "UpdateWorkItem") return failed ? "Work item update failed" : "Updated work item";
  if (toolName === "PickWorkItem") return failed ? "Work item switch failed" : "Picked work item";
  if (toolName === "CompleteWorkItem") return failed ? "Work item completion failed" : "Completed work item";
  return failed ? "Tool failed" : "Tool finished";
}

function formatDuration(milliseconds: number): string {
  if (milliseconds < 1000) return `${milliseconds}ms`;
  return `${(milliseconds / 1000).toFixed(milliseconds < 10_000 ? 1 : 0)}s`;
}

function summarizeWorkItemEvent(eventType: string, payload: Record<string, unknown> | undefined): string {
  const action = stringField(payload, "action") ?? eventType.replace(/^work_item_/, "");
  const record = asRecord(payload?.record);
  const objective = stringField(record, "objective");
  const reason = stringField(payload, "reason");
  if (eventType === "work_item_picked") {
    return compactJoin(["Picked work item", objective, reason]);
  }
  if (eventType === "work_item_focus_released") {
    return compactJoin(["Released work item focus", objective, reason, stringField(payload, "readiness")]);
  }
  if (eventType === "work_item_completion_report_promoted") {
    return compactJoin(["Promoted completion report", objective, stringField(payload, "text_preview")]);
  }
  if (eventType === "work_item_completion_report_candidate_promoted") {
    return compactJoin(["Promoted completion report candidate", objective, stringField(payload, "text_preview")]);
  }
  return compactJoin([humanizeEventType(`work_item_${action}`), objective]);
}

function summarizeDebugEvent(eventType: string, payload: Record<string, unknown> | undefined): string {
  const model = stringField(payload, "active_model");
  const stopReason = stringField(payload, "stop_reason");
  const tokens = numberField(payload, "input_tokens") == null && numberField(payload, "output_tokens") == null
    ? undefined
    : `${numberField(payload, "input_tokens") ?? 0} in / ${numberField(payload, "output_tokens") ?? 0} out`;
  return compactJoin([humanizeEventType(eventType), model, stopReason, tokens]);
}

function summarizeSystemRuntimeEvent(eventType: string, payload: Record<string, unknown> | undefined): string {
  if (eventType === "turn_local_checkpoint_resume_requested") {
    return "Refreshing local context; asking the model to continue.";
  }
  if (eventType === "turn_local_checkpoint_requested") {
    return compactJoin(["Context checkpoint requested", stringField(payload, "checkpoint_mode")]);
  }
  if (eventType === "turn_local_checkpoint_recorded") {
    if (payload?.checkpoint_recorded === false) return "Context checkpoint produced no visible text.";
    return compactJoin(["Context checkpoint recorded", stringField(payload, "text_preview")]);
  }
  if (eventType === "continuation_trigger_received") {
    return readableText(payload) || "Continuation trigger received.";
  }
  if (eventType === "continuation_resolved") {
    return readableText(payload) || "Continuation resolved.";
  }
  if (eventType === "closure_decided") {
    return readableText(asRecord(payload?.closure) ?? payload) || "Closure decided.";
  }
  return readableText(payload) || humanizeEventType(eventType);
}

function systemRuntimeLabel(eventType: string): string {
  if (eventType.startsWith("turn_local_checkpoint_")) return "Context checkpoint";
  if (eventType.startsWith("continuation_")) return "Continuation";
  if (eventType === "closure_decided") return "Closure";
  return humanizeEventType(eventType);
}

function messageEnvelopeProjection(payload: Record<string, unknown> | undefined): { origin: "operator" | "runtime"; body: string } | undefined {
  if (!payload) return undefined;
  const origin = asRecord(payload.origin);
  const originKind = stringField(origin, "kind")?.toLowerCase();
  const body = asRecord(payload.body);
  return {
    origin: originKind === "operator" ? "operator" : "runtime",
    body: messageBodyText(body),
  };
}

function messageBodyText(body: Record<string, unknown> | undefined): string {
  if (!body) return "";
  const text = stringField(body, "text");
  if (text) return text;
  const message = stringField(body, "message");
  if (message) return message;
  if (body.value != null) return debugJson(body.value);
  return "";
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

function readableTextWithoutSummary(value: unknown): string {
  if (typeof value === "string") return value;
  const record = asRecord(value);
  if (!record) return "";

  for (const key of ["text", "content", "brief", "message", "reason", "text_preview"]) {
    const candidate = record[key];
    if (typeof candidate === "string" && candidate.trim()) return candidate;
  }

  return "";
}

function toolErrorMessage(payload: Record<string, unknown> | undefined): string | undefined {
  const direct = stringField(payload, "error");
  if (direct) return structuredErrorMessage(direct) ?? direct;

  const error = payload?.error;
  if (typeof error === "string" && error.trim()) return error;

  const errorRecord = asRecord(error) ?? asRecord(payload?.tool_error);
  const message = firstStringField(errorRecord, ["message", "summary", "summary_text", "reason", "detail"]);
  if (message) return message;

  const nested = firstStringField(asRecord(errorRecord?.error), ["message", "summary", "summary_text", "reason", "detail"]);
  if (nested) return nested;

  return undefined;
}

function structuredErrorMessage(value: string): string | undefined {
  const text = value.trim();
  if (!text.startsWith("{") && !text.startsWith("[")) return undefined;

  try {
    const record = asRecord(JSON.parse(text));
    return firstStringField(record, ["message", "summary", "summary_text", "reason", "detail"]);
  } catch {
    return undefined;
  }
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value != null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : undefined;
}

function stringField(value: Record<string, unknown> | undefined, key: string): string | undefined {
  const candidate = value?.[key];
  return typeof candidate === "string" && candidate.trim() ? candidate : undefined;
}

function cleanStringField(value: Record<string, unknown> | undefined, key: string): string | undefined {
  const candidate = stringField(value, key);
  if (!candidate) return undefined;
  const normalized = candidate.trim().toLowerCase();
  return normalized === "none" || normalized === "null" ? undefined : candidate;
}

function firstStringField(value: Record<string, unknown> | undefined, keys: string[]): string | undefined {
  for (const key of keys) {
    const candidate = stringField(value, key);
    if (candidate) return candidate;
  }
  return undefined;
}

function stringArrayField(value: Record<string, unknown> | undefined, key: string): string[] {
  const candidate = value?.[key];
  if (!Array.isArray(candidate)) return [];
  return candidate.filter((item): item is string => typeof item === "string" && Boolean(item.trim()));
}

function numberField(value: Record<string, unknown> | undefined, key: string): number | undefined {
  const candidate = value?.[key];
  return typeof candidate === "number" ? candidate : undefined;
}

function arrayField(value: Record<string, unknown> | undefined, key: string): unknown[] | undefined {
  const candidate = value?.[key];
  return Array.isArray(candidate) ? candidate : undefined;
}

function humanizeEventType(value: string): string {
  return value
    .replace(/_/g, " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function compactJoin(parts: Array<string | undefined | null>): string {
  return parts.filter(Boolean).join(" · ");
}

function normalizeTextKey(text: string): string {
  return text.trim().replace(/\s+/g, " ");
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
