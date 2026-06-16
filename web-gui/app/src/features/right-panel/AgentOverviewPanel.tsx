import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type { AgentSummary, WorkItemDetailState, WorkItemSummary } from "../../runtime/types";

interface AgentOverviewPanelProps {
  agent: AgentSummary;
  onLoadWorkItemDetail: (workItemId: string) => void;
  onOpenWorkItemDetail: (workItem: WorkItemSummary) => void;
}

export function AgentOverviewPanel({ agent, onLoadWorkItemDetail, onOpenWorkItemDetail }: AgentOverviewPanelProps) {
  const workspace = agent.workspaceSummary;
  const workItems = agent.workItems ?? (agent.currentWork ? [agent.currentWork] : []);
  const currentWorkItems = workItems.filter((item) => item.current);
  const openWorkItems = workItems.filter((item) => !item.current && item.state !== "completed");
  const completedWorkItems = workItems.filter((item) => item.state === "completed");
  const currentWorkLabel = currentWorkItems[0]?.objective ?? agent.currentWork?.objective ?? "No current work item";
  const workspaceName = workspace?.name ?? agent.workspace;
  const workspaceRoot = workspace?.cwd ?? workspace?.executionRoot ?? workspace?.worktree?.path ?? workspace?.anchor;
  const modeLabel = workspace?.worktree ? "Managed worktree" : workspace?.projectionKind;
  const showCwd = Boolean(workspace?.cwd && workspace.cwd !== workspace.executionRoot);
  const hasActiveTasks = agent.activeTaskCount > 0 || Boolean(agent.tasks?.length);

  const selectWorkItem = (workItem: WorkItemSummary) => {
    onOpenWorkItemDetail(workItem);
    onLoadWorkItemDetail(workItem.id);
  };

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
            <dt>Current work</dt>
            <dd>{currentWorkLabel}</dd>
          </div>
          <div>
            <dt>Scheduling</dt>
            <dd>{compactMeta([agent.posture, agent.postureReason])}</dd>
          </div>
        </dl>
      </section>

      <section className="context-card inspector-card">
        <div className="context-head">
          <span className="eyebrow">Workspace</span>
          <StatusBadge className="state-chip" kind="connection" value={agent.workspace === "not bound" ? "unbound" : "active"} />
        </div>
        <h2>{workspaceName}</h2>
        <dl className="inspector-facts">
          <div>
            <dt>Working directory</dt>
            <dd>{workspaceRoot ?? (agent.workspace === "not bound" ? "No active workspace" : "—")}</dd>
          </div>
          <div>
            <dt>Anchor</dt>
            <dd>{workspace?.anchor ?? "—"}</dd>
          </div>
          <div>
            <dt>Mode</dt>
            <dd>{compactMeta([modeLabel, workspace?.accessMode])}</dd>
          </div>
        </dl>
        {workspace ? (
          <details className="inspector-details-list workspace-technical-details">
            <summary>Technical details</summary>
            <dl className="inspector-facts">
              <div>
                <dt>Name</dt>
                <dd>{workspace.name}</dd>
              </div>
              <div>
                <dt>ID</dt>
                <dd>{workspace.id}</dd>
              </div>
              <div>
                <dt>Projection</dt>
                <dd>{compactMeta([workspace.projectionKind, workspace.accessMode])}</dd>
              </div>
              <div>
                <dt>Execution root</dt>
                <dd>{workspace.executionRoot ?? "—"}</dd>
              </div>
              {showCwd ? (
                <div>
                  <dt>Cwd</dt>
                  <dd>{workspace.cwd}</dd>
                </div>
              ) : null}
              {workspace.worktree ? (
                <>
                  <div>
                    <dt>Worktree</dt>
                    <dd>{workspace.worktree.path ?? "—"}</dd>
                  </div>
                  <div>
                    <dt>Original branch</dt>
                    <dd>{workspace.worktree.originalBranch ?? "—"}</dd>
                  </div>
                </>
              ) : null}
            </dl>
          </details>
        ) : null}
      </section>

      {hasActiveTasks ? (
        <section className="context-card inspector-card">
          <div className="context-head">
            <span className="eyebrow">Tasks</span>
            <StatusBadge className="state-chip" kind="connection" value="active" />
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
          ) : null}
        </section>
      ) : null}

      {workItems.length ? (
        <section className="context-card current-work inspector-card">
          <div className="context-head">
            <span className="eyebrow">Work items</span>
            <StatusBadge className="state-chip" kind="work" value={`${openWorkItems.length + currentWorkItems.length} open`} />
          </div>
          {currentWorkItems.map((workItem) => (
            <WorkItemCard key={workItem.id} workItem={workItem} featured onSelect={selectWorkItem} />
          ))}
          {openWorkItems.length ? (
            <div className="inspector-nested-stack">
              {openWorkItems.map((workItem) => (
                <WorkItemCard key={workItem.id} workItem={workItem} onSelect={selectWorkItem} />
              ))}
            </div>
          ) : null}
          {completedWorkItems.length ? (
            <details className="inspector-details-list">
              <summary>{completedWorkItems.length} completed</summary>
              <div className="inspector-nested-stack">
                {completedWorkItems.map((workItem) => (
                  <WorkItemCard key={workItem.id} workItem={workItem} onSelect={selectWorkItem} />
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

function WorkItemCard({
  workItem,
  featured = false,
  onSelect,
}: {
  workItem: WorkItemSummary;
  featured?: boolean;
  onSelect: (workItem: WorkItemSummary) => void;
}) {
  return (
    <button
      type="button"
      className={`inspector-list-item work-item-button${featured ? " featured" : ""}`}
      onClick={() => onSelect(workItem)}
    >
      <div className="inspector-list-head">
        <strong>{workItem.objective}</strong>
        <StatusBadge className="state-chip" kind="work" value={workItem.state} />
      </div>
      <small>{compactMeta([workItem.current ? "current" : undefined, workItem.planStatus])}</small>
      <code>{workItem.id}</code>
    </button>
  );
}

export function WorkItemDetailPanel({ workItem, detailState }: { workItem: WorkItemSummary; detailState?: WorkItemDetailState }) {
  const loading = detailState?.loading && !detailState.workItem;
  const plan = workItem.planArtifact;
  return (
    <article className="work-item-detail inspector-list-item featured">
      <div className="inspector-list-head">
        <strong>Details</strong>
        {loading ? <StatusBadge className="state-chip" kind="connection" value="loading" /> : null}
      </div>
      {detailState?.error ? <p className="inspector-error">{detailState.error}</p> : null}
      <dl className="inspector-facts">
        <div>
          <dt>Objective</dt>
          <dd>{workItem.objective}</dd>
        </div>
        <div>
          <dt>Work item</dt>
          <dd>{workItem.id}</dd>
        </div>
        <div>
          <dt>Status</dt>
          <dd>{compactMeta([workItem.current ? "current" : undefined, workItem.state, workItem.planStatus])}</dd>
        </div>
        {workItem.revision != null ? (
          <div>
            <dt>Revision</dt>
            <dd>{workItem.revision}</dd>
          </div>
        ) : null}
        {workItem.blockedBy ? (
          <div>
            <dt>Blocked by</dt>
            <dd>{workItem.blockedBy}</dd>
          </div>
        ) : null}
        {workItem.resultSummary ? (
          <div>
            <dt>Result</dt>
            <dd>{workItem.resultSummary}</dd>
          </div>
        ) : null}
        {workItem.updatedAt ? (
          <div>
            <dt>Updated</dt>
            <dd>{formatDateTime(workItem.updatedAt)}</dd>
          </div>
        ) : null}
      </dl>
      {plan?.preview || plan?.path ? (
        <section className="work-item-detail-section">
          <h3>Plan</h3>
          {plan.path ? <code>{plan.path}</code> : null}
          {plan.preview ? <pre>{plan.preview}</pre> : null}
        </section>
      ) : null}
      {workItem.todoList?.length ? (
        <section className="work-item-detail-section">
          <h3>Todo</h3>
          <ul className="inspector-list">
            {workItem.todoList.map((item, index) => (
              <li key={`${item.state}-${index}`}>
                <div className="inspector-list-head">
                  <strong>{item.text}</strong>
                  <StatusBadge className="state-chip" kind="work" value={item.state} />
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      {workItem.workRefs?.length ? (
        <details className="work-item-detail-section work-item-detail-refs">
          <summary>Refs</summary>
          <ul className="inspector-list">
            {workItem.workRefs.map((ref) => (
              <li key={`${ref.kind}-${ref.ref}`}>
                <div className="inspector-list-head">
                  <strong>{ref.title ?? ref.ref}</strong>
                  <StatusBadge className="state-chip" kind="connection" value={ref.status ?? ref.kind} />
                </div>
                <small>{compactMeta([ref.kind, ref.reason])}</small>
                <code>{ref.ref}</code>
              </li>
            ))}
          </ul>
        </details>
      ) : null}
    </article>
  );
}

function formatDateTime(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function compactMeta(parts: Array<string | undefined>): string {
  return parts.filter(Boolean).join(" · ") || "—";
}

