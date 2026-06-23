import { useMemo, useState } from "react";
import type React from "react";

import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import type { AgentSummary, SkillCatalogEntry, SkillCatalogState, WorkItemDetailState, WorkItemSummary } from "../../runtime/types";

interface AgentOverviewPanelProps {
  agent: AgentSummary;
  skillCatalog?: SkillCatalogState;
  skillCatalogLoading?: boolean;
  skillCatalogError?: string;
  onLoadWorkItemDetail: (workItemId: string) => void;
  onOpenWorkItemDetail: (workItem: WorkItemSummary) => void;
  onRefreshAgentSkills: () => void;
  onDisableAgentSkill: (name: string) => void;
  onOpenSkillManager: () => void;
}

function AgentSkillItem({
  skill,
  disabled,
  onDisable,
}: {
  skill: SkillCatalogEntry;
  disabled?: boolean;
  onDisable: (name: string) => void;
}) {
  const canDisable = skill.scope === "agent";
  return (
    <li>
      <div className="inspector-list-head">
        <strong>{skill.name}</strong>
        <StatusBadge className="state-chip" kind="connection" value={skill.scope} />
      </div>
      <small>{skill.description || skill.path || skill.skillId}</small>
      {canDisable ? (
        <div className="agent-skill-row-actions">
          <button type="button" disabled={disabled} onClick={() => onDisable(skill.name)}>
            Disable
          </button>
        </div>
      ) : null}
    </li>
  );
}

function ManageAgentSkillItem({
  skill,
  disabled,
  onEnable,
}: {
  skill: SkillCatalogEntry;
  disabled?: boolean;
  onEnable: (name: string) => void;
}) {
  return (
    <li>
      <div className="inspector-list-head">
        <strong>{skill.name}</strong>
        <StatusBadge className="state-chip" kind="connection" value={skill.scope} />
      </div>
      <small>{skill.description || skill.skillId}</small>
      <div className="agent-skill-row-actions">
        <button type="button" disabled={disabled} onClick={() => onEnable(skill.name)}>
          Enable
        </button>
      </div>
    </li>
  );
}

export function AgentOverviewPanel({
  agent,
  skillCatalog,
  skillCatalogLoading,
  skillCatalogError,
  onLoadWorkItemDetail,
  onOpenWorkItemDetail,
  onRefreshAgentSkills,
  onDisableAgentSkill,
  onOpenSkillManager,
}: AgentOverviewPanelProps) {
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
      <CollapsibleInspectorCard
        title="Agent"
        summary={`Lifecycle: ${agent.lifecycle}`}
        badge={<StatusBadge className="state-chip" kind="agent" value={agent.posture || agent.lifecycle} />}
      >
        <h2>{agent.id}</h2>
        <dl className="inspector-facts">
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
      </CollapsibleInspectorCard>

      <CollapsibleInspectorCard
        title="Workspace"
        summary={workspaceName}
        badge={<StatusBadge className="state-chip" kind="connection" value={agent.workspace === "not bound" ? "unbound" : "active"} />}
      >
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
      </CollapsibleInspectorCard>

      <CollapsibleInspectorCard
        title="Skills"
        summary={`${skillCatalog?.catalog.length ?? 0} effective`}
        defaultOpen={false}
        badge={<StatusBadge className="state-chip" kind="connection" value={skillCatalogLoading ? "loading" : `${skillCatalog?.catalog.length ?? 0} active`} />}
      >
        <p className="inspector-muted">
          Agent skills are the effective set from user library, workspace skills, and agent-local overrides. Use Manage skills to
          choose user-library skills for this agent.
        </p>
        {skillCatalogError ? <p className="inspector-error">{skillCatalogError}</p> : null}
        {skillCatalog?.catalog.length ? (
          <ul className="inspector-list agent-skill-list">
            {skillCatalog.catalog.slice(0, 8).map((skill) => (
              <AgentSkillItem
                key={`${skill.scope}:${skill.skillId}:${skill.path}`}
                skill={skill}
                disabled={skillCatalogLoading}
                onDisable={onDisableAgentSkill}
              />
            ))}
          </ul>
        ) : (
          <p className="inspector-muted">{skillCatalogLoading ? "Loading effective skills…" : "No effective skills reported for this agent."}</p>
        )}
        <div className="agent-skill-actions">
          <button type="button" className="agent-skill-refresh" onClick={onRefreshAgentSkills} disabled={skillCatalogLoading}>
            {skillCatalogLoading ? "Refreshing…" : "Refresh"}
          </button>
          <button type="button" onClick={onOpenSkillManager}>
            Manage skills…
          </button>
        </div>
      </CollapsibleInspectorCard>

      {hasActiveTasks ? (
        <CollapsibleInspectorCard
          title="Tasks"
          summary={`${agent.activeTaskCount} active`}
          badge={<StatusBadge className="state-chip" kind="connection" value="active" />}
        >
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
        </CollapsibleInspectorCard>
      ) : null}

      {workItems.length ? (
        <CollapsibleInspectorCard
          title="Work items"
          summary={`${openWorkItems.length + currentWorkItems.length} open`}
          className="current-work"
          badge={<StatusBadge className="state-chip" kind="work" value={`${openWorkItems.length + currentWorkItems.length} open`} />}
        >
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
        </CollapsibleInspectorCard>
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

export function AgentSkillManagerPanel({
  skillCatalog,
  availableSkillCatalog,
  skillCatalogLoading,
  availableSkillCatalogLoading,
  onRefreshAvailableSkills,
  onEnableAgentSkill,
}: {
  skillCatalog?: SkillCatalogState;
  availableSkillCatalog?: SkillCatalogState;
  skillCatalogLoading?: boolean;
  availableSkillCatalogLoading?: boolean;
  onRefreshAvailableSkills: () => void;
  onEnableAgentSkill: (name: string) => void;
}) {
  const [skillQuery, setSkillQuery] = useState("");
  const effectiveSkillNames = useMemo(
    () => new Set((skillCatalog?.catalog ?? []).map((skill) => skill.name)),
    [skillCatalog?.catalog],
  );
  const availableAgentSkills = useMemo(() => {
    const query = skillQuery.trim().toLowerCase();
    return (availableSkillCatalog?.catalog ?? [])
      .filter((skill) => skill.scope === "user" && !effectiveSkillNames.has(skill.name))
      .filter((skill) => {
        if (!query) return true;
        return [skill.name, skill.description, skill.skillId].some((value) => value.toLowerCase().includes(query));
      });
  }, [availableSkillCatalog?.catalog, effectiveSkillNames, skillQuery]);

  return (
    <section className="agent-skill-manager" aria-label="Manage agent skills">
      <div className="agent-skill-manager-head">
        <div>
          <span className="eyebrow">User library</span>
          <h2>Manage agent skills</h2>
          <p className="inspector-muted">
            Enable adds a linked agent-local entry. Workspace skills are already effective and are not listed here.
          </p>
        </div>
        <button type="button" onClick={onRefreshAvailableSkills} disabled={availableSkillCatalogLoading}>
          {availableSkillCatalogLoading ? "Refreshing…" : "Refresh catalog"}
        </button>
      </div>
      <label className="agent-skill-search">
        <span>Search available skills</span>
        <input
          type="search"
          value={skillQuery}
          placeholder="Search by name, description, or id"
          aria-label="Search available skills"
          onChange={(event) => setSkillQuery(event.target.value)}
        />
      </label>
      {availableSkillCatalog?.error ? <p className="inspector-error">{availableSkillCatalog.error}</p> : null}
      {availableSkillCatalogLoading ? <p className="inspector-muted">Loading available skills…</p> : null}
      {availableAgentSkills.length ? (
        <>
          <p className="inspector-muted">
            Showing {availableAgentSkills.length} available user-library skill{availableAgentSkills.length === 1 ? "" : "s"}.
          </p>
          <ul className="inspector-list agent-skill-list agent-skill-manager-list">
            {availableAgentSkills.map((skill) => (
              <ManageAgentSkillItem
                key={`${skill.scope}:${skill.skillId}:${skill.path}`}
                skill={skill}
                disabled={skillCatalogLoading || availableSkillCatalogLoading}
                onEnable={onEnableAgentSkill}
              />
            ))}
          </ul>
        </>
      ) : !availableSkillCatalogLoading ? (
        <p className="inspector-muted">
          {availableSkillCatalog
            ? "No user-library skills are available to enable for this agent."
            : "Refresh the catalog to load available skills."}
        </p>
      ) : null}
    </section>
  );
}

function CollapsibleInspectorCard({
  title,
  summary,
  badge,
  children,
  defaultOpen = true,
  className,
}: {
  title: string;
  summary?: string;
  badge?: React.ReactNode;
  children: React.ReactNode;
  defaultOpen?: boolean;
  className?: string;
}) {
  return (
    <details className={`context-card inspector-card collapsible-inspector-card${className ? ` ${className}` : ""}`} open={defaultOpen}>
      <summary className="collapsible-inspector-summary">
        <span className="collapsible-inspector-title">
          <span className="eyebrow">{title}</span>
          {summary ? <strong>{summary}</strong> : null}
        </span>
        {badge}
      </summary>
      <div className="collapsible-inspector-body">{children}</div>
    </details>
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

