import { useMemo, useState } from "react";
import { Inbox } from "lucide-react";
import type React from "react";

import { EmptyState } from "../../components/ui/EmptyState";
import { StatusBadge } from "../../components/ui/StatusChip";
import { ToolExecutionContent } from "./ToolExecutionRenderers";
import type { AgentSummary, SkillCatalogEntry, SkillCatalogState, TaskDetailState, TaskSummary, ToolExecutionDetailState, WorkItemDetailState, WorkItemSummary } from "../../runtime/types";
import { useTranslation } from "react-i18next";
import i18next from "i18next";

interface AgentOverviewPanelProps {
  agent: AgentSummary;
  skillCatalog?: SkillCatalogState;
  availableSkillCatalog?: SkillCatalogState;
  skillCatalogLoading?: boolean;
  skillCatalogError?: string;
  onLoadWorkItemDetail: (workItemId: string) => void;
  onOpenWorkItemDetail: (workItem: WorkItemSummary) => void;
  onOpenTask: (task: TaskSummary) => void;
  onRefreshAgentSkills: () => void;
  onDisableAgentSkill: (name: string) => void;
  onOpenSkill: (skillId: string) => void;
  onOpenSkillManager: () => void;
  onBrowseFiles: (workspaceId: string, executionRootId?: string) => void;
}

function AgentSkillItem({
  skill,
  disabled,
  onDisable,
  onOpen,
}: {
  skill: SkillCatalogEntry;
  disabled?: boolean;
  onDisable: (name: string) => void;
  onOpen: (skillId: string) => void;
}) {
  const { t } = useTranslation();
  const canDisable = skill.scope === "agent";
  return (
    <li>
      <button type="button" className="agent-skill-open" onClick={() => onOpen(skill.skillId)}>
        <div className="inspector-list-head">
          <strong>{skill.name}</strong>
          <StatusBadge className="state-chip" kind="connection" value={skill.scope} />
        </div>
        <small>{skill.description || skill.path || skill.name}</small>
      </button>
      {canDisable ? (
        <div className="agent-skill-row-actions">
          <button type="button" disabled={disabled} onClick={() => onDisable(skill.name)}>
            {t("rightPanel.disable")}
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
  const { t } = useTranslation();
  return (
    <li>
      <div className="inspector-list-head">
        <strong>{skill.name}</strong>
        <StatusBadge className="state-chip" kind="connection" value={skill.scope} />
      </div>
      <small>{skill.description || skill.skillId}</small>
      <div className="agent-skill-row-actions">
        <button type="button" disabled={disabled} onClick={() => onEnable(skill.name)}>
          {t("rightPanel.enable")}
        </button>
      </div>
    </li>
  );
}

export function AgentOverviewPanel({
  agent,
  skillCatalog,
  availableSkillCatalog,
  skillCatalogLoading,
  skillCatalogError,
  onLoadWorkItemDetail,
  onOpenWorkItemDetail,
  onOpenTask,
  onRefreshAgentSkills,
  onDisableAgentSkill,
  onOpenSkill,
  onOpenSkillManager,
  onBrowseFiles,
}: AgentOverviewPanelProps) {
  const { t } = useTranslation();
  const workspace = agent.workspaceSummary;
  const workItems = agent.workItems ?? (agent.currentWork ? [agent.currentWork] : []);
  const currentWorkItems = workItems.filter((item) => item.current);
  const openWorkItems = workItems.filter((item) => !item.current && item.state !== "completed");
  const completedWorkItems = workItems.filter((item) => item.state === "completed");
  const currentWorkLabel = currentWorkItems[0]?.objective ?? agent.currentWork?.objective ?? t("rightPanel.noCurrentWork");
  const workspaceName = workspace?.name ?? agent.workspace;
  const workspaceRoot = workspace?.cwd ?? workspace?.executionRoot ?? workspace?.worktree?.path ?? workspace?.anchor;
  const modeLabel = workspace?.worktree ? t("rightPanel.managedWorktree") : workspace?.projectionKind;
  const showCwd = Boolean(workspace?.cwd && workspace.cwd !== workspace.executionRoot);
  const hasActiveTasks = agent.activeTaskCount > 0 || Boolean(agent.tasks?.length);
  const selectWorkItem = (workItem: WorkItemSummary) => {
    onOpenWorkItemDetail(workItem);
    onLoadWorkItemDetail(workItem.id);
  };

  return (
    <div className="inspector-stack">
      <CollapsibleInspectorCard
        title={t("rightPanel.agent")}
        summary={t("rightPanel.lifecycle", { value: agent.lifecycle })}
        badge={<StatusBadge className="state-chip" kind="agent" value={agent.posture || agent.lifecycle} />}
      >
        <h2>{agent.id}</h2>
        <dl className="inspector-facts">
          <div>
            <dt>{t("agent.model")}</dt>
            <dd>{agent.model}</dd>
          </div>
          <div>
            <dt>{t("agent.currentWork")}</dt>
            <dd>{currentWorkLabel}</dd>
          </div>
          <div>
            <dt>{t("agent.scheduling")}</dt>
            <dd>{compactMeta([agent.posture, agent.postureReason])}</dd>
          </div>
        </dl>
      </CollapsibleInspectorCard>

      {(() => {
        const wsCount = agent.attachedWorkspaces?.length ?? (workspace ? 1 : 0);
        const activeTitle = workspace?.worktree?.branch
          ? `${workspaceName} · ${workspace.worktree.branch}`
          : workspaceName;
        const activePath = workspace?.worktree?.path ?? workspace?.executionRoot ?? workspace?.anchor;
        const originPath = workspace?.anchor;
        // Show Worktree + Origin labels when the active path differs from the
        // anchor (origin). When they are the same, show a single "Path" row.
        const isWorktreePath =
          activePath != null && originPath != null && activePath !== originPath;
        return (
          <CollapsibleInspectorCard
           title={t("rightPanel.workspacesWithCount", { count: wsCount })}
            defaultOpen={true}
          >
            {workspace ? (
              <div className="workspace-active-section">
                <div className="inspector-list-head">
                  <strong>{activeTitle}</strong>
                </div>
                <dl className="inspector-facts">
                  {activePath ? (
                    <div>
                      <dt>{isWorktreePath ? t("rightPanel.worktree") : t("rightPanel.path")}</dt>
                      <dd>
                        <a
                          href="#"
                          className="workspace-path-link"
                          onClick={(e) => {
                            e.preventDefault();
                            if (workspace.id) onBrowseFiles(workspace.id, workspace.executionRootId ?? undefined);
                          }}
                        >
                          {activePath}
                        </a>
                      </dd>
                    </div>
                  ) : null}
                  {isWorktreePath && originPath ? (
                    <div>
                      <dt>{t("rightPanel.origin")}</dt>
                      <dd>
                        <a
                          href="#"
                          className="workspace-path-link"
                          onClick={(e) => {
                            e.preventDefault();
                            if (workspace.id) onBrowseFiles(workspace.id, undefined);
                          }}
                        >
                          {originPath}
                        </a>
                      </dd>
                    </div>
                  ) : null}
                </dl>
                <details className="inspector-details-list workspace-technical-details">
                  <summary>{t("panel.details")}</summary>
                  <dl className="inspector-facts">
                    <div>
                      <dt>{t("rightPanel.mode")}</dt>
                      <dd>{compactMeta([modeLabel, workspace.accessMode])}</dd>
                    </div>
                    <div>
                      <dt>{t("rightPanel.name")}</dt>
                      <dd>{workspace.name}</dd>
                    </div>
                    <div>
                      <dt>{t("rightPanel.id")}</dt>
                      <dd>{workspace.id}</dd>
                    </div>
                    <div>
                      <dt>{t("rightPanel.projection")}</dt>
                      <dd>{compactMeta([workspace.projectionKind, workspace.accessMode])}</dd>
                    </div>
                    <div>
                      <dt>{t("rightPanel.executionRoot")}</dt>
                      <dd>{workspace.executionRoot ?? "—"}</dd>
                    </div>
                    {showCwd ? (
                      <div>
                        <dt>{t("rightPanel.cwd")}</dt>
                        <dd>{workspace.cwd}</dd>
                      </div>
                    ) : null}
                    {workspace.worktree ? (
                      <div>
                        <dt>{t("rightPanel.originalBranch")}</dt>
                        <dd>{workspace.worktree.originalBranch ?? "—"}</dd>
                      </div>
                    ) : null}
                  </dl>
                </details>
              </div>
            ) : (
              <p className="inspector-muted">{t("rightPanel.noWorkspace")}</p>
            )}

            {agent.attachedWorkspaces && agent.attachedWorkspaces.length > 0 ? (
              <div className="inspector-stack workspace-other-list">
                {agent.attachedWorkspaces
                  .filter((ws) => {
                    const isActive = ws.executionRootId
                      ? ws.executionRootId === workspace?.executionRootId
                      : ws.workspaceId === workspace?.id;
                    return !isActive;
                  })
                  .map((ws) => (
                    <div key={ws.executionRootId ?? ws.workspaceId} className="workspace-list-item">
                      <div className="workspace-list-item-info">
                        <div className="workspace-list-item-name">{ws.name}</div>
                        <a
                          href="#"
                          className="workspace-path-link workspace-list-item-anchor"
                          onClick={(e) => {
                            e.preventDefault();
                            onBrowseFiles(ws.workspaceId, ws.executionRootId);
                          }}
                        >
                          {ws.anchor}
                        </a>
                      </div>
                    </div>
                  ))}
              </div>
            ) : null}
          </CollapsibleInspectorCard>
        );
      })()}

      <CollapsibleInspectorCard
        title={t("rightPanel.skillsWithCount", { count: skillCatalog?.catalog.length ?? 0 })}
        defaultOpen={false}
      >
        <p className="inspector-muted">
          {t("rightPanel.skillsDesc")}
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
                onOpen={onOpenSkill}
              />
            ))}
          </ul>
        ) : (
          <p className="inspector-muted">{skillCatalogLoading ? t("rightPanel.loadingSkills") : t("rightPanel.noSkills")}</p>
        )}
        <div className="agent-skill-actions">
          <button type="button" className="agent-skill-refresh" onClick={onRefreshAgentSkills} disabled={skillCatalogLoading}>
            {skillCatalogLoading ? t("common.refreshing") : t("common.refresh")}
          </button>
          <button type="button" onClick={onOpenSkillManager}>
            {t("rightPanel.manageSkills")}…
          </button>
        </div>
      </CollapsibleInspectorCard>

      {hasActiveTasks ? (
        <CollapsibleInspectorCard
          title={t("rightPanel.tasks")}
          summary={t("rightPanel.activeCount", { count: agent.activeTaskCount })}
        >
          {agent.tasks?.length ? (
            <ul className="inspector-list">
              {agent.tasks.map((task) => (
                <li key={task.id}>
                  <button type="button" className="inspector-list-item task-button" onClick={() => onOpenTask(task)}>
                    <div className="inspector-list-head">
                    <strong>{task.summary}</strong>
                    <StatusBadge className="state-chip" kind="connection" value={task.status} />
                    </div>
                    <small>{compactMeta([task.kind, task.command, task.workdir])}</small>
                  </button>
                </li>
              ))}
            </ul>
          ) : null}
        </CollapsibleInspectorCard>
      ) : null}

      {workItems.length ? (
        <CollapsibleInspectorCard
          title={t("rightPanel.workItemsWithCount", { count: currentWorkItems.length + openWorkItems.length })}
          className="current-work"
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
              <summary>{t("rightPanel.completedCount", { count: completedWorkItems.length })}</summary>
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
          icon={<Inbox size={20} />}
          title={t("panel.noCurrentWork")}
          description={t("rightPanel.noCurrentWorkDesc")}
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

  const { t } = useTranslation();  const [skillQuery, setSkillQuery] = useState("");
  const effectiveSkillNames = useMemo(
    () => new Set((skillCatalog?.catalog ?? []).map((skill) => skill.name)),
    [skillCatalog?.catalog],
  );
  const availableAgentSkills = useMemo(() => {
    const query = skillQuery.trim().toLowerCase();
    return (availableSkillCatalog?.catalog ?? [])
      .filter((skill) => (skill.scope === "user" || skill.scope === "user_global") && !effectiveSkillNames.has(skill.name))
      .filter((skill) => {
        if (!query) return true;
        return [skill.name, skill.description, skill.skillId].some((value) => value.toLowerCase().includes(query));
      });
  }, [availableSkillCatalog?.catalog, effectiveSkillNames, skillQuery]);

  return (
    <section className="agent-skill-manager" aria-label={t("rightPanel.manageSkills")}>
      <div className="agent-skill-manager-head">
        <div>
          <span className="eyebrow">{t("rightPanel.userLibrary")}</span>
          <h2>{t("rightPanel.manageSkills")}</h2>
          <p className="inspector-muted">
            {t("rightPanel.manageSkillsDesc")}
          </p>
        </div>
        <button type="button" onClick={onRefreshAvailableSkills} disabled={availableSkillCatalogLoading}>
          {availableSkillCatalogLoading ? t("common.refreshing") : t("rightPanel.refreshCatalog")}
        </button>
      </div>
      <label className="agent-skill-search">
        <span>{t("rightPanel.searchAvailable")}</span>
        <input
          type="search"
          value={skillQuery}
          placeholder={t("rightPanel.searchPlaceholder")}
          aria-label={t("rightPanel.searchAvailable")}
          onChange={(event) => setSkillQuery(event.target.value)}
        />
      </label>
      {availableSkillCatalog?.error ? <p className="inspector-error">{availableSkillCatalog.error}</p> : null}
      {availableSkillCatalogLoading ? <p className="inspector-muted">{t("rightPanel.loadingAvailable")}</p> : null}
      {availableAgentSkills.length ? (
        <>
          <p className="inspector-muted">
            {t("rightPanel.showing", { count: availableAgentSkills.length })}
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
            ? t("rightPanel.noAvailableUser")
            : t("rightPanel.refreshToLoad")}
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
  const { t } = useTranslation();
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
      <small>{compactMeta([workItem.current ? t("status.current") : undefined, workItem.planStatus])}</small>
      <code>{workItem.id}</code>
    </button>
  );
}

export function WorkItemDetailPanel({ workItem, detailState, onOpenPlanFile }: { workItem: WorkItemSummary; detailState?: WorkItemDetailState; onOpenPlanFile?: (workspaceId: string, filePath: string) => void }) {
  const { t } = useTranslation();
  const loading = detailState?.loading && !detailState.workItem;
  const plan = workItem.planArtifact;
  return (
    <article className="work-item-detail inspector-list-item featured">
      <div className="inspector-list-head">
        <strong>{t("panel.details")}</strong>
        {loading ? <StatusBadge className="state-chip" kind="connection" value="loading" /> : null}
      </div>
      {detailState?.error ? <p className="inspector-error">{detailState.error}</p> : null}
      <dl className="inspector-facts">
        <div>
          <dt>{t("rightPanel.objective")}</dt>
          <dd>{workItem.objective}</dd>
        </div>
        <div>
          <dt>{t("rightPanel.workItem")}</dt>
          <dd>{workItem.id}</dd>
        </div>
        <div>
          <dt>{t("common.status")}</dt>
          <dd>{compactMeta([workItem.current ? t("status.current") : undefined, workItem.state, workItem.planStatus])}</dd>
        </div>
        {workItem.revision != null ? (
          <div>
            <dt>{t("rightPanel.revision")}</dt>
            <dd>{workItem.revision}</dd>
          </div>
        ) : null}
        {workItem.blockedBy ? (
          <div>
            <dt>{t("rightPanel.blockedBy")}</dt>
            <dd>{workItem.blockedBy}</dd>
          </div>
        ) : null}
        {workItem.resultSummary ? (
          <div>
            <dt>{t("inspector.result")}</dt>
            <dd>{workItem.resultSummary}</dd>
          </div>
        ) : null}
        {workItem.updatedAt ? (
          <div>
            <dt>{t("rightPanel.updated")}</dt>
            <dd>{formatDateTime(workItem.updatedAt)}</dd>
          </div>
        ) : null}
      </dl>
      {plan?.preview || plan?.path ? (
        <section className="work-item-detail-section">
          <h3>{t("rightPanel.plan")}</h3>
          {plan.path ? (
            onOpenPlanFile && plan.workspaceId && plan.relativePath ? (
              <a href="#" className="workspace-path-link" onClick={(e) => { e.preventDefault(); onOpenPlanFile(plan.workspaceId!, plan.relativePath!); }}>
                <code>{plan.path}</code>
              </a>
            ) : (
              <code>{plan.path}</code>
            )
          ) : null}
          {plan.preview ? <pre>{plan.preview}</pre> : null}
        </section>
      ) : null}
      {workItem.todoList?.length ? (
        <section className="work-item-detail-section">
          <h3>{t("rightPanel.todo")}</h3>
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
          <summary>{t("rightPanel.refs")}</summary>
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

export function TaskDetailPanel({ task, detailState }: { task: TaskSummary; detailState?: TaskDetailState }) {
  const { t } = useTranslation();
  const loading = detailState?.loading && !detailState?.output;
  const output = detailState?.output;
  const taskRecord = output?.task;
  const status = taskRecord?.status ?? output?.status ?? task.status;
  const summary = taskRecord?.summary ?? output?.summary ?? task.summary;
  const exitStatus = taskRecord?.exit_status;
  const outputText =
    taskRecord?.output_preview ??
    output?.output ??
    output?.stdout ??
    "";
  const stderrText = output?.stderr ?? "";
  const truncated = taskRecord?.output_truncated ?? output?.truncated;
  const command = task.command;
  const taskIdShort = task.id.replace(/^task:/, "").slice(0, 12);
  const resultSummary = taskRecord?.result_summary;

  return (
    <article className="task-detail inspector-list-item featured">
      <div className="inspector-list-head">
        <strong>{summary || t("inspector.taskOutput")}</strong>
        <StatusBadge className="state-chip" kind="connection" value={status} />
        {loading ? <StatusBadge className="state-chip" kind="connection" value="loading" /> : null}
      </div>
      {detailState?.error ? <p className="inspector-error">{detailState.error}</p> : null}
      <dl className="inspector-facts">
        <div>
          <dt>{t("rightPanel.kind")}</dt>
          <dd>{task.kind}</dd>
        </div>
        {exitStatus != null ? (
          <div>
            <dt>{t("inspector.exit")}</dt>
            <dd>{exitStatus}</dd>
          </div>
        ) : null}
        {task.workdir ? (
          <div>
            <dt>{t("rightPanel.workdir")}</dt>
            <dd><code>{task.workdir}</code></dd>
          </div>
        ) : null}
      </dl>
      {command ? (
        <section className="work-item-detail-section">
          <h3>{t("rightPanel.command")}</h3>
          <pre className="task-command-pre">{command}</pre>
        </section>
      ) : null}
      {resultSummary ? (
        <section className="work-item-detail-section">
          <h3>{t("rightPanel.output")}</h3>
          <pre>{resultSummary}</pre>
        </section>
      ) : null}
      {outputText ? (
        <section className="work-item-detail-section">
          <h3>{truncated ? t("rightPanel.outputTruncated") : t("rightPanel.output")}</h3>
          <pre>{outputText}</pre>
        </section>
      ) : null}
      {stderrText ? (
        <section className="work-item-detail-section">
          <h3>{t("rightPanel.stderr")}</h3>
          <pre>{stderrText}</pre>
        </section>
      ) : null}
    </article>
  );
}

export function ToolExecutionDetailPanel({
  toolExecutionId,
  toolName,
  detailState,
  relatedStateObjectRef,
  onOpenWorkItem,
  onOpenTask,
}: {
  toolExecutionId: string;
  toolName?: string;
  detailState?: ToolExecutionDetailState;
  relatedStateObjectRef?: import("../../runtime/types").TimelineStateObjectRef;
  onOpenWorkItem?: (workItem: WorkItemSummary) => void;
  onOpenTask?: (task: TaskSummary) => void;
}) {
  const { t } = useTranslation();
  const loading = detailState?.loading && !detailState?.toolExecution;
  const record = detailState?.toolExecution;

  return (
    <article className="tool-execution-detail inspector-list-item featured">
      {relatedStateObjectRef ? (
        <div className="inspector-breadcrumb">
          {relatedStateObjectRef.kind === "work_item" && onOpenWorkItem ? (
            <button
              type="button"
              className="breadcrumb-link"
              onClick={() => onOpenWorkItem({ id: relatedStateObjectRef.id, objective: relatedStateObjectRef.objective ?? "", state: relatedStateObjectRef.state ?? "unknown" })}
            >
              {t("inspector.relatedWorkItem")}: {relatedStateObjectRef.id.replace(/^work_/, "").slice(0, 12)}
            </button>
          ) : null}
          {relatedStateObjectRef.kind === "task" && onOpenTask ? (
            <button
              type="button"
              className="breadcrumb-link"
              onClick={() => onOpenTask({ id: relatedStateObjectRef.id.replace(/^task:/, ""), kind: "task", status: relatedStateObjectRef.status ?? "unknown", summary: relatedStateObjectRef.summary ?? "" })}
            >
              {t("inspector.relatedTask")}: {relatedStateObjectRef.id.replace(/^task:/, "").slice(0, 12)}
            </button>
          ) : null}
        </div>
      ) : null}
      <div className="inspector-list-head">
        <strong>{toolName ?? record?.tool_name ?? t("inspector.toolExecution")}</strong>
        {loading ? <StatusBadge className="state-chip" kind="connection" value="loading" /> : null}
      </div>
      {detailState?.error ? <p className="inspector-error">{detailState.error}</p> : null}
      <dl className="inspector-facts">
        <div>
          <dt>{t("inspector.tool")}</dt>
          <dd>{record?.tool_name ?? toolName ?? "—"}</dd>
        </div>
        <div>
          <dt>{t("common.status")}</dt>
          <dd>{record?.status ?? "—"}</dd>
        </div>
        {record?.duration_ms != null ? (
          <div>
            <dt>{t("inspector.duration")}</dt>
            <dd>{record.duration_ms}ms</dd>
          </div>
        ) : null}
      </dl>
      {record ? <ToolExecutionContent record={record} /> : null}
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

