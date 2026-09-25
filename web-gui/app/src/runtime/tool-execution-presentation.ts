import { projectToolExecution } from "./session-reducer-core";
import type { RuntimeToolExecutionRecord } from "./types";

export interface AdvisoryDecisionToolPresentation {
  question?: string;
  choice?: string;
  confidence?: number;
  abstain: boolean;
  reason?: string;
  outcome?: string;
  provider?: string;
  model?: string;
  latencyMs?: number;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : undefined;
}

function stringValue(record: Record<string, unknown> | undefined, key: string): string | undefined {
  const value = record?.[key];
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : undefined;
}

function numberValue(record: Record<string, unknown> | undefined, key: string): number | undefined {
  const value = record?.[key];
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function advisoryDecisionPresentation(
  input: Record<string, unknown> | undefined,
  output: Record<string, unknown> | undefined,
): AdvisoryDecisionToolPresentation {
  const outcome = stringValue(output, "outcome");
  const reason = stringValue(output, "reason")?.replace(/^[a-z_]+:\s*/i, "");
  return {
    question: stringValue(input, "question"),
    choice: stringValue(output, "choice"),
    confidence: numberValue(output, "confidence"),
    abstain: output?.abstain === true || outcome === "abstain",
    reason,
    outcome,
    provider: stringValue(output, "provider"),
    model: stringValue(output, "model"),
    latencyMs: numberValue(output, "latency_ms"),
  };
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
  const advisoryDecision = record.tool_name === "AdvisoryDecision"
    ? advisoryDecisionPresentation(input, output)
    : undefined;
  return {
    text: text.length > 240 ? `${text.slice(0, 239)}…` : text,
    toolName: record.tool_name ?? "Tool",
    status: record.status ?? "unknown",
    durationMs: projection.executionMeta?.durationMs,
    command,
    ...(advisoryDecision ? { advisoryDecision } : {}),
  };
}
