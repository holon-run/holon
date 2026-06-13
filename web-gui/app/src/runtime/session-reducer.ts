import type {
  AgentTimelineActivity,
  AgentTimelineItem,
  AgentTimelineItemDetail,
  AgentTimelineItemKind,
  DisplayLevel,
} from "./types";

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
  eventDisplayLevel?: DisplayLevel;
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
  const eventDisplayLevel = input.eventDisplayLevel ?? "debug";
  const eventItems = (input.events.events ?? []).map((event) => projectEventEnvelope(event, eventDisplayLevel));

  const sorted = mergeAgentTimelineItems([], [...transcriptItems, ...briefItems, ...eventItems])
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
    .filter((item) => displayLevelRank[item.minDisplayLevel] <= rank)
    .map((item) => ({
      ...item,
      activities:
        displayLevel === "info"
          ? []
          : (item.activities ?? []).filter((activity) => displayLevelRank[activity.minDisplayLevel] <= rank),
    }));
  const limit = options.itemLimit ?? (displayLevel === "info" ? 12 : 160);
  return filtered.slice(-limit);
}

function projectTranscriptEntry(entry: SessionTranscriptEntry): AgentTimelineItem | undefined {
  if (!entry.id) return undefined;
  const kind = entry.kind ?? "transcript";
  const data = asRecord(entry.data);
  const timestamp = entry.created_at ?? "";

  if (kind === "incoming_message") {
    const message = messageEnvelopeProjection(data);
    if (message?.origin !== "operator") {
      return item({
        id: entry.id,
        kind: "system",
        label: "Runtime input",
        body: message?.body || readableText(entry.data) || "Runtime input received.",
        timestamp,
        meta: compactJoin([kind, roundMeta(entry.round)]),
        minDisplayLevel: "verbose",
        sourceIds: [entry.id],
        debug: debugJson(entry),
      });
    }

    return item({
      id: entry.id,
      kind: "operator",
      label: labelForTranscriptKind(kind),
      body: message.body || readableText(entry.data) || "Operator input.",
      timestamp,
      meta: compactJoin([kind, roundMeta(entry.round)]),
      minDisplayLevel: "info",
      sourceIds: [entry.id],
      debug: debugJson(entry),
    });
  }

  if (kind === "continuation_prompt" || kind === "subagent_prompt") {
    return item({
      id: entry.id,
      kind: "system",
      label: labelForTranscriptKind(kind),
      body: readableText(entry.data) || labelForTranscriptKind(kind),
      timestamp,
      meta: compactJoin([kind, roundMeta(entry.round)]),
      minDisplayLevel: "verbose",
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

function projectEventEnvelope(event: SessionEventEnvelope, eventDisplayLevel: DisplayLevel): AgentTimelineItem | undefined {
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
    timestamp: projection.timestamp ?? event.ts ?? "",
    meta: event.event_seq == null ? eventType : `${eventType} · event #${event.event_seq}`,
    minDisplayLevel: capDisplayLevel(projection.minDisplayLevel, eventDisplayLevel),
    sourceIds: [id],
    detail: projection.detail,
    debug: debugJson(event),
  });
}

function capDisplayLevel(level: DisplayLevel, maxLevel: DisplayLevel): DisplayLevel {
  void maxLevel;
  return level;
}

function projectRuntimeEvent(
  eventType: string,
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> & { timestamp?: string } {
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
      minDisplayLevel: "debug",
    };
  }

  if (eventType === "brief_created") {
    return {
      kind: "assistant",
      label: stringField(payload, "kind") === "result" ? "Result" : "Brief Created",
      body: stringField(payload, "text") ?? "Brief text unavailable.",
      timestamp: stringField(payload, "created_at"),
      minDisplayLevel: "info",
    };
  }

  if (eventType === "tool_executed" || eventType === "tool_execution_failed") {
    return projectToolExecution(eventType, payload);
  }

  if (
    eventType === "turn_local_checkpoint_resume_requested" ||
    eventType === "turn_local_checkpoint_requested" ||
    eventType === "turn_local_checkpoint_recorded" ||
    eventType === "continuation_trigger_received" ||
    eventType === "continuation_resolved" ||
    eventType === "closure_decided"
  ) {
    return {
      kind: "system",
      label: systemRuntimeLabel(eventType),
      body: summarizeSystemRuntimeEvent(eventType, payload),
      minDisplayLevel: "debug",
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
    const textPreview = cleanStringField(payload, "text_preview");
    const toolNames = toolNamesFromPayload(payload);
    const stopReason = stringField(payload, "stop_reason");
    if (!textPreview) {
      return {
        kind: "assistant",
        label: toolNames.length ? "Assistant requested tools" : "Assistant round",
        body: toolNames.length
          ? toolNames.join(", ")
          : compactJoin([
              stopReason ? `Stop reason: ${stopReason}` : undefined,
              numberField(payload, "tool_call_count") == null ? undefined : `${numberField(payload, "tool_call_count")} tool calls`,
            ]) || "Assistant round completed.",
        minDisplayLevel: toolNames.length ? "verbose" : "debug",
      };
    }

    return {
      kind: "assistant",
      label: "Assistant round",
      body: textPreview,
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

function timelineDedupeKey(item: AgentTimelineItem): string {
  if (item.kind === "operator") {
    return `operator:${normalizeTextKey(item.body)}`;
  }
  if (item.kind === "assistant") {
    return `assistant:${normalizeTextKey(item.body)}`;
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
  const deduped = flattened.filter((candidate, index) => !isAssistantPreviewDuplicate(candidate, flattened, index));
  return attachActivitiesToConversationItems(deduped);
}

function isAssistantPreviewDuplicate(candidate: AgentTimelineItem, items: AgentTimelineItem[], candidateIndex: number): boolean {
  if (candidate.kind !== "assistant" || candidate.label !== "Assistant round") return false;
  const candidateText = normalizeTextKey(candidate.body);
  if (candidateText.length < 80) return false;

  return items.some((item, index) => {
    if (index === candidateIndex || item.kind !== "assistant" || item.minDisplayLevel !== "info") return false;
    return isSameAssistantText(candidateText, normalizeTextKey(item.body));
  });
}

function isSameAssistantText(left: string, right: string): boolean {
  if (!left || !right) return false;
  if (left === right) return true;

  const [shorter, longer] = left.length < right.length ? [left, right] : [right, left];
  if (shorter.length < 160) return false;
  return longer.startsWith(shorter);
}

function flattenTimelineActivities(items: AgentTimelineItem[]): AgentTimelineItem[] {
  return items.flatMap((item) => {
    const base: AgentTimelineItem = { ...item, activities: undefined };
    const activities = (item.activities ?? []).map(activityToTimelineItem);
    return [base, ...activities];
  });
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
    debug: activity.debug,
  };
}

function attachActivitiesToConversationItems(items: AgentTimelineItem[]): AgentTimelineItem[] {
  const conversationItems: AgentTimelineItem[] = [];
  const orphanActivities: AgentTimelineActivity[] = [];

  for (const item of items) {
    const cleanItem: AgentTimelineItem = { ...item, activities: undefined };
    if (!isCompactActivityItem(cleanItem)) {
      conversationItems.push(cleanItem);
      continue;
    }

    const activity = timelineItemToActivity(cleanItem);
    const target = nearestConversationItem(conversationItems, cleanItem);
    if (target) {
      if (!isActivityDuplicateOfTarget(activity, target)) {
        target.activities = mergeTimelineActivities(target.activities ?? [], [activity]);
      }
    } else {
      orphanActivities.push(activity);
    }
  }

  if (orphanActivities.length) {
    const minDisplayLevel = orphanActivities.reduce<DisplayLevel>(
      (level, activity) => (displayLevelRank[activity.minDisplayLevel] < displayLevelRank[level] ? activity.minDisplayLevel : level),
      "debug",
    );
    conversationItems.push({
      id: `activity:${orphanActivities[0].id}`,
      kind: "system",
      label: "Agent activity",
      body: summarizeActivityGroup(orphanActivities),
      timestamp: orphanActivities[0].timestamp,
      meta: "activity",
      minDisplayLevel,
      sourceIds: orphanActivities.flatMap((activity) => activity.sourceIds),
      activities: orphanActivities,
      debug: debugJson(orphanActivities),
    });
  }

  return conversationItems.sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
}

function nearestConversationItem(items: AgentTimelineItem[], activity: AgentTimelineItem): AgentTimelineItem | undefined {
  const activityTime = sortableTime(activity.timestamp);
  const maxDistanceMs = 5 * 60 * 1000;
  let nearest: AgentTimelineItem | undefined;
  let nearestDistance = Number.POSITIVE_INFINITY;

  for (const item of items) {
    if (item.kind === "operator") continue;
    const distance = Math.abs(sortableTime(item.timestamp) - activityTime);
    if (distance <= maxDistanceMs && distance < nearestDistance) {
      nearest = item;
      nearestDistance = distance;
    }
  }

  return nearest;
}

function isCompactActivityItem(item: AgentTimelineItem): boolean {
  if (item.kind === "tool" || item.kind === "event") return true;
  return item.kind === "system" && item.minDisplayLevel !== "info";
}

function isActivityDuplicateOfTarget(activity: AgentTimelineActivity, target: AgentTimelineItem): boolean {
  const activityText = normalizeTextKey(activity.body);
  const targetText = normalizeTextKey(target.body);
  if (!activityText || !targetText) return false;
  if (activityText === targetText) return true;
  if (activityText.length > 120 && targetText.startsWith(activityText)) return true;
  if (targetText.length > 120 && activityText.startsWith(targetText)) return true;
  return false;
}

function timelineItemToActivity(item: AgentTimelineItem): AgentTimelineActivity {
  return {
    id: item.id,
    kind: item.kind,
    label: item.label,
    body: item.body,
    timestamp: item.timestamp,
    meta: item.meta,
    minDisplayLevel: item.minDisplayLevel,
    sourceIds: item.sourceIds,
    detail: item.detail,
    debug: item.debug,
  };
}

function summarizeActivityGroup(activities: AgentTimelineActivity[]): string {
  const toolCount = activities.filter((activity) => activity.kind === "tool").length;
  const eventCount = activities.length - toolCount;
  return compactJoin([
    toolCount ? `${toolCount} tool ${toolCount === 1 ? "activity" : "activities"}` : undefined,
    eventCount ? `${eventCount} runtime ${eventCount === 1 ? "signal" : "signals"}` : undefined,
  ]);
}

function mergeTimelineItemActivities(preferred: AgentTimelineItem, fallback: AgentTimelineItem): AgentTimelineItem {
  return {
    ...preferred,
    sourceIds: Array.from(new Set([...fallback.sourceIds, ...preferred.sourceIds])),
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

function projectToolExecution(
  eventType: string,
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "kind" | "label" | "body" | "minDisplayLevel" | "detail"> {
  const toolName = stringField(payload, "tool_name") ?? "tool";
  const failed = eventType === "tool_execution_failed" || Boolean(payload?.error);
  const projection = projectKnownToolExecution(toolName, payload);
  const label = toolFriendlyLabel(toolName, failed);
  const summary = stringField(payload, "summary");
  const commandPreview = execCommandPreview(payload);
  const result = asRecord(payload?.exec_command_result);
  const exitStatus = numberField(payload, "exit_status") ?? numberField(result, "exit_status");
  const durationMs = numberField(payload, "duration_ms") ?? numberField(result, "duration_ms");
  const error = stringField(payload, "error");
  const body = compactJoin([
    exitStatus == null ? undefined : `exit ${exitStatus}`,
    durationMs == null ? undefined : formatDuration(durationMs),
    projection?.body,
    commandPreview || projection ? undefined : summary,
    error,
  ]);
  const outputPreview = commandOutputPreview(payload);
  const detail = projection?.detail ?? toolExecutionDetail(toolName, payload, commandPreview, outputPreview);

  return {
    kind: "tool",
    label,
    body: body || (failed ? "Failed." : "Completed."),
    detail,
    minDisplayLevel: "verbose",
  };
}

function projectKnownToolExecution(
  toolName: string,
  payload: Record<string, unknown> | undefined,
): Pick<SessionItemDraft, "body" | "detail"> | undefined {
  if (toolName === "ApplyPatch") return projectApplyPatchTool(payload);
  return undefined;
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
): AgentTimelineItemDetail | undefined {
  if (toolName === "ExecCommandBatch") {
    const batchDetail = commandBatchDetail(payload);
    if (batchDetail) return { label: "Commands", text: batchDetail, tone: "command" };
  }

  if (commandPreview && outputPreview) {
    return { label: "Command + output", text: `${commandPreview}\n\nOutput:\n${outputPreview}`, tone: "command" };
  }
  if (commandPreview) {
    return { label: toolName === "ExecCommandBatch" ? "Commands" : "Command", text: commandPreview, tone: "command" };
  }
  if (outputPreview) return { label: "Output", text: outputPreview, tone: "output" };
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

function toolNamesFromPayload(value: Record<string, unknown> | undefined): string[] {
  const toolNames = arrayField(value, "tool_names");
  if (!toolNames?.length) return [];
  return toolNames
    .map((name) => (typeof name === "string" ? name.trim() : ""))
    .filter((name): name is string => Boolean(name));
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
