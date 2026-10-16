import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { StatusBadge } from "../../components/ui/StatusChip";
import type { TaskSummary } from "../../runtime/types";
import { TaskConversationLink } from "./TaskConversationLink";

export const RECENT_TASK_LIMIT = 20;

interface AgentTasksPanelProps {
  agentId: string;
  tasks: TaskSummary[];
  activeTaskCount: number;
  onOpenTask: (task: TaskSummary) => void;
  loadRecentTasks: (agentId: string) => Promise<TaskSummary[]>;
}

function TaskList({ tasks, onOpenTask }: Pick<AgentTasksPanelProps, "tasks" | "onOpenTask">) {
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
          <TaskConversationLink task={task} />
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
}: AgentTasksPanelProps) {
  const { t } = useTranslation();
  const [recentOpen, setRecentOpen] = useState(false);
  const [recent, setRecent] = useState<TaskSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    if (!recentOpen) return;
    let cancelled = false;
    setLoading(true);
    setRecent([]);
    setError(undefined);
    void loadRecentTasks(agentId).then(
      (result) => {
        if (!cancelled) {
          setRecent(result.slice(0, RECENT_TASK_LIMIT));
          setLoading(false);
        }
      },
      (cause: unknown) => {
        if (!cancelled) {
          setError(cause instanceof Error ? cause.message : String(cause));
          setLoading(false);
        }
      },
    );
    return () => { cancelled = true; };
  }, [agentId, recentOpen, revision, loadRecentTasks]);

  return (
    <details className="context-card inspector-card collapsible-inspector-card" open>
      <summary className="collapsible-inspector-summary">
        <span className="collapsible-inspector-title">
          <span className="eyebrow">{t("rightPanel.tasks")}</span>
          <strong>{t("rightPanel.activeCount", { count: activeTaskCount })}</strong>
        </span>
      </summary>
      <div className="collapsible-inspector-body">
        {tasks.length ? <TaskList tasks={tasks} onOpenTask={onOpenTask} /> : null}
        <details
          className="inspector-details-list"
          onToggle={(event) => setRecentOpen(event.currentTarget.open)}
        >
          <summary>{t("rightPanel.recentCompletedTasks", { count: RECENT_TASK_LIMIT })}</summary>
          {recentOpen ? (
            <div className="inspector-nested-stack">
              <button type="button" className="text-button" disabled={loading} onClick={() => setRevision((value) => value + 1)}>
                {t("rightPanel.refreshRecentTasks")}
              </button>
              {loading ? <p className="inspector-muted">{t("rightPanel.loadingRecentTasks")}</p> : null}
              {error ? <p className="inspector-error" role="alert">{t("rightPanel.recentTasksError", { error })}</p> : null}
              {!loading && !error && !recent.length ? <p className="inspector-muted">{t("rightPanel.noRecentTasks")}</p> : null}
              <TaskList tasks={recent} onOpenTask={onOpenTask} />
            </div>
          ) : null}
        </details>
      </div>
    </details>
  );
}
