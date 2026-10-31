import { useState } from "react";
import { useTranslation } from "react-i18next";

import { StatusBadge } from "../../components/ui/StatusChip";
import type { TaskSummary, TaskTargetRef } from "../../runtime/types";
import { TaskConversationLink } from "./TaskConversationLink";
import { ObjectDetailRefreshFeedback } from "./ObjectDetailRefreshFeedback";
import { useVisibleAgentResourceRefresh, type AgentResourceReadOptions } from "../../runtime/useVisibleAgentResourceRefresh";

export const RECENT_TASK_LIMIT = 20;

interface AgentTasksPanelProps {
  agentId: string;
  tasks: TaskSummary[];
  activeTaskCount: number;
  onOpenTask: (task: TaskSummary) => void;
  loadRecentTasks: (agentId: string, options?: AgentResourceReadOptions) => Promise<TaskSummary[]>;
  onOpenConversation?: (target: TaskTargetRef) => void;
}

function TaskList({ tasks, agentId, onOpenTask, onOpenConversation }: Pick<AgentTasksPanelProps, "tasks" | "agentId" | "onOpenTask" | "onOpenConversation">) {
  return (
    <ul className="inspector-list">
      {tasks.map((task) => (
        <li key={task.id}>
          <button type="button" className="inspector-list-item task-button" onClick={() => onOpenTask(task)}>
            <div className="inspector-list-head">
              <strong>{task.summary}</strong>
              <StatusBadge className="state-chip" kind="task" value={task.status} />
            </div>
            <small>{[task.kind, task.command, task.workdir].filter(Boolean).join(" · ")}</small>
          </button>
          <TaskConversationLink task={task} fromAgentId={agentId} onOpenConversation={onOpenConversation} />
        </li>
      ))}
    </ul>
  );
}

export function AgentTasksPanel(props: AgentTasksPanelProps) {
  return <AgentTasksPanelContent key={props.agentId} {...props} />;
}

function AgentTasksPanelContent({
  agentId,
  tasks,
  activeTaskCount,
  onOpenTask,
  loadRecentTasks,
  onOpenConversation,
}: AgentTasksPanelProps) {
  const { t } = useTranslation();
  const [recentOpen, setRecentOpen] = useState(false);
  const [sectionOpen, setSectionOpen] = useState(true);
  const resource = useVisibleAgentResourceRefresh(agentId, "recentTasks", sectionOpen && recentOpen, loadRecentTasks);
  const recent = resource.state?.data?.slice(0, RECENT_TASK_LIMIT) ?? [];
  const loading = resource.state?.loading;
  const error = resource.state?.error;

  return (
    <details className="context-card inspector-card collapsible-inspector-card" open
      onToggle={(event) => {
        if (event.target === event.currentTarget) setSectionOpen(event.currentTarget.open);
      }}>
      <summary className="collapsible-inspector-summary">
        <span className="collapsible-inspector-title">
          <span className="eyebrow">{t("rightPanel.tasks")}</span>
          <strong>{t("rightPanel.activeCount", { count: activeTaskCount })}</strong>
        </span>
      </summary>
      <div className="collapsible-inspector-body">
        {tasks.length ? <TaskList tasks={tasks} agentId={agentId} onOpenTask={onOpenTask} onOpenConversation={onOpenConversation} /> : null}
        <details
          className="inspector-details-list"
          onToggle={(event) => setRecentOpen(event.currentTarget.open)}
        >
          <summary>{t("rightPanel.recentCompletedTasks", { count: RECENT_TASK_LIMIT })}</summary>
          {recentOpen ? (
            <div className="inspector-nested-stack">
              <button type="button" className="text-button" disabled={loading || resource.state?.refreshing} onClick={resource.refresh}>
                {t("rightPanel.refreshRecentTasks")}
              </button>
              <ObjectDetailRefreshFeedback state={resource.state ? { ...resource.state, error: undefined } : undefined} />
              {loading ? <p className="inspector-muted">{t("rightPanel.loadingRecentTasks")}</p> : null}
              {error ? <p className="inspector-error" role="alert">{t("rightPanel.recentTasksError", { error })}</p> : null}
              {!loading && !error && !recent.length ? <p className="inspector-muted">{t("rightPanel.noRecentTasks")}</p> : null}
              <TaskList tasks={recent} agentId={agentId} onOpenTask={onOpenTask} onOpenConversation={onOpenConversation} />
            </div>
          ) : null}
        </details>
      </div>
    </details>
  );
}
