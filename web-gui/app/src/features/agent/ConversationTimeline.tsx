import {
  Bot,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  Clock,
  LoaderCircle,
  RefreshCw,
  Unplug,
  User,
  Wrench,
} from "lucide-react";
import { memo, useEffect, useMemo, useState } from "react";
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
import type { AgentTimelineActivity } from "../../runtime/types";
import {
  turnResultPresentation,
  type ConversationSessionModel,
  type ConversationTurnGroup,
} from "../../runtime/conversation-view-model";

export interface ConversationTimelineActions {
  onLoadBrief: (briefId: string) => void;
  onLoadDetail: (turnId: string) => void;
  onLoadOlderActivities: (turnId: string) => void;
  onRetry: () => void;
  onInspectActivity?: (activity: AgentTimelineActivity) => void;
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
    <div className="conversation-timeline" role="log" aria-label={t("agent.conversationAria")}>
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
      {model.pendingInputs.length > 0 ? (
        <div className="conversation-pending-inputs" aria-label={t("agentPage.pendingInputs")}>
          {model.pendingInputs.map((input) => (
            <PendingInputChip key={input.message_id} input={input} />
          ))}
        </div>
      ) : null}
      {model.turns.map((turn) => (
        <ConversationTurnCard key={turn.turnId} turn={turn} {...actions} />
      ))}
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
    <div
      className={`conversation-pending-chip is-${input.state}`}
      role="status"
      aria-label={t("agentPage.pendingInputState")}
    >
      {input.state === "assigning" ? (
        <LoaderCircle size={12} className="is-spinning" />
      ) : (
        <Clock size={12} />
      )}
      {input.preview ? (
        <MarkdownContent text={parseInputPreview(input.preview)} compact />
      ) : (
        <span>
          {input.state === "assigning"
            ? t("agentPage.pendingAssigning")
            : t("agentPage.pendingQueued")}
        </span>
      )}
    </div>
  );
}

const ConversationTurnCard = memo(function ConversationTurnCard({
  turn,
  ...actions
}: {
  turn: ConversationTurnGroup;
} & ConversationTimelineActions) {
  const { t } = useTranslation();
  const [manualExpanded, setManualExpanded] = useState<boolean | null>(null);
  const expanded = manualExpanded ?? turn.execution.kind === "active";
  const presentation = turnResultPresentation(turn);
  const detailState = actions.detailLoadState(turn.turnId);

  const toggleExpanded = () => {
    const next = !expanded;
    setManualExpanded(next);
    if (next && turn.detail === null && detailState.kind !== "loading") {
      actions.onLoadDetail(turn.turnId);
    }
  };

  return (
    <section className="timeline-turn conversation-turn" aria-label={t("agentPage.turnAria", { index: turn.turnIndex })}>
      <div className="timeline-turn-rail" aria-hidden="true" />
      <div className="timeline-turn-body">
        <div className="timeline-turn-header">
          <span className="timeline-turn-icon" data-tooltip={turn.turnId} data-tooltip-pos="bottom">
            {isOperatorTurn(turn) ? <User size={14} /> : <Bot size={14} />}
          </span>
          <span className="conversation-turn-class">
            {t(`agentPage.turnClass.${turn.presentationClass}`)}
          </span>
          <TurnStatusIcon turn={turn} presentation={presentation} />
          <time>{`#${turn.turnIndex}`}</time>
        </div>

        {turn.inputs.map((input) => (
          <ConversationInputLine key={input.message_id} input={input} />
        ))}

        {presentation.kind === "available" ? (
          presentation.briefIds.map((briefId) => (
            <ConversationBriefCard
              key={briefId}
              briefId={briefId}
              actions={actions}
            />
          ))
        ) : null}
        {presentation.kind === "terminal_without_result" ? (
          <div className="conversation-turn-notice is-muted">
            {t("agentPage.turnNoBrief", {
              outcome: t(`agentPage.outcome.${presentation.outcome}`),
            })}
          </div>
        ) : null}
        {presentation.kind === "unavailable" ? (
          <div className="conversation-turn-notice is-error" role="note">
            {t("agentPage.turnResultUnavailable")}
            {presentation.reason ? ` (${presentation.reason})` : ""}
          </div>
        ) : null}
        {presentation.kind === "pending" ? (
          <WorkingTurnIndicator />
        ) : null}

        <button
          type="button"
          className={`conversation-detail-toggle ${expanded ? "is-expanded" : ""}`}
          aria-expanded={expanded}
          onClick={toggleExpanded}
        >
          {expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
          <span>{t("agentPage.executionProcess")}</span>
          {turn.detail !== null && turn.detail.has_more ? (
            <span className="conversation-detail-more">+</span>
          ) : null}
        </button>

        {expanded ? (
          <ConversationDetailPanel
            turn={turn}
            detailState={detailState}
            actions={actions}
          />
        ) : null}
      </div>
    </section>
  );
});

function isOperatorTurn(turn: ConversationTurnGroup): boolean {
  return turn.presentationClass === "operator";
}

function TurnStatusIcon({
  turn,
  presentation,
}: {
  turn: ConversationTurnGroup;
  presentation: ReturnType<typeof turnResultPresentation>;
}) {
  const { t } = useTranslation();
  if (turn.attention?.kind === "failed") {
    return <CircleAlert size={14} className="conversation-status is-failed" aria-label={t("agentPage.turnFailed")} />;
  }
  if (presentation.kind === "pending") {
    return <LoaderCircle size={14} className="is-spinning conversation-status is-active" aria-label={t("agentPage.turnRunning")} />;
  }
  if (presentation.kind === "available" || presentation.kind === "terminal_without_result") {
    return <CircleCheck size={14} className="conversation-status is-done" aria-label={t("agentPage.turnDone")} />;
  }
  return <CircleDashed size={14} className="conversation-status is-unknown" aria-label={t("agentPage.turnUnknown")} />;
}

function WorkingTurnIndicator() {
  const { t } = useTranslation();
  return (
    <div className="conversation-working" role="status">
      <LoaderCircle size={14} className="is-spinning" />
      <span>{t("agentPage.turnWorking")}</span>
    </div>
  );
}

function ConversationInputLine({ input }: { input: TurnInputSummary }) {
  const text = useMemo(() => parseInputPreview(input.preview), [input.preview]);
  return (
    <div className="conversation-input-line">
      <User size={14} />
      <MarkdownContent text={text} compact />
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
  const created = useMemo(
    () => (brief.created_at ? new Date(brief.created_at) : null),
    [brief.created_at],
  );
  return (
    <article className={`conversation-brief is-${brief.kind}`}>
      {created !== null && !Number.isNaN(created.getTime()) ? (
        <time dateTime={brief.created_at}>{created.toLocaleTimeString()}</time>
      ) : null}
      <MarkdownContent
        text={brief.text}
        citations={brief.citations?.map((citation) => ({
          url: citation.url,
          ...(citation.title === undefined ? {} : { title: citation.title }),
        }))}
        compact={false}
      />
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
      {detail.activities.length === 0 ? (
        <div className="conversation-detail-notice">{t("agentPage.detailEmpty")}</div>
      ) : (
        <ol className="conversation-activities">
          {detail.activities.map((activity) => (
            <ConversationActivityRow
              activity={activity}
              key={activity.id}
              onInspectActivity={actions.onInspectActivity}
            />
          ))}
        </ol>
      )}
      {detail.has_more && detail.next_before_cursor !== null ? (
        <button
          type="button"
          className="conversation-detail-older"
          onClick={() => actions.onLoadOlderActivities(turn.turnId)}
        >
          <ChevronUpLoadMore />
          <span>{t("agentPage.loadOlderActivities")}</span>
        </button>
      ) : null}
    </div>
  );
}

function ChevronUpLoadMore() {
  return <ChevronDown size={13} className="is-flipped" />;
}

function ConversationActivityRow({
  activity,
  onInspectActivity,
}: {
  activity: ConversationActivity;
  onInspectActivity?: (activity: AgentTimelineActivity) => void;
}) {
  const { t } = useTranslation();
  const summary = useMemo(
    () => summarizeActivity(activity),
    [activity],
  );
  const icon = activityIcon(activity);
  const kindLabel = t(`agentPage.activityKind.${activity.kind}`);
  const inspect = onInspectActivity === undefined ? undefined : () =>
    onInspectActivity(conversationActivityToInspectorActivity(activity));
  return (
    <li
      className={`conversation-activity is-${activity.kind}${inspect === undefined ? "" : " is-inspectable"}`}
      data-activity-id={activity.id}
      {...(inspect === undefined ? {} : { onClick: inspect })}
    >
      <span className="conversation-activity-icon">{icon}</span>
      <span className="conversation-activity-kind">{kindLabel}</span>
      <span className="conversation-activity-summary" title={summary.plain}>
        {summary.display}
      </span>
      <span className="conversation-activity-seq">#{activity.key.event_seq}</span>
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
 * Activity summaries embed different payload shapes per source kind:
 * operator carries serialized MessageBody JSON, assistant carries serialized
 * transcript entry JSON (extract text blocks), tool carries a status string,
 * wait carries the waiting description.
 */
export function summarizeActivity(activity: ConversationActivity): ActivitySummary {
  const raw = activity.summary;
  if (activity.kind === "assistant") {
    return { display: summarizeAssistantSummary(raw), plain: raw };
  }
  if (activity.kind === "operator") {
    const text = parseInputPreview(raw);
    return { display: text.length > 0 ? text : raw, plain: raw };
  }
  return { display: raw, plain: raw };
}

function summarizeAssistantSummary(raw: string): string {
  try {
    const parsed = JSON.parse(raw) as {
      blocks?: Array<{ type?: string; text?: string; name?: string }>;
    };
    if (!Array.isArray(parsed.blocks)) return truncateSummary(raw);
    const parts: string[] = [];
    for (const block of parsed.blocks) {
      if (typeof block.text === "string" && block.text.length > 0) {
        parts.push(block.text);
      } else if (typeof block.name === "string") {
        parts.push(`⚡ ${block.name}`);
      }
    }
    return truncateSummary(parts.join("\n"));
  } catch {
    return truncateSummary(raw);
  }
}

function truncateSummary(value: string, max = 240): string {
  const normalized = value.replace(/\s+/g, " ").trim();
  return normalized.length > max ? `${normalized.slice(0, max)}…` : normalized;
}
