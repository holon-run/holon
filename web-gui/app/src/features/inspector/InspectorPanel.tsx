import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type { AgentSummary, AgentTimelineActivity, AgentTimelineItemDetail, InspectorSelection } from "../../runtime/types";

interface InspectorPanelProps {
  agent: AgentSummary;
  selection?: InspectorSelection;
  open: boolean;
  onClearSelection: () => void;
  onClose: () => void;
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
        {selection?.kind === "activity" ? <ActivityDetails activity={selection.activity} /> : <SessionOverview agent={agent} />}
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

function ActivityDetails({ activity }: { activity: AgentTimelineActivity }) {
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
