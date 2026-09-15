import { projectToolExecution } from "./session-reducer-core";
import type { RuntimeToolExecutionRecord } from "./types";

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : undefined;
}

/** Reuse the timeline's tool-specific summaries with canonical tool records. */
export function toolExecutionPresentation(record: RuntimeToolExecutionRecord) {
  const input = asRecord(record.input);
  const rawOutput = asRecord(record.output ?? record.result);
  const output = asRecord(asRecord(rawOutput?.envelope)?.result) ?? rawOutput;
  const command = record.tool_name === "ExecCommand" || record.tool_name === "ExecCommandBatch";
  const payload = {
    ...record,
    input,
    output,
    result: output,
    // Older event projections kept these selected arguments at the top level.
    reason: input?.reason,
    objective: input?.objective,
    resource: input?.resource,
    task_id: input?.task_id,
    work_item_id: input?.work_item_id,
    ...(command ? {
      cmd: input?.cmd,
      exec_command_result: output?.exec_command_result ?? output,
      exec_command_batch_items: output?.items ?? input?.items,
    } : {}),
    ...(record.tool_name === "ApplyPatch" ? { apply_patch_result: output?.apply_patch_result ?? output } : {}),
  };
  const projection = projectToolExecution(
    record.status === "error" || record.status === "failed" ? "tool_execution_failed" : "tool_execution_completed",
    payload,
    { includeHiddenWorkItemMutations: true },
  )!;
  const target = [input?.query, input?.path, input?.url].find((value): value is string => typeof value === "string" && value.trim().length > 0);
  const generic = projection.body === record.summary || projection.body === record.tool_name;
  const text = (generic && target ? `${record.tool_name} · ${target}` : projection.body).trim();
  return {
    text: text.length > 240 ? `${text.slice(0, 239)}…` : text,
    toolName: record.tool_name ?? "Tool",
    status: record.status ?? "unknown",
    durationMs: projection.executionMeta?.durationMs,
    command,
  };
}
