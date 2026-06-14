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
          <div>
            <dt>Tasks</dt>
            <dd>
              {agent.activeTaskCount} active · {agent.pending} queued · {agent.waitingCount} waiting
            </dd>
          </div>
        </dl>
      </section>

      {agent.currentWork ? (
        <section className="context-card current-work inspector-card">
          <div className="context-head">
            <span className="eyebrow">Current work item</span>
            <StatusBadge className="state-chip" kind="work" value={agent.currentWork.state} />
          </div>
          <h2>{agent.currentWork.objective}</h2>
          <code>{agent.currentWork.id}</code>
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

function ActivityDetails({ activity }: { activity: AgentTimelineActivity }) {
  const detail = activity.detail;

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
          description="This activity has no projected detail yet, so only its timeline summary is available."
        />
      )}
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
