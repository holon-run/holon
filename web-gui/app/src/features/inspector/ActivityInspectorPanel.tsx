import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type {
  AgentTimelineActivity,
  AgentTimelineItemDetail,
  InspectorActivityDetailState,
  RuntimeTaskOutputResult,
  RuntimeToolExecutionRecord,
} from "../../runtime/types";

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

function scalarText(value: unknown): string {
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return "";
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

      {detail ? (
        <section className={`context-card inspector-card inspector-detail ${detail.tone ?? "data"}`}>
          <div className="context-head">
            <span className="eyebrow">{detailLabel(detail.tone)}</span>
            <strong>{detail.label}</strong>
          </div>
          <pre>{detail.text}</pre>
        </section>
      ) : (
        <EmptyState
          className="inspector-empty"
          icon="⌁"
          title="No structured detail"
          description="This activity has no projected detail yet. Use the raw event below for the source payload."
        />
      )}

      <HydratedActivityDetails detailState={detailState} />

      {rawEventText ? (
        <section className="context-card inspector-card inspector-detail data">
          <div className="context-head">
            <span className="eyebrow">Raw event</span>
            <strong>Source payload</strong>
          </div>
          <pre>{rawEventText}</pre>
        </section>
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
  if (tone === "command") return "Command output";
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
