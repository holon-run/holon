import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type {
  AgentSummary,
  AgentTimelineActivity,
  AgentTimelineItemDetail,
  InspectorActivityDetailState,
  InspectorSelection,
  RuntimeTaskOutputResult,
  RuntimeToolExecutionRecord,
} from "../../runtime/types";

interface InspectorPanelProps {
  agent: AgentSummary;
  selection?: InspectorSelection;
  open: boolean;
  onClearSelection: () => void;
  onClose: () => void;
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

export function InspectorPanel({ agent, selection, open, onClearSelection, onClose }: InspectorPanelProps) {
  const title = selection?.kind === "activity" ? activityInspectorTitle(selection.activity) : "Session overview";

  return (
    <aside className="side-panel" aria-label="Object side panel" hidden={!open}>
      <div className="panel-header">
        <div>
          <span className="eyebrow">Inspector</span>
          <strong>{title}</strong>
        </div>
        <div className="panel-actions">
          {selection ? (
            <button type="button" aria-label="Show session overview" onClick={onClearSelection}>
              Overview
            </button>
          ) : null}
          <button type="button" aria-label="Close side panel" onClick={onClose}>
            ×
          </button>
        </div>
      </div>
      <div className="panel-body">
        {selection?.kind === "activity" ? (
          <ActivityDetails activity={selection.activity} detailState={selection.detailState} />
        ) : (
          <SessionOverview agent={agent} />
        )}
      </div>
    </aside>
  );
}

function SessionOverview({ agent }: { agent: AgentSummary }) {
  const workspace = agent.workspaceSummary;
  const openWorkItems = agent.workItems ?? (agent.currentWork ? [agent.currentWork] : []);
  const currentWorkItems = openWorkItems.filter((item) => item.current);
  const otherWorkItems = openWorkItems.filter((item) => !item.current);

  return (
    <div className="inspector-stack">
      <section className="context-card inspector-card">
        <div className="context-head">
          <span className="eyebrow">Agent</span>
          <StatusBadge className="state-chip" kind="agent" value={agent.posture || agent.lifecycle} />
        </div>
        <h2>{agent.id}</h2>
        <dl className="inspector-facts">
          <div>
            <dt>Lifecycle</dt>
            <dd>{agent.lifecycle}</dd>
          </div>
          <div>
            <dt>Model</dt>
            <dd>{agent.model}</dd>
          </div>
          <div>
            <dt>Focus</dt>
            <dd>{agent.focusSummary}</dd>
          </div>
        </dl>
      </section>

      <section className="context-card inspector-card">
        <div className="context-head">
          <span className="eyebrow">Workspace</span>
          <StatusBadge className="state-chip" kind="connection" value={agent.workspace === "not bound" ? "unbound" : "active"} />
        </div>
        <h2>{workspace?.name ?? agent.workspace}</h2>
        <dl className="inspector-facts">
          <div>
            <dt>Name</dt>
            <dd>{workspace?.name ?? (agent.workspace === "not bound" ? "No active workspace" : agent.workspace)}</dd>
          </div>
          <div>
            <dt>Anchor</dt>
            <dd>{workspace?.anchor ?? "—"}</dd>
          </div>
          <div>
            <dt>ID</dt>
            <dd>{workspace?.id ?? "—"}</dd>
          </div>
        </dl>
      </section>

      <section className="context-card inspector-card">
        <div className="context-head">
          <span className="eyebrow">{workspace?.worktree ? "Worktree / Execution" : "Execution"}</span>
          <StatusBadge className="state-chip" kind="connection" value={workspace?.worktree ? "worktree" : "root"} />
        </div>
        <h2>{workspace?.worktree?.branch ?? "Current root"}</h2>
        <dl className="inspector-facts">
          {workspace?.worktree ? (
            <>
              <div>
                <dt>Branch</dt>
                <dd>{workspace.worktree.branch ?? "—"}</dd>
              </div>
              <div>
                <dt>Worktree</dt>
                <dd>{workspace.worktree.path ?? "—"}</dd>
              </div>
            </>
          ) : null}
          <div>
            <dt>Root</dt>
            <dd>{workspace?.executionRoot ?? workspace?.anchor ?? "—"}</dd>
          </div>
          <div>
            <dt>Cwd</dt>
            <dd>{workspace?.cwd ?? "—"}</dd>
          </div>
        </dl>
      </section>

      <section className="context-card inspector-card">
        <div className="context-head">
          <span className="eyebrow">Tasks</span>
          <StatusBadge className="state-chip" kind="connection" value={agent.activeTaskCount ? "active" : "idle"} />
        </div>
        <h2>{agent.activeTaskCount} active</h2>
        {agent.tasks?.length ? (
          <ul className="inspector-list">
            {agent.tasks.map((task) => (
              <li key={task.id}>
                <div className="inspector-list-head">
                  <strong>{task.summary}</strong>
                  <StatusBadge className="state-chip" kind="connection" value={task.status} />
                </div>
                <small>{compactMeta([task.kind, task.command, task.workdir])}</small>
                <code>{task.id}</code>
              </li>
            ))}
          </ul>
        ) : (
          <dl className="inspector-facts">
            <div>
              <dt>Queued</dt>
              <dd>{agent.pending}</dd>
            </div>
            <div>
              <dt>Waiting</dt>
              <dd>{agent.waitingCount}</dd>
            </div>
            <div>
              <dt>Attention</dt>
              <dd>{agent.attention}</dd>
            </div>
          </dl>
        )}
      </section>

      {openWorkItems.length ? (
        <section className="context-card current-work inspector-card">
          <div className="context-head">
            <span className="eyebrow">Open work items</span>
            <StatusBadge className="state-chip" kind="work" value={`${openWorkItems.length} open`} />
          </div>
          {currentWorkItems.map((workItem) => (
            <WorkItemCard key={workItem.id} workItem={workItem} featured />
          ))}
          {otherWorkItems.length ? (
            <details className="inspector-details-list">
              <summary>{otherWorkItems.length} other open</summary>
              <div className="inspector-nested-stack">
                {otherWorkItems.map((workItem) => (
                  <WorkItemCard key={workItem.id} workItem={workItem} />
                ))}
              </div>
            </details>
          ) : null}
        </section>
      ) : (
        <EmptyState
          className="inspector-empty"
          icon="◎"
          title="No current work item"
          description="Select a timeline activity to inspect tool output, or continue the conversation from the main pane."
        />
      )}
    </div>
  );
}

function WorkItemCard({ workItem, featured = false }: { workItem: NonNullable<AgentSummary["workItems"]>[number]; featured?: boolean }) {
  return (
    <article className={`inspector-list-item${featured ? " featured" : ""}`}>
      <div className="inspector-list-head">
        <strong>{workItem.objective}</strong>
        <StatusBadge className="state-chip" kind="work" value={workItem.state} />
      </div>
      <small>{compactMeta([workItem.current ? "current" : undefined, workItem.planStatus])}</small>
      <code>{workItem.id}</code>
    </article>
  );
}

function ActivityDetails({ activity, detailState }: { activity: AgentTimelineActivity; detailState?: InspectorActivityDetailState }) {
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

function activityInspectorTitle(activity: AgentTimelineActivity): string {
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
