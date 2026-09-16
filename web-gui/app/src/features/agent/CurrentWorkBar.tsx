import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronRight, CircleCheck, Clock, ListTodo, LoaderCircle } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { AgentSummary, TaskSummary, WorkItemSummary } from "../../runtime/types";
import type { ConversationSessionModel } from "../../runtime/conversation-view-model";
import { useAgentWorkStatus } from "../../runtime/useAgentWorkStatus";
import { formatTurnDuration } from "./TurnElapsedTime";

const activeTaskStates = new Set(["queued", "running", "cancelling"]);
const normalize = (value?: string) => value?.replaceAll("-", "_") ?? "";

export function deriveCurrentWorkStatus(agent: AgentSummary) {
  const currentWork = agent.currentWork?.state !== "completed" ? agent.currentWork : undefined;
  const waitingOwner = agent.waits?.find((wait) => wait.status === "active" || wait.status === "triggered")?.work_item_id;
  const work = currentWork ?? agent.workItems?.find((item) => item.id === waitingOwner && item.state !== "completed");
  const tasks = (agent.tasks ?? []).filter((task) => activeTaskStates.has(task.status));
  const waits = (agent.waits ?? []).filter((wait) =>
    (!wait.work_item_id || wait.work_item_id === work?.id)
    && (wait.status === "active" || wait.status === "triggered"));
  const activeWait = waits.find((wait) => wait.status === "active");
  const running = Boolean(agent.currentRunId) || normalize(agent.lifecycle) === "awake_running";
  const stopped = ["stopped", "archived", "deleting"].includes(normalize(agent.lifecycle));
  const scheduling = normalize(work?.schedulingState);
  const useAgentWait = !work || !(agent.waits ?? []).some((wait) => wait.status === "active" || wait.status === "triggered");
  const reason = useAgentWait ? normalize(agent.waitingReason) : "";
  const waitKind = activeWait?.kind
    ?? ({ waiting_operator: "operator", waiting_task: "task", waiting_timer: "timer", waiting_external: "external", waiting_system: "system" } as Record<string, string>)[scheduling]
    ?? ({ awaiting_operator_input: "operator", awaiting_task_result: "task", awaiting_timer: "timer", awaiting_external_change: "external" } as Record<string, string>)[reason]
    ?? (useAgentWait && normalize(agent.posture) === "waiting_for_operator" ? "operator" : undefined);
  let state = "ready";
  if (stopped) state = "stopped";
  else if (running) state = "running";
  else if (waits.some((wait) => wait.status === "triggered") && !activeWait) state = "resultReady";
  else if (waitKind === "operator") state = "needsInput";
  else if (waitKind === "task") state = "waitingTask";
  else if (waitKind === "timer") state = "waitingTimer";
  else if (waitKind) state = "waitingExternal";
  else if (scheduling === "blocked" || work?.blockedBy) state = "blocked";
  else if (work?.state === "completing") state = "completing";
  else if (tasks.length > 0 || agent.activeTaskCount > 0) state = "background";
  const waitingTask = state === "waitingTask" && activeWait?.task_ids.length === 1
    ? tasks.find((task) => task.id === activeWait.task_ids[0]) : undefined;
  const waitingSince = !running && !stopped && activeWait ? activeWait.created_at : undefined;
  return { work, tasks, taskCount: Math.max(tasks.length, agent.activeTaskCount || 0), waits, state, waitingTask, waitingSince };
}

function Duration({ since, label }: { since: string; label: string }) {
  const { t } = useTranslation();
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const start = Date.parse(since);
  if (!Number.isFinite(start)) return null;
  const elapsed = Math.max(0, now - start);
  const days = Math.floor(elapsed / 86_400_000);
  return <time className="current-work-duration" title={new Date(start).toLocaleString()}>
    {label} {days > 0 ? `${t("currentWork.days", { count: days })} ` : ""}{formatTurnDuration(elapsed % 86_400_000)}
  </time>;
}

export function CurrentWorkBar({ agent, conversation, onOpenWorkItem, onOpenTask }: {
  agent: AgentSummary;
  conversation?: ConversationSessionModel;
  onOpenWorkItem: (work: WorkItemSummary) => void;
  onOpenTask: (task: TaskSummary) => void;
}) {
  const { t } = useTranslation();
  const latestTurn = conversation?.turns.at(-1);
  const changeKey = `${latestTurn?.turnId}:${latestTurn?.execution.kind}:${conversation?.pendingInputs.length}`;
  const snapshot = useAgentWorkStatus(agent.id, changeKey);
  const current = snapshot.agent ?? agent;
  const view = deriveCurrentWorkStatus(current);
  const [expanded, setExpanded] = useState(false);
  const previousWork = useRef<WorkItemSummary | undefined>(undefined);
  const [completed, setCompleted] = useState<WorkItemSummary>();
  useEffect(() => {
    const previous = previousWork.current;
    previousWork.current = view.work;
    if (view.work) { setCompleted(undefined); return; }
    if (!previous || snapshot.stale) return;
    const finished = current.workItems?.find((item) => item.id === previous.id && item.state === "completed");
    if (!finished) return;
    setCompleted(finished);
  }, [view.work?.id, current.workItems, snapshot.stale]);
  useEffect(() => {
    if (!completed) return;
    const timer = window.setTimeout(() => setCompleted(undefined), 8000);
    return () => window.clearTimeout(timer);
  }, [completed?.id]);
  const work = view.work ?? completed;
  const hasActivity = view.taskCount > 0 || view.waits.length > 0;
  if (!work && !hasActivity && !snapshot.stale) return null;
  const state = completed && !view.work && !hasActivity ? "completed" : view.state;
  const statusText = snapshot.stale ? t("currentWork.stale") : view.waitingTask
    ? t("currentWork.waitingFor", { task: view.waitingTask.summary }) : t(`currentWork.${state}`);
  const Icon = snapshot.stale ? Clock : state === "completed" ? CircleCheck
    : state === "running" || state === "background" ? LoaderCircle : Clock;
  return (
    <section className="current-work-bar" aria-label={t("currentWork.title")} data-state={snapshot.stale ? "stale" : state}>
      {work ? <button className="current-work-title" type="button" onClick={() => onOpenWorkItem(work)} title={work.objective}>
        <ListTodo size={15} /><strong>{work.objective}</strong><ChevronRight size={14} />
      </button> : <div className="current-work-title"><ListTodo size={15} /><strong>{t("currentWork.backgroundTitle")}</strong></div>}
      <div className="current-work-status">
        <Icon size={13} className={!snapshot.stale && ["running", "background"].includes(state) ? "spin" : undefined} />
        <span className="current-work-status-text" role="status" title={statusText}>{statusText}</span>
        {!snapshot.stale && view.waitingSince ? <Duration since={view.waitingSince} label={t("currentWork.waited")} /> : null}
        {view.taskCount > 0 ? <span className="current-work-count">{t("currentWork.taskCount", { count: view.taskCount })}</span> : null}
        <button className="current-work-toggle" type="button" aria-expanded={expanded}
          aria-controls={`current-work-details-${agent.id}`} onClick={() => setExpanded(!expanded)}>
          {t(expanded ? "currentWork.collapse" : "currentWork.expand")}<ChevronDown size={12} />
        </button>
      </div>
      {expanded ? <div className="current-work-details" id={`current-work-details-${agent.id}`}>
        {work ? <button className="current-work-objective" type="button" onClick={() => onOpenWorkItem(work)}>{work.objective}<ChevronRight size={14} /></button> : null}
        {view.work?.blockedBy ? <p>{view.work.blockedBy}</p> : null}
        {view.tasks.map((task) => <button type="button" className="current-work-task" key={task.id} onClick={() => onOpenTask(task)}>
          <span><strong>{task.summary}</strong><small>
            {t(`currentWork.taskState.${task.status}`)}
            {task.workItemId && task.workItemId !== work?.id ? ` · ${t("currentWork.otherWork")}: ${current.workItems?.find((item) => item.id === task.workItemId)?.objective ?? task.workItemId}` : ""}
            {!task.workItemId ? ` · ${t("currentWork.agentTask")}` : ""}
          </small></span>
          {!snapshot.stale && task.createdAt ? <Duration since={task.createdAt} label={t("currentWork.taskAge")} /> : null}<ChevronRight size={14} />
        </button>)}
        {view.taskCount > view.tasks.length ? <p>{t("currentWork.partialTasks", { shown: view.tasks.length, total: view.taskCount })}</p> : null}
        {view.taskCount === 0 ? <p>{t("currentWork.noTasks")}</p> : null}
        {snapshot.stale ? <button type="button" className="current-work-retry" onClick={snapshot.refresh}>{t("currentWork.retry")}</button> : null}
      </div> : null}
    </section>
  );
}
