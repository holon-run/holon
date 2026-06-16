import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type { AgentSummary } from "../../runtime/types";

export function AgentOverviewPanel({ agent }: { agent: AgentSummary }) {
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

function compactMeta(parts: Array<string | undefined>): string {
  return parts.filter(Boolean).join(" · ") || "—";
}

