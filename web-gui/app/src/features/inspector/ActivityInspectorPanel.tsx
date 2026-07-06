import { Unplug } from "lucide-react";
import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import { useTranslation } from "react-i18next";
import i18next from "i18next";
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
          <span className="eyebrow">{i18next.t("inspector.fullDetail")}</span>
          <strong>{i18next.t("inspector.loading")}</strong>
        </div>
        <pre>{i18next.t("inspector.loadingDetail")}</pre>
      </section>
    );
  }
  if (detailState.error) {
    return (
      <section className="context-card inspector-card inspector-detail data">
        <div className="context-head">
          <span className="eyebrow">{i18next.t("inspector.fullDetail")}</span>
          <strong>{i18next.t("inspector.unavailable")}</strong>
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
          <span className="eyebrow">{i18next.t("inspector.toolExecution")}</span>
          <strong>{compactMeta([record.tool_name, record.status])}</strong>
        </div>
        <pre>{detail.text}</pre>
      </section>
      {rawText ? (
        <details className="context-card inspector-card inspector-raw-detail">
          <summary>{i18next.t("inspector.rawJson")}</summary>
          <pre>{rawText}</pre>
        </details>
      ) : null}
    </>
  );
}

export function formatToolExecutionDetail(record: RuntimeToolExecutionRecord): {
  text: string;
  tone: "output" | "data";
} {
  const knownToolDetail = formatKnownToolExecutionDetail(record);
  if (knownToolDetail) return knownToolDetail;

  const output = unwrapToolOutput(record.output ?? record.result);
  const batchItemSections = formatBatchToolOutput(record.input, output);
  const lines = [
    labelledText(i18next.t("inspector.summary"), record.summary),
    batchItemSections.length ? "" : labelledText(i18next.t("inspector.command"), commandText(record.input)),
    labelledText(i18next.t("inspector.stdout"), nestedText(output, ["stdout", "stdout_preview", "output", "output_preview", "combined_output_preview"])),
    labelledText(i18next.t("inspector.stderr"), nestedText(output, ["stderr", "stderr_preview"])),
    labelledText(i18next.t("inspector.initialOutput"), nestedText(output, ["initial_output_preview"])),
    labelledText(i18next.t("inspector.result"), nestedText(output, ["summary", "summary_text", "result_summary", "result_summary_preview"])),
    labelledText(i18next.t("inspector.error"), record.error ?? nestedValue(output, ["error"])),
    labelledText(i18next.t("inspector.exit"), nestedValue(output, ["exit_status", "status", "disposition"])),
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
    labelledText(i18next.t("inspector.summary"), record.summary || result?.summary_text),
    labelledText(i18next.t("inspector.changedFiles"), fileSummaries.length ? fileSummaries.join("\n") : changedPaths.join("\n")),
    labelledText(i18next.t("inspector.diagnostics"), diagnostics.join("\n")),
    labelledText(patchText ? i18next.t("inspector.patch") : i18next.t("inspector.patchPreview"), patchText || diffPreview),
    labelledText(i18next.t("inspector.result"), result?.summary_text),
    labelledText(i18next.t("inspector.error"), record.error),
  ].filter(Boolean);

  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatExecCommandToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const lines = [
    labelledText(i18next.t("inspector.command"), commandText(record.input) || textField(record.cmd_preview)),
    labelledText(i18next.t("inspector.stdout"), nestedText(output, ["stdout", "stdout_preview", "output", "output_preview", "combined_output_preview"])),
    labelledText(i18next.t("inspector.stderr"), nestedText(output, ["stderr", "stderr_preview"])),
    labelledText(i18next.t("inspector.initialOutput"), nestedText(output, ["initial_output_preview"])),
    labelledText(i18next.t("inspector.result"), nestedText(output, ["summary", "summary_text", "result_summary", "result_summary_preview"]) || record.summary),
    labelledText(i18next.t("inspector.error"), record.error ?? nestedValue(output, ["error"])),
    labelledText(i18next.t("inspector.exit"), nestedValue(output, ["exit_status", "status", "disposition"])),
  ].filter(Boolean);

  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatExecCommandBatchToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const batchItemSections = formatBatchToolOutput(record.input, output);
  const lines = [
    labelledText(i18next.t("inspector.summary"), record.summary),
    labelledText(i18next.t("inspector.result"), nestedText(output, ["summary", "summary_text", "result_summary", "result_summary_preview"])),
    labelledText(i18next.t("inspector.error"), record.error ?? nestedValue(output, ["error"])),
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
    labelledText(i18next.t("inspector.summary"), record.summary),
    labelledText(i18next.t("inspector.tasks"), taskLines.join("\n")),
    labelledText(i18next.t("inspector.totalActive"), total),
    labelledText(i18next.t("inspector.returned"), returned),
    labelledText(i18next.t("inspector.error"), record.error),
  ].filter(Boolean);

  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatTaskOutputToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "task_output_result") ?? (isRecord(output) ? output : undefined);
  const task = isRecord(result?.task) ? result.task : isRecord(result?.task_record) ? result.task_record : undefined;
  const taskId = nestedValue(task, ["task_id", "id"]) ?? nestedValue(record.input, ["task_id"]);
  const status = nestedValue(task, ["status"]) ?? nestedValue(result, ["status"]);
  const retrievalStatus = nestedValue(result, ["retrieval_status"]);
  const exitStatus = nestedValue(task, ["exit_status"]) ?? nestedValue(result, ["exit_status"]);
  const outputText =
    nestedText(task, ["output_preview", "output", "stdout", "stderr", "combined_output_preview"]) ??
    nestedText(result, ["output_preview", "output", "stdout", "stderr", "combined_output_preview"]);
  const truncated = nestedValue(task, ["output_truncated"]) === true || nestedValue(result, ["output_truncated", "truncated"]) === true;
  const lines = [
    labelledText(i18next.t("inspector.taskId"), taskId),
    labelledText(i18next.t("inspector.status"), status),
    labelledText(i18next.t("inspector.retrieval"), retrievalStatus),
    labelledText(i18next.t("inspector.exit"), exitStatus),
    labelledText(i18next.t("inspector.summary"), nestedValue(task, ["summary"]) ?? nestedValue(result, ["summary", "result_summary"])),
    outputText ? labelledText(truncated ? i18next.t("inspector.outputTruncated") : i18next.t("inspector.output"), outputText) : "",
    labelledText(i18next.t("inspector.error"), record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatTaskStatusToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "task_status_result") ?? (isRecord(output) ? output : undefined);
  const taskId = nestedValue(result, ["task_id", "id"]) ?? nestedValue(record.input, ["task_id"]);
  const lines = [
    labelledText(i18next.t("inspector.taskId"), taskId),
    labelledText(i18next.t("inspector.status"), nestedValue(result, ["status"])),
    labelledText(i18next.t("inspector.kind"), nestedValue(result, ["kind"])),
    labelledText(i18next.t("inspector.summary"), nestedValue(result, ["summary"])),
    labelledText(i18next.t("inspector.error"), record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatTaskStopToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "task_stop_result") ?? (isRecord(output) ? output : undefined);
  const taskId = nestedValue(result, ["task_id", "id"]) ?? nestedValue(record.input, ["task_id"]);
  const lines = [
    labelledText(i18next.t("inspector.taskId"), taskId),
    labelledText(i18next.t("inspector.status"), nestedValue(result, ["status"])),
    labelledText(i18next.t("inspector.summary"), record.summary),
    labelledText(i18next.t("inspector.error"), record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatTaskInputToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "task_input_result") ?? (isRecord(output) ? output : undefined);
  const taskId = nestedValue(result, ["task_id", "id"]) ?? nestedValue(record.input, ["task_id"]);
  const input = nestedValue(record.input, ["input"]) ?? nestedValue(result, ["input"]);
  const lines = [
    labelledText(i18next.t("inspector.taskId"), taskId),
    labelledText(i18next.t("inspector.input"), input),
    labelledText(i18next.t("inspector.status"), nestedValue(result, ["status"])),
    labelledText(i18next.t("inspector.summary"), record.summary),
    labelledText(i18next.t("inspector.error"), record.error),
  ].filter(Boolean);
  return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
}

function formatWaitForToolExecution(record: RuntimeToolExecutionRecord): { text: string; tone: "output" | "data" } {
  const input = isRecord(record.input) ? record.input : record;
  const lines = [
    labelledText(i18next.t("inspector.reason"), input.reason),
    labelledText(i18next.t("inspector.wake"), input.wake),
    labelledText(i18next.t("inspector.resource"), input.resource),
    labelledText(i18next.t("inspector.recheckAfter"), input.recheck_after_ms),
    labelledText(i18next.t("inspector.result"), record.summary),
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
    labelledText(i18next.t("inspector.path"), nestedValue(record.input, ["path", "image_path"]) ?? nestedValue(result, ["path", "image_path"])),
    labelledText(i18next.t("inspector.dimensions"), width != null && height != null ? `${width}×${height}` : ""),
    labelledText(i18next.t("inspector.observation"), nestedText(result, ["visual_observation", "observation", "text_preview"])),
    labelledText(i18next.t("inspector.result"), result?.summary_text || record.summary),
    labelledText(i18next.t("inspector.error"), record.error),
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
      `${index + 1}. ${title ?? i18next.t("inspector.untitled")}`,
      [url, source ? `(${source})` : undefined, publishedAt ? String(publishedAt) : undefined, typeof snippet === "string" ? truncateInspectorText(snippet, 300) : undefined]
        .filter(Boolean)
        .join("\n"),
    );
  });
  const lines = [
    labelledText(i18next.t("inspector.query"), query),
    labelledText(i18next.t("inspector.provider"), provider),
    labelledText(i18next.t("inspector.mode"), mode),
    labelledText(i18next.t("inspector.results"), i18next.t("inspector.resultsCount", { count: results.length })),
    ...resultLines,
    labelledText(i18next.t("inspector.error"), record.error),
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
    labelledText(i18next.t("inspector.url"), url),
    finalUrl && finalUrl !== url ? labelledText(i18next.t("inspector.finalUrl"), finalUrl) : "",
    labelledText(i18next.t("inspector.status"), status),
    labelledText(i18next.t("inspector.contentType"), contentType),
    labelledText(i18next.t("inspector.bytesRead"), bytesRead),
    truncated ? labelledText(i18next.t("inspector.truncated"), "yes") : "",
    labelledText(i18next.t("inspector.content"), contentPreview),
    labelledText(i18next.t("inspector.error"), record.error),
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
      `${index + 1}. ${sourceRef ?? i18next.t("inspector.unknownSource")}`,
      [typeof score === "string" ? `score: ${score}` : undefined, typeof preview === "string" ? truncateInspectorText(preview, 300) : undefined]
        .filter(Boolean)
        .join("\n"),
    );
  });
  const lines = [
    labelledText(i18next.t("inspector.query"), query),
    labelledText(i18next.t("inspector.results"), i18next.t("inspector.resultsCount", { count: results.length })),
    ...resultLines,
    labelledText(i18next.t("inspector.error"), record.error),
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
    labelledText(i18next.t("inspector.sourceRef"), sourceRef),
    truncated ? labelledText(i18next.t("inspector.truncated"), "yes") : "",
    labelledText(i18next.t("inspector.content"), contentPreview),
    labelledText(i18next.t("inspector.error"), record.error),
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
      labelledText(i18next.t("inspector.filter"), nestedValue(record.input, ["filter"]) ?? result?.filter),
      labelledText(i18next.t("inspector.workItems"), itemLines.join("\n")),
      labelledText(i18next.t("inspector.total"), result?.total ?? result?.total_open ?? items.length),
      labelledText(i18next.t("inspector.returned"), result?.returned ?? items.length),
      labelledText(i18next.t("inspector.result"), record.summary || result?.summary_text),
      labelledText(i18next.t("inspector.error"), record.error),
    ].filter(Boolean);
    return { text: lines.join("\n\n") || formatInspectorJson(record), tone: lines.length ? "output" : "data" };
  }

  const workItem = isRecord(result?.work_item) ? result.work_item : result;
  const planArtifact = isRecord(workItem?.plan_artifact) ? workItem.plan_artifact : undefined;
  const lines = [
    labelledText(i18next.t("inspector.objective"), nestedValue(workItem, ["objective", "objective_preview"]) ?? nestedValue(record.input, ["objective"])),
    labelledText(i18next.t("inspector.workItem"), nestedValue(workItem, ["id", "work_item_id"]) ?? nestedValue(record.input, ["work_item_id"])),
    labelledText(i18next.t("inspector.state"), compactMeta([textField(workItem?.lifecycle), textField(workItem?.state), textField(workItem?.plan_status), textField(workItem?.readiness)])),
    labelledText(i18next.t("inspector.focus"), truthyText(workItem?.current) || truthyText(workItem?.current_focus)),
    labelledText(i18next.t("inspector.plan"), nestedValue(planArtifact, ["path"]) ?? nestedValue(workItem, ["plan_path"])),
    labelledText(i18next.t("inspector.todo"), formatTodoItems(arrayRecords(workItem?.todo_list))),
    labelledText(i18next.t("inspector.result"), record.summary || result?.summary_text),
    labelledText(i18next.t("inspector.error"), record.error),
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
        labelledText(i18next.t("inspector.command"), command),
        labelledText(i18next.t("inspector.stdout"), nestedText(result, ["stdout", "stdout_preview", "output", "output_preview", "combined_output_preview"])),
        labelledText(i18next.t("inspector.stderr"), nestedText(result, ["stderr", "stderr_preview"])),
        labelledText(i18next.t("inspector.result"), nestedText(result, ["summary", "summary_text", "result_summary", "result_summary_preview"])),
        labelledText(i18next.t("inspector.error"), nestedValue(result, ["error"])),
        labelledText(i18next.t("inspector.exit"), nestedValue(result, ["exit_status", "status", "disposition"])),
      ].filter(Boolean);
      return lines.length ? `${i18next.t("inspector.batchItem")} ${item.index ?? index + 1}:\n${lines.join("\n\n")}` : "";
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
  return value === true ? i18next.t("inspector.current") : "";
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
        <span className="eyebrow">{i18next.t("inspector.taskOutput")}</span>
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
  const { t } = useTranslation();
  const detail = activity.detail;
  const rawEventText = formatInspectorJson(activity.rawEvent);
  const hydratedDetail = hasHydratedDetail(detailState);
  const showTimelineDetail = Boolean(detail && !hydratedDetail);
  const structuredDetail = showTimelineDetail || Boolean(detail) || hydratedDetail;

  return (
    <div className="inspector-stack">
      <section className="context-card inspector-card">
        <div className="context-head">
          <span className="eyebrow">{t("inspector.timelineActivity")}</span>
          <StatusBadge className="state-chip" kind="connection" value={activity.kind} />
        </div>
        <h2>{activity.body || activity.label}</h2>
        <dl className="inspector-facts">
          <div>
            <dt>{t("inspector.tool")}</dt>
            <dd>{activity.label}</dd>
          </div>
          <div>
            <dt>{t("inspector.meta")}</dt>
            <dd>{activity.meta || "—"}</dd>
          </div>
          <div>
            <dt>{t("inspector.time")}</dt>
            <dd>{formatInspectorTime(activity.timestamp)}</dd>
          </div>
          <div>
            <dt>{t("inspector.sources")}</dt>
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
          icon={<Unplug size={20} />}
          title={t("inspector.noStructuredDetail")}
          description={t("inspector.noStructuredDetailDesc")}
        />
      ) : null}

      <HydratedActivityDetails detailState={detailState} />

      {rawEventText ? (
        <details className="context-card inspector-card inspector-raw-detail" open={!structuredDetail}>
          <summary>{t("inspector.rawEvent")}</summary>
          <pre>{rawEventText}</pre>
        </details>
      ) : null}
    </div>
  );
}

export function activityInspectorTitle(activity: AgentTimelineActivity): string {
  if (activity.detail?.tone === "command") return i18next.t("inspector.command");
  if (activity.detail?.tone === "diff") return i18next.t("inspector.patch");
  if (activity.detail?.tone === "output") return i18next.t("inspector.output");
  return activity.label || i18next.t("inspector.activity");
}

function detailLabel(tone?: AgentTimelineItemDetail["tone"]): string {
  if (tone === "command") return i18next.t("inspector.command");
  if (tone === "diff") return i18next.t("inspector.patchDiff");
  if (tone === "output") return i18next.t("inspector.output");
  return i18next.t("inspector.result");
}

function formatInspectorTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value || "—";
  return date.toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
}

function compactMeta(parts: Array<string | undefined>): string {
  return parts.filter(Boolean).join(" · ") || "—";
}
