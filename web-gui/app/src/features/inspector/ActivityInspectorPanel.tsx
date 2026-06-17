import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type {
  AgentTimelineActivity,
  AgentTimelineItemDetail,
  InspectorActivityDetailState,
  RuntimeTaskOutputResult,
  RuntimeToolExecutionRecord,
} from "../../runtime/types";

function hasHydratedDetail(detailState: InspectorActivityDetailState | undefined): boolean {
  return Boolean(detailState?.toolExecution || detailState?.taskOutput);
}

function HydratedActivityDetails({ detailState }: { detailState?: InspectorActivityDetailState }) {
  if (!detailState) return null;
  if (detailState.loading) {
    return (
      <section className="context-card inspector-card inspector-detail data">
        <div className="context-head">
          <span className="eyebrow">Full detail</span>
          <strong>Loading…</strong>
        </div>
        <pre>Fetching full tool/task detail from the runtime API.</pre>
      </section>
    );
  }
  if (detailState.error) {
    return (
      <section className="context-card inspector-card inspector-detail data">
        <div className="context-head">
          <span className="eyebrow">Full detail</span>
          <strong>Unavailable</strong>
        </div>
        <pre>{detailState.error}</pre>
      </section>
    );
  }
  return (
    <>
      {detailState.toolExecution ? <ToolExecutionDetail record={detailState.toolExecution} /> : null}
      {detailState.taskOutput ? <TaskOutputDetail output={detailState.taskOutput} /> : null}
    </>
  );
}

function ToolExecutionDetail({ record }: { record: RuntimeToolExecutionRecord }) {
  const detail = formatToolExecutionDetail(record);
  const rawText = formatInspectorJson(record);

  return (
    <>
      <section className={`context-card inspector-card inspector-detail ${detail.tone}`}>
        <div className="context-head">
          <span className="eyebrow">Tool execution</span>
          <strong>{compactMeta([record.tool_name, record.status])}</strong>
        </div>
        <pre>{detail.text}</pre>
      </section>
      {rawText ? (
        <details className="context-card inspector-card inspector-raw-detail">
          <summary>Raw tool execution JSON</summary>
          <pre>{rawText}</pre>
        </details>
      ) : null}
    </>
  );
}

export function formatToolExecutionDetail(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const knownToolDetail = formatKnownToolExecutionDetail(record);
  if (knownToolDetail) return knownToolDetail;

  const output = unwrapToolOutput(record.output ?? record.result);
  const batchItemSections = formatBatchToolOutput(record.input, output);
  const lines = [
    labelledText("Summary", record.summary),
    batchItemSections.length ? "" : labelledText("Command", commandText(record.input)),
    labelledText("Stdout", nestedText(output, ["stdout", "stdout_preview", "output", "output_preview", "combined_output_preview"])),
    labelledText("Stderr", nestedText(output, ["stderr", "stderr_preview"])),
    labelledText("Initial output", nestedText(output, ["initial_output_preview"])),
    labelledText("Result", nestedText(output, ["summary", "summary_text", "result_summary", "result_summary_preview"])),
    labelledText("Error", record.error ?? nestedValue(output, ["error"])),
    labelledText("Exit", nestedValue(output, ["exit_status", "status", "disposition"])),
    ...batchItemSections,
  ].filter(Boolean);

  return {
    text: lines.join("\n\n") || formatInspectorJson(record),
    tone: lines.length ? "output" : "data",
  };
}

function unwrapToolOutput(value: unknown): unknown {
  const envelope = isRecord(value) ? value.envelope : undefined;
  const result = isRecord(envelope) ? envelope.result : undefined;
  return result ?? value;
}

function formatKnownToolExecutionDetail(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } | undefined {
  if (record.tool_name === "ApplyPatch") return formatApplyPatchToolExecution(record);
  if (record.tool_name === "ExecCommand") return formatExecCommandToolExecution(record);
  if (record.tool_name === "ExecCommandBatch") return formatExecCommandBatchToolExecution(record);
  if (record.tool_name === "ListTasks") return formatListTasksToolExecution(record);
  if (record.tool_name === "WaitFor") return formatWaitForToolExecution(record);
  if (record.tool_name === "ViewImage") return formatViewImageToolExecution(record);
  if (isWorkItemTool(record.tool_name)) return formatWorkItemToolExecution(record);
  if (isWebSearchTool(record.tool_name)) return formatWebSearchToolExecution(record);
  if (isWebFetchTool(record.tool_name)) return formatWebFetchToolExecution(record);
  if (record.tool_name === "MemorySearch") return formatMemorySearchToolExecution(record);
  if (record.tool_name === "MemoryGet") return formatMemoryGetToolExecution(record);
  if (record.tool_name === "TaskOutput") return formatTaskOutputToolExecution(record);
  if (record.tool_name === "TaskStatus") return formatTaskStatusToolExecution(record);
  if (record.tool_name === "TaskStop") return formatTaskStopToolExecution(record);
  if (record.tool_name === "TaskInput") return formatTaskInputToolExecution(record);
  return undefined;
}

function isWebSearchTool(toolName: string | undefined): boolean {
  return toolName === "WebSearch";
}

function isWebFetchTool(toolName: string | undefined): boolean {
  return toolName === "WebFetch";
}


function formatApplyPatchToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "apply_patch_result");
  const changedFiles = arrayRecords(result?.changed_files);
  const changedPaths = stringArray(result?.changed_paths);
  const diagnostics = arrayRecords(result?.diagnostics)
    .map((diagnostic) => compactMeta([textField(diagnostic.level), textField(diagnostic.message)]))
    .filter(Boolean);
  const fileSummaries = changedFiles.map((file) => compactMeta([textField(file.action), textField(file.path) || "unknown path"]));
  const patchText = patchInputText(record.input) || textField(record.patch) || textField(result?.patch);
  const diffPreview = textField(result?.diff_preview) || changedFiles.map((file) => textField(file.diff_preview)).filter(Boolean).join("\n\n");
  const lines = [
    labelledText("Summary", record.summary || result?.summary_text),
    labelledText("Changed files", fileSummaries.length ? fileSummaries.join("\n") : changedPaths.join("\n")),
    labelledText("Diagnostics", diagnostics.join("\n")),
    labelledText(patchText ? "Patch" : "Patch preview", patchText || diffPreview),
    labelledText("Result", result?.summary_text),
    labelledText("Error", record.error),
  ].filter(Boolean);

  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatExecCommandToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const lines = [
    labelledText("Command", commandText(record.input) || textField(record.cmd_preview)),
    labelledText("Stdout", nestedText(output, ["stdout", "stdout_preview", "output", "output_preview", "combined_output_preview"])),
    labelledText("Stderr", nestedText(output, ["stderr", "stderr_preview"])),
    labelledText("Initial output", nestedText(output, ["initial_output_preview"])),
    labelledText("Result", nestedText(output, ["summary", "summary_text", "result_summary", "result_summary_preview"]) || record.summary),
    labelledText("Error", record.error ?? nestedValue(output, ["error"])),
    labelledText("Exit", nestedValue(output, ["exit_status", "status", "disposition"])),
  ].filter(Boolean);

  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatExecCommandBatchToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const batchItemSections = formatBatchToolOutput(record.input, output);
  const lines = [
    labelledText("Summary", record.summary),
    labelledText("Result", nestedText(output, ["summary", "summary_text", "result_summary", "result_summary_preview"])),
    labelledText("Error", record.error ?? nestedValue(output, ["error"])),
    ...batchItemSections,
  ].filter(Boolean);

  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatListTasksToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "list_tasks_result") ?? (isRecord(output) ? output : undefined);
  const tasks = arrayRecords(result?.tasks ?? result?.active_tasks);
  const taskLines = tasks.map(formatTaskRecord).filter(Boolean);
  const total = result?.total_active ?? result?.total ?? tasks.length;
  const returned = result?.returned ?? tasks.length;
  const lines = [
    labelledText("Summary", record.summary),
    labelledText("Tasks", taskLines.join("\n")),
    labelledText("Total active", total),
    labelledText("Returned", returned),
    labelledText("Error", record.error),
  ].filter(Boolean);

  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatTaskOutputToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "task_output_result") ?? (isRecord(output) ? output : undefined);
  const task = isRecord(result?.task) ? result.task : isRecord(result?.task_record) ? result.task_record : undefined;
  const taskId = nestedValue(task, ["task_id", "id"]) ?? nestedValue(record.input, ["task_id"]);
  const status = nestedValue(task, ["status"]) ?? nestedValue(result, ["status", "retrieval_status"]);
  const exitStatus = nestedValue(task, ["exit_status"]) ?? nestedValue(result, ["exit_status"]);
  const outputText = nestedText(result, ["output_preview", "output", "stdout", "stderr", "combined_output_preview"]);
  const truncated = nestedValue(result, ["output_truncated", "truncated"]) === true;
  const lines = [
    labelledText("Task ID", taskId),
    labelledText("Status", status),
    labelledText("Exit", exitStatus),
    labelledText("Summary", nestedValue(task, ["summary"]) ?? nestedValue(result, ["summary", "result_summary"])),
    outputText ? labelledText(truncated ? "Output (truncated)" : "Output", outputText) : "",
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatTaskStatusToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "task_status_result") ?? (isRecord(output) ? output : undefined);
  const taskId = nestedValue(result, ["task_id", "id"]) ?? nestedValue(record.input, ["task_id"]);
  const lines = [
    labelledText("Task ID", taskId),
    labelledText("Status", nestedValue(result, ["status"])),
    labelledText("Kind", nestedValue(result, ["kind"])),
    labelledText("Summary", nestedValue(result, ["summary"])),
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatTaskStopToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "task_stop_result") ?? (isRecord(output) ? output : undefined);
  const taskId = nestedValue(result, ["task_id", "id"]) ?? nestedValue(record.input, ["task_id"]);
  const lines = [
    labelledText("Task ID", taskId),
    labelledText("Status", nestedValue(result, ["status"])),
    labelledText("Summary", record.summary),
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatTaskInputToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "task_input_result") ?? (isRecord(output) ? output : undefined);
  const taskId = nestedValue(result, ["task_id", "id"]) ?? nestedValue(record.input, ["task_id"]);
  const input = nestedValue(record.input, ["input"]) ?? nestedValue(result, ["input"]);
  const lines = [
    labelledText("Task ID", taskId),
    labelledText("Input", input),
    labelledText("Status", nestedValue(result, ["status"])),
    labelledText("Summary", record.summary),
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatWaitForToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const input = isRecord(record.input) ? record.input : record;
  const lines = [
    labelledText("Reason", input.reason),
    labelledText("Wake", input.wake),
    labelledText("Resource", input.resource),
    labelledText("Recheck after", input.recheck_after_ms),
    labelledText("Result", record.summary),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: "data" };
}

function formatViewImageToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "view_image_result");
  const dimensions = isRecord(result?.dimensions) ? result.dimensions : undefined;
  const width = nestedValue(result, ["width"]) ?? nestedValue(dimensions, ["width"]);
  const height = nestedValue(result, ["height"]) ?? nestedValue(dimensions, ["height"]);
  const lines = [
    labelledText("Path", nestedValue(record.input, ["path", "image_path"]) ?? nestedValue(result, ["path", "image_path"])),
    labelledText("Dimensions", width != null && height != null ? `${width}×${height}` : ""),
    labelledText("Observation", nestedText(result, ["visual_observation", "observation", "text_preview"])),
    labelledText("Result", result?.summary_text || record.summary),
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatWebSearchToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const results = arrayRecords(nestedValue(output, ["results"]));
  const query = nestedValue(output, ["query"]) ?? nestedValue(input, ["query", "search_query", "q"]);
  const provider = nestedValue(output, ["provider"]);
  const mode = nestedValue(output, ["mode"]);
  const resultLines = results.slice(0, 15).map((item, index) => {
    const title = nestedValue(item, ["title"]);
    const url = nestedValue(item, ["url"]);
    const source = nestedValue(item, ["source"]);
    const snippet = nestedValue(item, ["snippet"]);
    const publishedAt = nestedValue(item, ["published_at"]);
    return labelledText(
      `${index + 1}. ${title ?? "Untitled"}`,
      [url, source ? `(${source})` : undefined, publishedAt ? String(publishedAt) : undefined, typeof snippet === "string" ? truncateInspectorText(snippet, 300) : undefined]
        .filter(Boolean)
        .join("\n"),
    );
  });
  const lines = [
    labelledText("Query", query),
    labelledText("Provider", provider),
    labelledText("Mode", mode),
    labelledText("Results", `${results.length} found`),
    ...resultLines,
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: results.length ? "output" : "data" };
}

function formatWebFetchToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const url = nestedValue(output, ["url"]) ?? nestedValue(input, ["url"]);
  const finalUrl = nestedValue(output, ["final_url"]);
  const status = nestedValue(output, ["status"]);
  const contentType = nestedValue(output, ["content_type"]);
  const bytesRead = nestedValue(output, ["bytes_read"]);
  const truncated = nestedValue(output, ["truncated"]);
  const text = nestedText(output, ["text"]);
  const contentPreview = typeof text === "string" && text.trim() ? truncateInspectorText(text, 2000) : undefined;
  const lines = [
    labelledText("URL", url),
    finalUrl && finalUrl !== url ? labelledText("Final URL", finalUrl) : "",
    labelledText("Status", status),
    labelledText("Content-Type", contentType),
    labelledText("Bytes read", bytesRead),
    truncated ? labelledText("Truncated", "yes") : "",
    labelledText("Content", contentPreview),
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: contentPreview ? "output" : "data" };
}

function formatMemorySearchToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const results = arrayRecords(nestedValue(output, ["results"]));
  const query = nestedValue(output, ["query"]) ?? nestedValue(input, ["query"]);
  const resultLines = results.slice(0, 15).map((item, index) => {
    const sourceRef = nestedValue(item, ["source_ref"]);
    const preview = nestedValue(item, ["preview"]);
    const score = nestedValue(item, ["score"]);
    return labelledText(
      `${index + 1}. ${sourceRef ?? "Unknown source"}`,
      [typeof score === "string" ? `score: ${score}` : undefined, typeof preview === "string" ? truncateInspectorText(preview, 300) : undefined]
        .filter(Boolean)
        .join("\n"),
    );
  });
  const lines = [
    labelledText("Query", query),
    labelledText("Results", `${results.length} found`),
    ...resultLines,
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: results.length ? "output" : "data" };
}

function formatMemoryGetToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const sourceRef = nestedValue(output, ["source_ref"]) ?? nestedValue(input, ["source_ref"]);
  const content = nestedText(output, ["content"]);
  const contentPreview = typeof content === "string" && content.trim() ? truncateInspectorText(content, 2000) : undefined;
  const truncated = nestedValue(output, ["truncated"]);
  const lines = [
    labelledText("Source ref", sourceRef),
    truncated ? labelledText("Truncated", "yes") : "",
    labelledText("Content", contentPreview),
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: contentPreview ? "output" : "data" };
}

function truncateInspectorText(value: string, maxChars: number): string {
  if (value.length <= maxChars) return value;
  const truncated = value.slice(0, maxChars - 1);
  const lastNewline = truncated.lastIndexOf("\n");
  const cutPoint = lastNewline > maxChars * 0.6 ? lastNewline : truncated.length;
  return `${truncated.slice(0, cutPoint)}…`;
}

function formatWorkItemToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = workItemToolResultRecord(output, record.tool_name) ?? (isRecord(output) ? output : undefined);
  if (record.tool_name === "ListWorkItems") {
    const items = arrayRecords(result?.work_items ?? result?.items);
    const itemLines = items.map(formatWorkItemRecord).filter(Boolean);
    const lines = [
      labelledText("Filter", nestedValue(record.input, ["filter"]) ?? result?.filter),
      labelledText("Work items", itemLines.join("\n")),
      labelledText("Total", result?.total ?? result?.total_open ?? items.length),
      labelledText("Returned", result?.returned ?? items.length),
      labelledText("Result", record.summary || result?.summary_text),
      labelledText("Error", record.error),
    ].filter(Boolean);
    return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
  }

  const workItem = isRecord(result?.work_item) ? result.work_item : result;
  const planArtifact = isRecord(workItem?.plan_artifact) ? workItem.plan_artifact : undefined;
  const lines = [
    labelledText("Objective", nestedValue(workItem, ["objective", "objective_preview"]) ?? nestedValue(record.input, ["objective"])),
    labelledText("Work item", nestedValue(workItem, ["id", "work_item_id"]) ?? nestedValue(record.input, ["work_item_id"])),
    labelledText("State", compactMeta([textField(workItem?.lifecycle), textField(workItem?.state), textField(workItem?.plan_status), textField(workItem?.readiness)])),
    labelledText("Focus", truthyText(workItem?.current) || truthyText(workItem?.current_focus)),
    labelledText("Plan", nestedValue(planArtifact, ["path"]) ?? nestedValue(workItem, ["plan_path"])),
    labelledText("Todo", formatTodoItems(arrayRecords(workItem?.todo_list))),
    labelledText("Result", record.summary || result?.summary_text),
    labelledText("Error", record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatBatchToolOutput(input: unknown, output: unknown): string[] {
  if (!isRecord(output) || !Array.isArray(output.items)) return [];
  const inputItems = isRecord(input) && Array.isArray(input.items) ? input.items : [];

  return output.items
    .map((item, index) => {
      if (!isRecord(item)) return "";
      const result = isRecord(item.result) ? item.result : item;
      const command = textField(item.cmd) || commandText(inputItems[index]);
      const lines = [
        labelledText("Command", command),
        labelledText("Stdout", nestedText(result, ["stdout", "stdout_preview", "output", "output_preview", "combined_output_preview"])),
        labelledText("Stderr", nestedText(result, ["stderr", "stderr_preview"])),
        labelledText("Result", nestedText(result, ["summary", "summary_text", "result_summary", "result_summary_preview"])),
        labelledText("Error", nestedValue(result, ["error"])),
        labelledText("Exit", nestedValue(result, ["exit_status", "status", "disposition"])),
      ].filter(Boolean);
      return lines.length ? `Batch item ${item.index ?? index + 1}:\n${lines.join("\n\n")}` : "";
    })
    .filter(Boolean);
}

function formatWorkItemRecord(item: Record<string, unknown>): string {
  const status = compactMeta([
    textField(item.lifecycle) || textField(item.state) || textField(item.status),
    textField(item.plan_status),
    textField(item.readiness),
    truthyText(item.current) || truthyText(item.current_focus),
  ]);
  return compactMeta([
    textField(item.objective) || textField(item.objective_preview) || textField(item.title),
    status,
    textField(item.id) || textField(item.work_item_id),
  ]);
}

function formatTaskRecord(task: Record<string, unknown>): string {
  const command = isRecord(task.command) ? commandText(task.command) || textField(task.command.cmd_preview) : "";
  const retrieval = isRecord(task.retrieval) ? textField(task.retrieval.status) || textField(task.retrieval.output) : "";
  return compactMeta([
    textField(task.summary) || command || textField(task.kind),
    textField(task.status),
    textField(task.kind),
    textField(task.task_id) || textField(task.id),
    retrieval,
  ]);
}

function formatTodoItems(items: Record<string, unknown>[]): string {
  return items
    .map((item) => compactMeta([textField(item.state), textField(item.text) || textField(item.title)]))
    .filter(Boolean)
    .join("\n");
}

function labelledText(label: string, value: unknown): string {
  const text = textField(value) || scalarText(value);
  return text ? `${label}:\n${text}` : "";
}

function commandText(input: unknown): string {
  const value = nestedValue(input, ["cmd", "command", "cmd_preview"]);
  if (typeof value === "string") return value;
  return "";
}

function nestedText(value: unknown, keys: string[]): string {
  const found = nestedValue(value, keys);
  return textField(found) || scalarText(found);
}

function nestedValue(value: unknown, keys: string[]): unknown {
  if (!isRecord(value)) return undefined;
  for (const key of keys) {
    const field = value[key];
    if (field != null && (!isBlankString(field))) return field;
  }
  return undefined;
}

function asResultRecord(output: unknown, key: string): Record<string, unknown> | undefined {
  if (!isRecord(output)) return undefined;
  return (isRecord(output[key]) ? output[key] : output) as Record<string, unknown>;
}

function arrayRecords(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value) ? value.filter(isRecord) : [];
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function patchInputText(input: unknown): string {
  if (typeof input === "string") return input;
  if (!isRecord(input)) return "";
  return textField(input.patch) || textField(input.input) || textField(input.freeform);
}

function isWorkItemTool(toolName: string | undefined): boolean {
  return Boolean(toolName && ["ListWorkItems", "GetWorkItem", "CreateWorkItem", "UpdateWorkItem", "PickWorkItem", "CompleteWorkItem"].includes(toolName));
}

function workItemToolResultRecord(output: unknown, toolName: string | undefined): Record<string, unknown> | undefined {
  if (!isRecord(output)) return undefined;
  const resultKeyByTool: Record<string, string> = {
    ListWorkItems: "list_work_items_result",
    GetWorkItem: "get_work_item_result",
    CreateWorkItem: "create_work_item_result",
    UpdateWorkItem: "update_work_item_result",
    PickWorkItem: "pick_work_item_result",
    CompleteWorkItem: "complete_work_item_result",
  };
  const resultKey = toolName ? resultKeyByTool[toolName] : undefined;
  if (resultKey && isRecord(output[resultKey])) return output[resultKey] as Record<string, unknown>;
  if (isRecord(output.result)) return output.result;
  return undefined;
}

function scalarText(value: unknown): string {
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return "";
}

function truthyText(value: unknown): string {
  return value === true ? "current" : "";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value != null && !Array.isArray(value);
}

function isBlankString(value: unknown): boolean {
  return typeof value === "string" && value.trim() === "";
}

function TaskOutputDetail({ output }: { output: RuntimeTaskOutputResult }) {
  const text = taskOutputText(output) || formatInspectorJson(output);
  return (
    <section className="context-card inspector-card inspector-detail output">
      <div className="context-head">
        <span className="eyebrow">Task output</span>
        <strong>{compactMeta([output.task?.status ?? output.status, output.retrieval_status])}</strong>
      </div>
      <pre>{text}</pre>
    </section>
  );
}

function taskOutputText(output: RuntimeTaskOutputResult): string {
  return [
    textField(output.task?.result_summary),
    textField(output.task?.output_preview),
    textField(output.output),
    textField(output.stdout),
    textField(output.stderr),
  ]
    .filter(Boolean)
    .join("\n\n");
}

function textField(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function formatInspectorJson(value: unknown): string {
  if (value == null) return "";
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export function ActivityInspectorPanel({ activity, detailState }: { activity: AgentTimelineActivity; detailState?: InspectorActivityDetailState }) {
  const detail = activity.detail;
  const rawEventText = formatInspectorJson(activity.rawEvent);
  const hydratedDetail = hasHydratedDetail(detailState);
  const showTimelineDetail = Boolean(detail && !hydratedDetail);
  const structuredDetail = showTimelineDetail || Boolean(detail) || hydratedDetail;

  return (
    <div className="inspector-stack">
      <section className="context-card inspector-card">
        <div className="context-head">
          <span className="eyebrow">Timeline activity</span>
          <StatusBadge className="state-chip" kind="connection" value={activity.kind} />
        </div>
        <h2>{activity.body || activity.label}</h2>
        <dl className="inspector-facts">
          <div>
            <dt>Tool</dt>
            <dd>{activity.label}</dd>
          </div>
          <div>
            <dt>Meta</dt>
            <dd>{activity.meta || "—"}</dd>
          </div>
          <div>
            <dt>Time</dt>
            <dd>{formatInspectorTime(activity.timestamp)}</dd>
          </div>
          <div>
            <dt>Sources</dt>
            <dd>{activity.sourceIds.length ? activity.sourceIds.join(", ") : "—"}</dd>
          </div>
        </dl>
      </section>

      {showTimelineDetail && detail ? (
        <section className={`context-card inspector-card inspector-detail ${detail.tone ?? "data"}`}>
          <div className="context-head">
            <span className="eyebrow">{detailLabel(detail.tone)}</span>
            <strong>{detail.label}</strong>
          </div>
          <pre>{detail.text}</pre>
        </section>
      ) : !hydratedDetail ? (
        <EmptyState
          className="inspector-empty"
          icon="⌁"
          title="No structured detail"
          description="This activity has no projected detail yet. Use the raw event below for the source payload."
        />
      ) : null}

      <HydratedActivityDetails detailState={detailState} />

      {rawEventText ? (
        <details className="context-card inspector-card inspector-raw-detail" open={!structuredDetail}>
          <summary>Raw event</summary>
          <pre>{rawEventText}</pre>
        </details>
      ) : null}
    </div>
  );
}

export function activityInspectorTitle(activity: AgentTimelineActivity): string {
  if (activity.detail?.tone === "command") return "Command";
  if (activity.detail?.tone === "diff") return "Patch";
  if (activity.detail?.tone === "output") return "Output";
  return activity.label || "Activity";
}

function detailLabel(tone?: AgentTimelineItemDetail["tone"]): string {
  if (tone === "command") return "Command";
  if (tone === "diff") return "Patch diff";
  if (tone === "output") return "Output";
  return "Result";
}

function formatInspectorTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value || "—";
  return date.toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
}

function compactMeta(parts: Array<string | undefined>): string {
  return parts.filter(Boolean).join(" · ") || "—";
}
