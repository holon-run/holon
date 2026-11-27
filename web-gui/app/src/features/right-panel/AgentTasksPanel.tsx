import { Activity, Bot, MessageSquare, Terminal } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import { StatusBadge } from "../../components/ui/StatusChip";
import type { TaskSummary, TaskTargetRef } from "../../runtime/types";
import { TaskConversationLink } from "./TaskConversationLink";

interface AgentTasksPanelProps {
  agentId: string;
  incarnation?: number;
  tasks: TaskSummary[];
  activeTaskCount: number;
  onOpenTask: (task: TaskSummary) => void;
  onOpenConversation?: (target: TaskTargetRef) => void;
}

function formatShortWorkdir(workdir?: string): string | undefined {
  if (!workdir) return undefined;
  const trimmed = workdir.replace(/\/+$/, "");
  if (!trimmed) return "/";
  const parts = trimmed.split("/").filter(Boolean);
  return parts.length >= 2 ? `${parts.at(-2)}/${parts.at(-1)}` : parts.at(-1) ?? trimmed;
}

function formatTaskTime(iso?: string): string | undefined {
  if (!iso) return undefined;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return undefined;
  const now = new Date();
  const sameDay =
    date.getFullYear() === now.getFullYear()
    && date.getMonth() === now.getMonth()
    && date.getDate() === now.getDate();
  return sameDay
    ? date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : date.toLocaleDateString([], { month: "short", day: "numeric" });
}

function resolveTaskKindMeta(kind: string, t: TFunction) {
  const normalized = kind.trim().toLowerCase();
  if (normalized === "command" || normalized === "command_task" || normalized === "shell") {
    return { Icon: Terminal, label: t("rightPanel.taskKindCommand") };
  }
  if (normalized === "child_agent" || normalized === "subagent") {
    return { Icon: Bot, label: t("rightPanel.taskKindSubagent") };
  }
  if (normalized === "agent_message_wait") {
    return { Icon: MessageSquare, label: t("rightPanel.taskKindMessageWait") };
  }
  return { Icon: Activity, label: kind.replace(/_/g, " ") };
}

function TaskList({ tasks, agentId, onOpenTask, onOpenConversation }: Pick<AgentTasksPanelProps, "tasks" | "agentId" | "onOpenTask" | "onOpenConversation">) {
  const { t } = useTranslation();
  return (
    <ul className="inspector-list task-list">
      {tasks.map((task) => {
        const { Icon, label: kindLabel } = resolveTaskKindMeta(task.kind, t);
        const firstCommandLine = task.command?.trim().split(/\r?\n/, 1)[0]?.trim();
        const showCommand = Boolean(firstCommandLine && firstCommandLine !== task.summary.trim());
        const shortWorkdir = formatShortWorkdir(task.workdir);
        const shortTime = formatTaskTime(task.updatedAt ?? task.createdAt);
        return (
          <li key={task.id} className="task-card">
            <button type="button" className="task-button" onClick={() => onOpenTask(task)}>
              <div className="task-row-head">
                <span className="task-row-icon" aria-hidden="true">
                  <Icon size={13} />
                </span>
                <strong className="task-row-title" title={task.summary}>
                  {task.summary}
                </strong>
                <StatusBadge className="state-chip" kind="task" value={task.status} />
              </div>
              {showCommand ? (
                <code className="task-row-command" title={task.command}>
                  {firstCommandLine}
                </code>
              ) : null}
              <div className="task-row-meta">
                <span>{kindLabel}</span>
                {shortWorkdir ? <span title={task.workdir}>{shortWorkdir}</span> : null}
                {shortTime ? <time dateTime={task.updatedAt ?? task.createdAt}>{shortTime}</time> : null}
              </div>
            </button>
            <TaskConversationLink task={task} fromAgentId={agentId} onOpenConversation={onOpenConversation} />
          </li>
        );
      })}
    </ul>
  );
}

export function AgentTasksPanel(props: AgentTasksPanelProps) {
  return <AgentTasksPanelContent key={`${props.agentId}:${props.incarnation}`} {...props} />;
}

function AgentTasksPanelContent({
  agentId,
  tasks,
  activeTaskCount,
  onOpenTask,
  onOpenConversation,
}: AgentTasksPanelProps) {
  const { t } = useTranslation();

  return (
    <details className="context-card inspector-card collapsible-inspector-card" open>
      <summary className="collapsible-inspector-summary">
        <span className="collapsible-inspector-title">
          <span className="eyebrow">{t("rightPanel.tasks")}</span>
        </span>
        {activeTaskCount > 0 ? (
          <span className="inspector-card-count">{t("rightPanel.activeCount", { count: activeTaskCount })}</span>
        ) : (
          <span className="inspector-quiet-meta">{t("rightPanel.activeCount", { count: 0 })}</span>
        )}
      </summary>
      <div className="collapsible-inspector-body">
        {activeTaskCount > tasks.length ? <p className="inspector-muted">{t("rightPanel.activeSnapshotTruncated", { shown: tasks.length, count: activeTaskCount })}</p> : null}
        {tasks.length ? <TaskList tasks={tasks} agentId={agentId} onOpenTask={onOpenTask} onOpenConversation={onOpenConversation} /> : null}
      </div>
    </details>
  );
}
