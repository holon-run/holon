import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { StatusBadge } from "../../components/ui/StatusChip";
import { useRuntimeStore } from "../../runtime/runtime-store";
import type {
  TaskDetailState,
  TaskFailureArtifact,
  TaskStatusSnapshot,
  TaskSummary,
  WorkItemSummary,
} from "../../runtime/types";
import { normalizeTaskDetailContent } from "./TaskDetailRenderers";

const ACTIVE_REFRESH_MS = 4000;
const TERMINAL_STATUSES = new Set(["completed", "failed", "cancelled", "interrupted"]);

function formatDateTime(value: string | null | undefined): string {
  if (!value) return "-";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function formatDuration(ms: number): string {
  if (ms < 0 || !Number.isFinite(ms)) return "-";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.round(seconds % 60);
  if (minutes < 60) return `${minutes}m ${remainingSeconds}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

function computeDuration(createdAt: string, updatedAt: string, isRunning: boolean): string {
  const start = new Date(createdAt).getTime();
  const end = isRunning ? Date.now() : new Date(updatedAt).getTime();
  if (Number.isNaN(start) || Number.isNaN(end)) return "-";
  return formatDuration(end - start);
}

function copyToClipboard(text: string): void {
  if (!navigator.clipboard) return;
  void navigator.clipboard.writeText(text);
}

interface TaskDetailPanelProps {
  task: TaskSummary;
  detailState?: TaskDetailState;
  agentId: string;
  onOpenWorkItem?: (workItem: WorkItemSummary) => void;
}

export function TaskDetailPanel({ task, detailState, agentId, onOpenWorkItem }: TaskDetailPanelProps) {
  const { t } = useTranslation();
  const loadAgentTaskDetail = useRuntimeStore((s) => s.loadAgentTaskDetail);
  const loading = detailState?.loading && !detailState?.output;
  const output = detailState?.output;
  const status = detailState?.status;
  const taskRecord = output?.task;
  const effectiveStatus = status?.status ?? taskRecord?.status ?? output?.status ?? task.status;
  const isRunning = !TERMINAL_STATUSES.has(effectiveStatus);
  const detail = normalizeTaskDetailContent(task, output);
  const summary = detail.summary || task.summary;

  const handleRefresh = useCallback(() => {
    void loadAgentTaskDetail(agentId, task.id, true);
  }, [agentId, task.id, loadAgentTaskDetail]);

  // Auto-refresh for running tasks
  useEffect(() => {
    if (!isRunning) return;
    const interval = setInterval(() => {
      void loadAgentTaskDetail(agentId, task.id, true);
    }, ACTIVE_REFRESH_MS);
    return () => clearInterval(interval);
  }, [isRunning, agentId, task.id, loadAgentTaskDetail]);

  const failureArtifact = useMemo(() => {
    const raw = taskRecord?.failure_artifact as TaskFailureArtifact | undefined;
    if (raw && typeof raw.summary === "string") return raw;
    return undefined;
  }, [taskRecord]);

  return (
    <article className="task-detail inspector-list-item featured">
      <TaskDetailHeader
        summary={summary}
        status={effectiveStatus}
        kind={status?.kind ?? task.kind}
        createdAt={status?.created_at}
        updatedAt={status?.updated_at}
        isRunning={isRunning}
        loading={loading}
        onRefresh={handleRefresh}
        t={t}
      />
      {detailState?.error ? <p className="inspector-error">{detailState.error}</p> : null}
      {failureArtifact ? <TaskFailureSection failure={failureArtifact} t={t} /> : null}
      <TaskOverviewSection status={status} task={task} onOpenWorkItem={onOpenWorkItem} t={t} />
      {task.kind === "command_task" || status?.kind === "command_task" ? (
        <CommandTaskSection task={task} status={status} t={t} />
      ) : null}
      {status?.child_agent_id || status?.child_observability || status?.child_supervision ? (
        <ChildAgentTaskSection status={status} onOpenWorkItem={onOpenWorkItem} t={t} />
      ) : null}
      <TaskOutputSection detail={detail} isRunning={isRunning} t={t} />
      <TaskTechnicalDetails task={task} status={status} t={t} />
    </article>
  );
}

type TFunc = ReturnType<typeof useTranslation>["t"];

function TaskDetailHeader({
  summary,
  status,
  kind,
  createdAt,
  updatedAt,
  isRunning,
  loading,
  onRefresh,
  t,
}: {
  summary: string;
  status: string;
  kind: string;
  createdAt?: string;
  updatedAt?: string;
  isRunning: boolean;
  loading?: boolean;
  onRefresh: () => void;
  t: TFunc;
}) {
  const duration = createdAt ? computeDuration(createdAt, updatedAt ?? createdAt, isRunning) : null;

  return (
    <div className="task-detail-header">
      <div className="inspector-list-head">
        <strong>{summary || t("inspector.taskOutput")}</strong>
        <StatusBadge className="state-chip" kind="task" value={status} />
      </div>
      <div className="task-detail-subhead">
        <span className="task-detail-kind">{kind}</span>
        {duration ? (
          <>
            <span className="task-detail-sep">·</span>
            <span className="task-detail-duration">
              {isRunning ? `${t("inspector.running")} ` : ""}
              {duration}
            </span>
          </>
        ) : null}
        {loading ? <StatusBadge className="state-chip" kind="runtime" value="loading" /> : null}
        <button
          type="button"
          className="task-detail-refresh"
          onClick={onRefresh}
          aria-label={t("inspector.refresh")}
        >
          ↻
        </button>
      </div>
    </div>
  );
}

function TaskOverviewSection({
  status,
  task,
  onOpenWorkItem,
  t,
}: {
  status?: TaskStatusSnapshot;
  task: TaskSummary;
  onOpenWorkItem?: (workItem: WorkItemSummary) => void;
  t: TFunc;
}) {
  const childWorkItemId = status?.child_supervision?.child_work_item_id;
  const parentWorkItemId = status?.child_supervision?.parent_work_item_id;

  return (
    <dl className="inspector-facts">
      {status?.created_at ? (
        <div>
          <dt>{t("inspector.started")}</dt>
          <dd>{formatDateTime(status.created_at)}</dd>
        </div>
      ) : null}
      {status?.updated_at ? (
        <div>
          <dt>{t("inspector.updated")}</dt>
          <dd>{formatDateTime(status.updated_at)}</dd>
        </div>
      ) : null}
      {task.workdir ? (
        <div>
          <dt>{t("rightPanel.workdir")}</dt>
          <dd><code>{task.workdir}</code></dd>
        </div>
      ) : null}
      {childWorkItemId && onOpenWorkItem ? (
        <div>
          <dt>{t("inspector.workItem")}</dt>
          <dd>
            <button
              type="button"
              className="breadcrumb-link"
              onClick={() => onOpenWorkItem({ id: childWorkItemId, objective: "", state: "open" })}
            >
              {childWorkItemId.replace(/^work_/, "").slice(0, 12)}
            </button>
          </dd>
        </div>
      ) : null}
      {parentWorkItemId && onOpenWorkItem ? (
        <div>
          <dt>{t("inspector.parentWorkItem")}</dt>
          <dd>
            <button
              type="button"
              className="breadcrumb-link"
              onClick={() => onOpenWorkItem({ id: parentWorkItemId, objective: "", state: "open" })}
            >
              {parentWorkItemId.replace(/^work_/, "").slice(0, 12)}
            </button>
          </dd>
        </div>
      ) : null}
    </dl>
  );
}

function CommandTaskSection({
  task,
  status,
  t,
}: {
  task: TaskSummary;
  status?: TaskStatusSnapshot;
  t: TFunc;
}) {
  const cmd = status?.command?.cmd ?? task.command ?? "";
  const workdir = status?.command?.workdir ?? task.workdir;
  const shell = status?.command?.shell;
  const tty = status?.command?.tty;
  const login = status?.command?.login;
  const exitStatus = status?.command?.exit_status ?? undefined;
  const promoted = status?.command?.promoted_from_exec_command;
  const acceptsInput = status?.command?.accepts_input;

  const metaParts: string[] = [];
  if (shell) metaParts.push(`shell: ${shell}`);
  if (login) metaParts.push("login");
  if (tty) metaParts.push("tty");
  if (promoted) metaParts.push("promoted");
  if (acceptsInput) metaParts.push("interactive");

  return (
    <div className="task-detail-section">
      <h3 className="task-detail-section-title">{t("inspector.execution")}</h3>
      {cmd ? (
        <div className="task-detail-command">
          <pre className="tool-detail-field-content">{cmd}</pre>
          <button
            type="button"
            className="task-detail-copy"
            aria-label={t("inspector.copy")}
            onClick={() => copyToClipboard(cmd)}
          >
            {t("inspector.copy")}
          </button>
        </div>
      ) : null}
      {workdir ? (
        <div className="tool-detail-simple">
          <span className="tool-detail-simple-label">{t("rightPanel.workdir")}</span>
          <span className="tool-detail-simple-value"><code>{workdir}</code></span>
        </div>
      ) : null}
      {metaParts.length > 0 ? (
        <div className="tool-detail-simple">
          <span className="tool-detail-simple-label">{t("inspector.meta")}</span>
          <span className="tool-detail-simple-value">{metaParts.join(" · ")}</span>
        </div>
      ) : null}
      {exitStatus != null ? (
        <div className="tool-detail-simple">
          <span className="tool-detail-simple-label">{t("inspector.exit")}</span>
          <span className="tool-detail-simple-value">{exitStatus}</span>
        </div>
      ) : null}
    </div>
  );
}

function ChildAgentTaskSection({
  status,
  onOpenWorkItem,
  t,
}: {
  status: TaskStatusSnapshot;
  onOpenWorkItem?: (workItem: WorkItemSummary) => void;
  t: TFunc;
}) {
  const observability = status.child_observability;
  const supervision = status.child_supervision;
  const tokenUsage = status.token_usage;
  const childAgentId = status.child_agent_id ?? supervision?.child_agent_id;

  return (
    <div className="task-detail-section">
      <h3 className="task-detail-section-title">{t("inspector.childAgent")}</h3>
      <dl className="inspector-facts">
        {childAgentId ? (
          <div>
            <dt>{t("inspector.agentId")}</dt>
            <dd><code>{childAgentId}</code></dd>
          </div>
        ) : null}
        {observability?.phase ? (
          <div>
            <dt>{t("inspector.phase")}</dt>
            <dd>
              <StatusBadge className="state-chip" kind="task" value={observability.phase} />
              {observability.waiting_reason ? ` · ${observability.waiting_reason}` : ""}
            </dd>
          </div>
        ) : null}
        {observability?.work_summary ? (
          <div>
            <dt>{t("inspector.workSummary")}</dt>
            <dd>{observability.work_summary}</dd>
          </div>
        ) : null}
        {observability?.last_progress_brief ? (
          <div>
            <dt>{t("inspector.lastProgress")}</dt>
            <dd>{observability.last_progress_brief}</dd>
          </div>
        ) : null}
        {observability?.last_result_brief ? (
          <div>
            <dt>{t("inspector.lastResult")}</dt>
            <dd>{observability.last_result_brief}</dd>
          </div>
        ) : null}
        {supervision?.workspace_mode ? (
          <div>
            <dt>{t("inspector.workspaceMode")}</dt>
            <dd>{supervision.workspace_mode}</dd>
          </div>
        ) : null}
        {supervision?.worktree?.actual_branch ? (
          <div>
            <dt>{t("inspector.branch")}</dt>
            <dd><code>{supervision.worktree.actual_branch}</code></dd>
          </div>
        ) : null}
        {supervision?.worktree?.changed_files?.length ? (
          <div>
            <dt>{t("inspector.changedFiles")}</dt>
            <dd>{supervision.worktree.changed_files.length} files</dd>
          </div>
        ) : null}
        {supervision?.cleanup_status ? (
          <div>
            <dt>{t("inspector.cleanupStatus")}</dt>
            <dd>{supervision.cleanup_status}</dd>
          </div>
        ) : null}
        {tokenUsage ? (
          <div>
            <dt>{t("inspector.tokenUsage")}</dt>
            <dd>
              {tokenUsage.total.total_tokens.toLocaleString()} tokens · {tokenUsage.total_model_rounds} rounds
            </dd>
          </div>
        ) : null}
        {status.model_resolution ? (
          <div>
            <dt>{t("inspector.modelRef")}</dt>
            <dd>
              <code>{status.model_resolution.resolved_provider}@{status.model_resolution.resolved_model}</code>
              {status.model_resolution.resolution_status === "fallback_used" && status.model_resolution.requested
                ? <span className="task-detail-sep"> · {t("inspector.fallback")}: <code>{status.model_resolution.requested.provider}@{status.model_resolution.requested.model}</code></span>
                : null}
              {status.model_resolution.resolution_status === "inherited" ? ` · ${t("inspector.inherited")}` : ""}
            </dd>
          </div>
        ) : null}
      </dl>
      {observability?.current_work_item_id && onOpenWorkItem ? (
        <button
          type="button"
          className="breadcrumb-link"
          onClick={() => onOpenWorkItem({ id: observability.current_work_item_id!, objective: "", state: "open" })}
        >
          {t("inspector.openWorkItem")}: {observability.current_work_item_id.replace(/^work_/, "").slice(0, 12)}
        </button>
      ) : null}
    </div>
  );
}

type OutputTab = "stdout" | "stderr" | "result" | "raw";

function TaskOutputSection({
  detail,
  isRunning,
  t,
}: {
  detail: ReturnType<typeof normalizeTaskDetailContent>;
  isRunning: boolean;
  t: TFunc;
}) {
  const tabs = useMemo(() => {
    const result: { key: OutputTab; label: string; value: string; variant?: "error" }[] = [];
    if (detail.stdout) result.push({ key: "stdout", label: t("inspector.stdout"), value: detail.stdout });
    if (detail.stderr) result.push({ key: "stderr", label: t("inspector.stderr"), value: detail.stderr, variant: "error" });
    if (detail.result) result.push({ key: "result", label: t("inspector.result"), value: detail.result });
    if (detail.rawOutput) result.push({ key: "raw", label: detail.rawOutputTruncated ? t("inspector.outputTruncated") : t("inspector.output"), value: detail.rawOutput });
    return result;
  }, [detail, t]);

  const [activeTab, setActiveTab] = useState<OutputTab | null>(null);
  useEffect(() => {
    if (tabs.length === 0) return;
    if (activeTab && tabs.some((tab) => tab.key === activeTab)) return;
    const stderrTab = tabs.find((tab) => tab.key === "stderr");
    setActiveTab(stderrTab?.key ?? tabs[0].key);
  }, [tabs, activeTab]);

  if (tabs.length === 0) return null;

  const showTabs = tabs.length > 1;
  const activeContent = tabs.find((tab) => tab.key === activeTab);

  return (
    <div className="task-detail-section">
      <div className="task-detail-output-head">
        <h3 className="task-detail-section-title">{t("inspector.output")}</h3>
        <span className="task-detail-output-badge">{isRunning ? t("inspector.live") : t("inspector.final")}</span>
      </div>
      {showTabs ? (
        <div className="task-detail-tabs" role="tablist">
          {tabs.map((tab) => (
            <button
              key={tab.key}
              type="button"
              role="tab"
              aria-selected={activeTab === tab.key}
              className={`task-detail-tab${activeTab === tab.key ? " active" : ""}${tab.variant === "error" ? " error" : ""}`}
              onClick={() => setActiveTab(tab.key)}
            >
              {tab.label}
              {tab.key === "stderr" ? ` ${tab.value.split("\n").length}` : ""}
            </button>
          ))}
        </div>
      ) : null}
      {activeContent ? (
        <div className="task-detail-output-body">
          <pre className={`tool-detail-field-content${activeContent.variant === "error" ? " error" : ""}`}>
            {activeContent.value}
          </pre>
          <button
            type="button"
            className="task-detail-copy"
            aria-label={t("inspector.copy")}
            onClick={() => copyToClipboard(activeContent.value)}
          >
            {t("inspector.copy")}
          </button>
        </div>
      ) : null}
    </div>
  );
}

function TaskFailureSection({ failure, t }: { failure: TaskFailureArtifact; t: TFunc }) {
  return (
    <div className="task-detail-failure">
      <div className="task-detail-failure-head">
        <strong>{t("inspector.failure")}</strong>
      </div>
      <p className="task-detail-failure-summary">{failure.summary}</p>
      <dl className="inspector-facts">
        {failure.kind ? (
          <div>
            <dt>{t("inspector.kind")}</dt>
            <dd>{failure.kind}</dd>
          </div>
        ) : null}
        {failure.category ? (
          <div>
            <dt>{t("inspector.category")}</dt>
            <dd>{failure.category}</dd>
          </div>
        ) : null}
        {failure.domain ? (
          <div>
            <dt>{t("inspector.domain")}</dt>
            <dd>{failure.domain}</dd>
          </div>
        ) : null}
        {failure.status != null ? (
          <div>
            <dt>{t("inspector.httpStatus")}</dt>
            <dd>{failure.status}</dd>
          </div>
        ) : null}
        {failure.exit_status != null ? (
          <div>
            <dt>{t("inspector.exit")}</dt>
            <dd>{failure.exit_status}</dd>
          </div>
        ) : null}
        {failure.provider ? (
          <div>
            <dt>{t("inspector.provider")}</dt>
            <dd>{failure.provider}</dd>
          </div>
        ) : null}
        {failure.model_ref ? (
          <div>
            <dt>{t("inspector.modelRef")}</dt>
            <dd><code>{failure.model_ref}</code></dd>
          </div>
        ) : null}
        {failure.retryable != null ? (
          <div>
            <dt>{t("inspector.retryable")}</dt>
            <dd>{failure.retryable ? t("inspector.yes") : t("inspector.no")}</dd>
          </div>
        ) : null}
      </dl>
      {failure.recovery_hint ? (
        <div className="task-detail-failure-recovery">
          <span className="tool-detail-simple-label">{t("inspector.recoveryHint")}</span>
          <p>{failure.recovery_hint}</p>
        </div>
      ) : null}
      {failure.source_chain?.length ? (
        <div className="task-detail-failure-chain">
          <span className="tool-detail-simple-label">{t("inspector.sourceChain")}</span>
          <code>{failure.source_chain.join(" -> ")}</code>
        </div>
      ) : null}
    </div>
  );
}

function TaskTechnicalDetails({
  task,
  status,
  t,
}: {
  task: TaskSummary;
  status?: TaskStatusSnapshot;
  t: TFunc;
}) {
  return (
    <details className="task-detail-technical collapsible-inspector-card">
      <summary className="collapsible-inspector-summary">
        <span className="collapsible-inspector-title">
          <strong>{t("inspector.technicalDetails")}</strong>
        </span>
      </summary>
      <div className="collapsible-inspector-body">
        <dl className="inspector-facts">
          <div>
            <dt>{t("inspector.taskId")}</dt>
            <dd><code>{task.id}</code></dd>
          </div>
          <div>
            <dt>{t("inspector.kind")}</dt>
            <dd>{status?.kind ?? task.kind}</dd>
          </div>
          {status?.wait_policy ? (
            <div>
              <dt>{t("inspector.waitPolicy")}</dt>
              <dd>{status.wait_policy}</dd>
            </div>
          ) : null}
          {status?.parent_message_id ? (
            <div>
              <dt>{t("inspector.parentMessage")}</dt>
              <dd><code>{status.parent_message_id}</code></dd>
            </div>
          ) : null}
          {status?.command?.output_path ? (
            <div>
              <dt>{t("inspector.outputPath")}</dt>
              <dd><code>{status.command.output_path}</code></dd>
            </div>
          ) : null}
          {status?.command?.cmd_digest ? (
            <div>
              <dt>{t("inspector.cmdDigest")}</dt>
              <dd><code>{status.command.cmd_digest}</code></dd>
            </div>
          ) : null}
        </dl>
      </div>
    </details>
  );
}
