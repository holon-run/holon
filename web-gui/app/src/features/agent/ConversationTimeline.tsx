import { TurnElapsedTime } from "./TurnElapsedTime";
import {
  Bot,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  Check,
  Copy,
  Clock,
  LoaderCircle,
  RefreshCw,
  Unplug,
  User,
  Wrench,
  ExternalLink,
} from "lucide-react";
import { memo, useEffect, useMemo, useState, useRef, useId } from "react";
import { useTranslation } from "react-i18next";

import type {
  BriefRecord,
  ConversationActivity,
  ConversationBriefLoadState,
  ConversationDetailLoadState,
  PendingInput,
  TurnInputSummary,
} from "@holon/conversation-sdk";

import { MarkdownContent } from "../../components/MarkdownContent";
import { EmptyState } from "../../components/ui/EmptyState";
import type { AgentTimelineActivity, ToolExecutionDetailState } from "../../runtime/types";
import { toolExecutionPresentation } from "../../runtime/tool-execution-presentation";
import {
  turnResultPresentation,
  turnExecutionPresentation,
  type ConversationSessionModel,
  type ConversationTurnGroup,
} from "../../runtime/conversation-view-model";

export interface ConversationTimelineActions {
  onLoadBrief: (briefId: string) => void;
  onLoadDetail: (turnId: string) => void;
  onLoadOlderActivities: (turnId: string) => void;
  onRetry: () => void;
  onInspectActivity?: (activity: AgentTimelineActivity) => void;
  onLoadToolDetail?: (id: string, revision: number) => void;
  toolDetails?: Record<string, ToolExecutionDetailState>;
  selectedActivityId?: string;
  briefRecord: (briefId: string) => BriefRecord | null;
  briefLoadState: (briefId: string) => ConversationBriefLoadState | null;
  detailLoadState: (turnId: string) => ConversationDetailLoadState;
}

export interface ConversationTimelineProps extends ConversationTimelineActions {
  model: ConversationSessionModel;
}

/**
 * Canonical turn-based conversation surface. Renders one card per
 * ConversationTurnGroup: assigned inputs, retained briefs, and on-demand
 * activity detail. Protocol state stays in the SDK; this component is a pure
 * projection over ConversationSessionModel.
 */
export const ConversationTimeline = memo(function ConversationTimeline({
  model,
  ...actions
}: ConversationTimelineProps) {
  const { t } = useTranslation();
  const status = model.status;
  return (
    <div className="conversation-timeline" aria-label={t("agent.conversationAria")}>
      {status.kind === "reconnecting" ? (
        <div className="conversation-status-banner is-reconnecting" role="status">
          <Unplug size={14} />
          <span>{t("agentPage.conversationReconnecting", { attempt: status.attempt })}</span>
        </div>
      ) : null}
      {status.kind === "unsupported" ? (
        <div className="conversation-status-banner is-error" role="alert">
          <CircleAlert size={14} />
          <span>{t("agentPage.conversationUnsupported")}</span>
        </div>
      ) : null}
      {status.kind === "recoverable_error" || status.kind === "terminal_error" ? (
        <div className="conversation-status-banner is-error" role="alert">
          <CircleAlert size={14} />
          <span>{t("agentPage.conversationError")}</span>
          <button type="button" onClick={actions.onRetry}>
            <RefreshCw size={13} />
            {t("agentPage.retry")}
          </button>
        </div>
      ) : null}
      {model.turns.map((turn) => (
        <ConversationTurnCard
          key={`${model.view?.scope?.remote_id}:${model.view?.scope?.agent_id}:${model.view?.scope?.event_log_epoch}:${turn.turnId}`}
          turn={turn}
          {...actions}
        />
      ))}
      {model.pendingInputs.length > 0 ? (
        <div className="conversation-pending-inputs" aria-label={t("agentPage.pendingInputs")}>
          {model.pendingInputs.map((input) => (
            <PendingInputChip key={input.message_id} input={input} />
          ))}
        </div>
      ) : null}
      {!model.bootstrapLoading &&
      model.turns.length === 0 &&
      model.pendingInputs.length === 0 &&
      (status.kind === "ready" || status.kind === "reconnecting") ? (
        <EmptyState
          className="conversation-empty"
          icon="↵"
          title={t("agent.noActivity")}
          description={t("agent.conversationEmpty")}
        />
      ) : null}
      {model.bootstrapLoading ? (
        <div className="conversation-loading" role="status" aria-label={t("common.loading")}>
          <LoaderCircle size={24} className="is-spinning" />
          <span>{t("common.syncing")}</span>
        </div>
      ) : null}
    </div>
  );
});

function PendingInputChip({ input }: { input: PendingInput }) {
  const { t } = useTranslation();
  return (
    <div className="conversation-pending-chip" data-conversation-anchor={`input:${input.message_id}`}>
      {input.preview ? <MarkdownContent text={parseInputPreview(input.preview)} /> : null}
      <span className="conversation-input-status" role="status">
        <Clock size={12} />
        {t(input.state === "assigning" ? "agentPage.pendingAssigning" : "agentPage.pendingQueued")}
      </span>
    </div>
  );
}

const ConversationTurnCard = memo(function ConversationTurnCard({
  turn,
  ...actions
}: { turn: ConversationTurnGroup } & ConversationTimelineActions) {
  const { t } = useTranslation();
  const detailId = useId();
  const detailRef = useRef<HTMLDivElement>(null);
  const [manualExpanded, setManualExpanded] = useState<boolean | null>(null);
  const [readingDetail, setReadingDetail] = useState(false);
  const wasActive = useRef(turn.execution.kind === "active");
  const presentation = turnResultPresentation(turn);
  const execution = turnExecutionPresentation(turn);
  const detailState = actions.detailLoadState(turn.turnId);
  const briefReady = turn.briefIds.every((id) => actions.briefRecord(id) !== null);
  const hasReadableBrief = turn.briefIds.length > 0 && briefReady;
  const showExecutionNotice = execution !== "running" && execution !== "completed"
    && !(execution === "waitingResult" && hasReadableBrief);
  const awaitingResult = (execution === "waitingResult" && !hasReadableBrief)
    || (turn.briefIds.length > 0 && !briefReady);
  const timingStatus = execution === "waitingResult" && hasReadableBrief ? "completed"
    : execution !== "running" && turn.briefIds.length > 0 && !briefReady && turn.execution.kind === "terminal" && turn.execution.outcome === "completed"
      ? "loadingResult" : execution;
  const autoExpanded = execution === "running" || (wasActive.current && awaitingResult);
  const expanded = manualExpanded ?? (autoExpanded || readingDetail);
  const [mounted, setMounted] = useState(expanded);
  const operator = turn.presentationClass === "operator";

  useEffect(() => {
    if (turn.execution.kind === "active") wasActive.current = true;
  }, [turn.execution.kind]);
  useEffect(() => {
    if (expanded) {
      setMounted(true);
      return;
    }
    const timer = window.setTimeout(() => setMounted(false), 240);
    return () => window.clearTimeout(timer);
  }, [expanded]);
  useEffect(() => {
    if (expanded && (turn.detail === null || turn.detail.invalidated)
      && detailState.kind !== "loading" && detailState.kind !== "error") {
      actions.onLoadDetail(turn.turnId);
    }
  }, [expanded, turn.detail, detailState.kind, turn.turnId, actions.onLoadDetail]);
  useEffect(() => {
    const updateReading = () => {
      const node = detailRef.current;
      const selection = window.getSelection();
      setReadingDetail(Boolean(node && (
        node.contains(document.activeElement) ||
        (selection && !selection.isCollapsed && node.contains(selection.anchorNode))
      )));
    };
    document.addEventListener("selectionchange", updateReading);
    document.addEventListener("focusin", updateReading);
    return () => {
      document.removeEventListener("selectionchange", updateReading);
      document.removeEventListener("focusin", updateReading);
    };
  }, []);

  return (
    <section className={`conversation-turn is-${execution}`} data-turn-id={turn.turnId}
      aria-label={t("agentPage.turnAria", { index: turn.turnIndex })}>
      {operator ? turn.inputs.map((input) => (
        <ConversationInputLine key={input.message_id} input={input} />
      )) : (
        <details className="conversation-source" data-conversation-anchor={`source:${turn.turnId}`}>
          <summary><Bot size={13} />{t(`agentPage.turnSource.${turn.presentationClass}`)}<ChevronRight size={12} /></summary>
          {turn.inputs.map((input) => <MarkdownContent key={input.message_id} text={parseInputPreview(input.preview)} compact />)}
          <span className="conversation-source-id">{`#${turn.turnIndex}`}</span>
        </details>
      )}
      <div className="conversation-response">
        <button type="button" className={`conversation-detail-toggle ${expanded ? "is-expanded" : ""}`}
          data-conversation-anchor={`process:${turn.turnId}`} aria-expanded={expanded} aria-controls={detailId}
          title={t(expanded ? "agentPage.hideExecutionProcess" : "agentPage.executionProcess")}
          onClick={() => setManualExpanded(!expanded)}>
          <ChevronRight size={14} className="conversation-disclosure-chevron" />
          {execution === "running" ? <LoaderCircle size={14} className="is-spinning" /> : null}
          <span>{t(`agentPage.turnTimingStatus.${timingStatus}`)}</span>
          <TurnElapsedTime turn={turn} />
        </button>
        <div id={detailId} ref={detailRef} className={`conversation-detail-collapse ${expanded ? "is-expanded" : ""}`}
          aria-hidden={!expanded} inert={!expanded}>
          <div className="conversation-detail-clip">
            {mounted || expanded ? <ConversationDetailPanel turn={turn} detailState={detailState} actions={actions} /> : null}
          </div>
        </div>
        {showExecutionNotice ? (
          <div className={`conversation-turn-notice is-${execution}`} role="status">
            {execution === "failed" ? <CircleAlert size={14} /> : <Clock size={14} />}
            {t(`agentPage.executionState.${execution === "waitingResult" && hasReadableBrief ? "finishingResult" : execution}`)}
          </div>
        ) : null}
        {turn.briefIds.map((briefId) => <ConversationBriefCard key={briefId} briefId={briefId} actions={actions} />)}
        {presentation.kind === "terminal_without_result" && execution === "completed" ? (
          <div className="conversation-turn-notice">{t("agentPage.turnNoBrief", { outcome: t(`agentPage.outcome.${presentation.outcome}`) })}</div>
        ) : null}
        {presentation.kind === "unavailable" ? (
          <div className="conversation-turn-notice is-error" role="note">{t("agentPage.turnResultUnavailable")}</div>
        ) : null}
      </div>
    </section>
  );
});

function ConversationInputLine({ input }: { input: TurnInputSummary }) {
  return (
    <div className="conversation-input-line" data-conversation-anchor={`input:${input.message_id}`}>
      <MarkdownContent text={parseInputPreview(input.preview)} />
    </div>
  );
}

/**
 * Message body previews are serialized MessageBody JSON (internally tagged).
 * Fall back to the raw string for non-JSON or unexpected shapes.
 */
export function parseInputPreview(preview: string): string {
  if (preview.length === 0) return "";
  try {
    const parsed = JSON.parse(preview) as {
      type?: string;
      text?: string;
      value?: unknown;
    };
    if (typeof parsed.text === "string") return parsed.text;
    if (parsed.type === "json" && parsed.value !== undefined) {
      return JSON.stringify(parsed.value, null, 2);
    }
  } catch {
    // Not JSON: treat as plain text preview.
  }
  return preview;
}

const ConversationBriefCard = memo(function ConversationBriefCard({
  briefId,
  actions,
}: {
  briefId: string;
  actions: ConversationTimelineActions;
}) {
  const { t } = useTranslation();
  const brief = actions.briefRecord(briefId);
  const loadState = actions.briefLoadState(briefId);

  useEffect(() => {
    if (brief === null && loadState === null) {
      actions.onLoadBrief(briefId);
    }
  }, [brief, loadState, briefId, actions]);

  if (brief === null) {
    if (loadState?.kind === "error") {
      return (
        <div className="conversation-brief is-error" role="alert">
          <CircleAlert size={14} />
          <span>{t("agentPage.briefLoadFailed")}</span>
          <button type="button" onClick={() => actions.onLoadBrief(briefId)}>
            <RefreshCw size={13} />
            {t("agentPage.retryBrief")}
          </button>
        </div>
      );
    }
    return (
      <div className="conversation-brief is-loading" role="status">
        <LoaderCircle size={14} className="is-spinning" />
        <span>{t("agentPage.briefLoading")}</span>
      </div>
    );
  }
  return <BriefCardBody brief={brief} />;
});

function BriefCardBody({ brief }: { brief: BriefRecord }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const created = useMemo(
    () => (brief.created_at ? new Date(brief.created_at) : null),
    [brief.created_at],
  );
  return (
    <article className={`conversation-brief is-${brief.kind}`} data-conversation-anchor={`brief:${brief.id}`}>
      <MarkdownContent
        text={brief.text}
        citations={brief.citations?.map((citation) => ({
          url: citation.url,
          ...(citation.title === undefined ? {} : { title: citation.title }),
        }))}
        compact={false}
      />
      <div className="conversation-brief-actions">
        <button type="button" aria-label={t(copied ? "agentPage.copiedReply" : "agentPage.copyReply")}
          onClick={async () => {
            try { await navigator.clipboard.writeText(brief.text); setCopied(true); }
            catch { setCopied(false); }
          }}>
          {copied ? <Check size={14} /> : <Copy size={14} />}
        </button>
        {created !== null && !Number.isNaN(created.getTime()) ? (
          <time dateTime={brief.created_at} title={created.toLocaleString()}>
            {created.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
          </time>
        ) : null}
      </div>
      {brief.attachments !== null && brief.attachments.length > 0 ? (
        <span className="conversation-brief-attachments">
          {brief.attachments.length}
        </span>
      ) : null}
    </article>
  );
}

function ConversationDetailPanel({
  turn,
  detailState,
  actions,
}: {
  turn: ConversationTurnGroup;
  detailState: ConversationDetailLoadState;
  actions: ConversationTimelineActions;
}) {
  const { t } = useTranslation();
  const detail = turn.detail;
  const [showEarlier, setShowEarlier] = useState(false);
  if (detail === null) {
    if (detailState.kind === "error") {
      return (
        <div className="conversation-detail is-error" role="alert">
          <CircleAlert size={14} />
          <span>{t("agentPage.detailLoadFailed")}</span>
          <button type="button" onClick={() => actions.onLoadDetail(turn.turnId)}>
            <RefreshCw size={13} />
            {t("agentPage.retry")}
          </button>
        </div>
      );
    }
    return (
      <div className="conversation-detail is-loading" role="status">
        <LoaderCircle size={14} className="is-spinning" />
        <span>{t("agentPage.detailLoading")}</span>
      </div>
    );
  }
  const activities = executionProcessActivities(
    detail.activities,
    turn.briefIds.flatMap((id) => {
      const brief = actions.briefRecord(id);
      return brief ? [brief.text] : [];
    }),
    turn.execution.kind === "terminal",
  );
  const onlyResult = activities.length === 0 && detail.activities.some((activity) => activity.kind === "assistant")
    && !detail.has_more && !detail.truncated;
  return (
    <div className="conversation-detail">
      {detail.invalidated ? (
        <div className="conversation-detail-invalidated" role="status">
          <span>{t("agentPage.detailStale")}</span>
          <button type="button" onClick={() => actions.onLoadDetail(turn.turnId)}>
            <RefreshCw size={13} />
            {t("common.refresh")}
          </button>
        </div>
      ) : null}
      {detail.truncated ? (
        <div className="conversation-detail-notice">{t("agentPage.detailTruncated")}</div>
      ) : null}
      {detail.has_more && detail.next_before_cursor !== null ? (
        <button
          type="button"
          className="conversation-detail-older"
          onClick={() => { setShowEarlier(true); actions.onLoadOlderActivities(turn.turnId); }}
        >
          <ChevronUpLoadMore />
          <span>{t("agentPage.loadOlderActivities")}</span>
        </button>
      ) : null}
      {!showEarlier && activities.length > 8 ? (
        <button type="button" className="conversation-detail-older" onClick={() => setShowEarlier(true)}>
          <ChevronDown size={13} />{t("agentPage.showEarlierProcess")}
        </button>
      ) : null}
      {activities.length === 0 ? (
        <div className="conversation-detail-notice">{t(turn.execution.kind === "active" ? "agentPage.awaitingActivity" : onlyResult ? "agentPage.resultOnlyProcess" : "agentPage.detailEmpty")}</div>
      ) : (
        <ol className="conversation-activities">
          {activities.filter((activity, index) =>
            showEarlier || index >= activities.length - 8 || activity.kind === "error" || activity.kind === "wait"
          ).map((activity) => (
            <ConversationActivityRow
              activity={activity}
              key={activity.id}
              onInspectActivity={actions.onInspectActivity}
              onLoadToolDetail={actions.onLoadToolDetail}
              toolDetail={actions.toolDetails?.[activity.id.slice(5)]}
              selected={actions.selectedActivityId === activity.id}
            />
          ))}
        </ol>
      )}

    </div>
  );
}

/** Deduplicate only a delivered final response, never in-flight progress or evidence. */
export function executionProcessActivities(
  activities: readonly ConversationActivity[],
  readableBriefs: readonly string[],
  terminal: boolean,
): readonly ConversationActivity[] {
  const process = activities.filter((activity) => activity.kind !== "operator");
  if (!terminal || readableBriefs.length === 0) return process;
  // Keep Markdown/code whitespace intact; only normalize line endings and outer space.
  const normalize = (text: string) => text.replace(/\r\n/g, "\n").trim();
  let lastAssistant = -1;
  for (let index = process.length - 1; index >= 0; index--) {
    if (process[index].kind === "assistant" && normalize(summarizeActivity(process[index]).display)) {
      lastAssistant = index;
      break;
    }
  }
  if (lastAssistant < 0) return process;
  const finalText = normalize(summarizeActivity(process[lastAssistant]).display);
  if (!readableBriefs.some((text) => normalize(text) === finalText)) return process;
  return process.filter((_, index) => index !== lastAssistant);
}

function ChevronUpLoadMore() {
  return <ChevronDown size={13} className="is-flipped" />;
}

function ConversationActivityRow({
  activity,
  onInspectActivity,
  onLoadToolDetail,
  toolDetail,
  selected,
}: {
  activity: ConversationActivity;
  onInspectActivity?: (activity: AgentTimelineActivity) => void;
  onLoadToolDetail?: (id: string, revision: number) => void;
  toolDetail?: ToolExecutionDetailState;
  selected?: boolean;
}) {
  const { t } = useTranslation();
  const toolId = activity.kind === "tool" && activity.id.startsWith("tool:") ? activity.id.slice(5) : undefined;
  useEffect(() => {
    // Mounted rows are the visible part of an expanded process. Retry stale
    // records on a new activity revision, including after an earlier failure.
    if (toolId && !toolDetail?.loading && (toolDetail?.conversationRevision ?? -1) < activity.revision) {
      onLoadToolDetail?.(toolId, activity.revision);
    }
  }, [toolId, activity.revision, toolDetail, onLoadToolDetail]);
  const tool = useMemo(() => toolId && toolDetail?.toolExecution
    ? toolExecutionPresentation(toolDetail.toolExecution) : undefined, [toolId, toolDetail?.toolExecution]);
  const summary = useMemo(
    () => summarizeActivity(activity),
    [activity],
  );
  const icon = activityIcon(activity);
  const label = t(`agentPage.activityKind.${activity.kind}`);
  return (
    <li className={`conversation-activity is-${activity.kind}${selected ? " is-selected" : ""}`} data-activity-id={activity.id}
      data-conversation-anchor={`activity:${activity.id}`}>
      {activity.kind === "assistant" ? (
        <div className="conversation-progress-text" onClick={(event) => {
          if (event.target instanceof Element && event.target.closest("a, button")) return;
          if (window.getSelection()?.isCollapsed === false) return;
          onInspectActivity?.(conversationActivityToInspectorActivity(activity));
        }}>
          {summary.display ? <MarkdownContent text={summary.display} /> : <span>{t("agentPage.activitySummaryUnavailable")}</span>}
          {onInspectActivity ? (
            <button type="button" className="conversation-progress-inspect"
              aria-label={t("agentPage.inspectActivity")}
              onClick={() => onInspectActivity(conversationActivityToInspectorActivity(activity))}>
              <ExternalLink size={13} />
            </button>
          ) : null}
        </div>
      ) : (
        <button type="button" className="conversation-activity-row"
          disabled={!onInspectActivity}
          onClick={() => onInspectActivity?.(conversationActivityToInspectorActivity(activity))}>
          <span className="conversation-activity-icon">{icon}</span>
          {tool ? (
            <span className="conversation-tool-content">
              <span className={`conversation-activity-summary${tool.command ? " is-command" : ""}`}>{tool.text}</span>
              <span className="conversation-tool-meta">
                {tool.toolName} · {tool.status}
                {tool.durationMs != null ? ` · ${tool.durationMs < 1000 ? `${tool.durationMs}ms` : `${(tool.durationMs / 1000).toFixed(1)}s`}` : ""}
              </span>
            </span>
          ) : <span className="conversation-activity-summary">{summary.display || label}</span>}
          <ChevronRight size={13} />
        </button>
      )}
    </li>
  );
}

/** Adapt a read-model activity into the Debug inspector's activity shape. */
export function conversationActivityToInspectorActivity(
  activity: ConversationActivity,
): AgentTimelineActivity {
  const summary = summarizeActivity(activity);
  return {
    id: activity.id,
    kind:
      activity.kind === "operator" ||
      activity.kind === "assistant" ||
      activity.kind === "tool"
        ? activity.kind
        : "event",
    label: activity.kind,
    body: summary.plain,
    timestamp: "",
    meta: `#${activity.key.event_seq} r${activity.revision}`,
    minDisplayLevel: "info",
    sourceIds: [activity.id],
    ...(activity.kind === "tool" && activity.id.startsWith("tool:") && activity.id.length > 5
      ? { stateObjectRef: { kind: "tool_execution" as const, id: activity.id.slice(5), toolName: "Tool", status: "unknown" } }
      : {}),
  };
}

function activityIcon(activity: ConversationActivity) {
  switch (activity.kind) {
    case "operator":
      return <User size={12} />;
    case "assistant":
      return <Bot size={12} />;
    case "tool":
      return <Wrench size={12} />;
    case "wait":
      return <Clock size={12} />;
    case "error":
      return <CircleAlert size={12} />;
  }
}

interface ActivitySummary {
  readonly display: string;
  readonly plain: string;
}

/**
 * Assistant/tool summaries are display text. Older daemons may send serialized
 * assistant blocks; operator previews still carry MessageBody JSON.
 */
export function summarizeActivity(activity: ConversationActivity): ActivitySummary {
  const raw = activity.summary;
  if (activity.kind === "assistant") {
    const text = summarizeAssistantSummary(raw);
    return { display: text, plain: text };
  }
  if (activity.kind === "operator") {
    const text = parseInputPreview(raw);
    return { display: text, plain: text };
  }
  return { display: raw, plain: raw };
}

function summarizeAssistantSummary(raw: string): string {
  // Older daemons send truncated transcript JSON. Never expose its raw
  // provider state, thinking blocks, or signatures in text or tooltips.
  if (!/^\s*\{\s*"(?:blocks|role|type|data|active_model|checkpoint|signature|thinking)"\s*:/.test(raw)) return raw;
  try {
    const parsed = JSON.parse(raw) as { blocks?: Array<{ type?: string; text?: string }> };
    if (!Array.isArray(parsed.blocks)) return "";
    return parsed.blocks.filter((block) => block && block.type === "text" && typeof block.text === "string")
      .map((block) => block.text).join("\n\n");
  } catch {
    return "";
  }
}
