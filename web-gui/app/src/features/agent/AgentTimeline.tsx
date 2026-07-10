import {
  Bot,
  CircleAlert,
  Clock,
  Diamond,
  ImageIcon,
  ChevronRight,
  Equal,
  RefreshCw,
  Sparkles,
  User,
  Zap,
} from "lucide-react";
import { memo, useMemo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import i18next from "i18next";

import { MarkdownContent, parseWorkspaceImageRef, type WorkspaceImageRef } from "../../components/MarkdownContent";
import { useRuntimeStore } from "../../runtime/runtime-store";
import type { AgentSummary, AgentTimelineActivity, AgentTimelineItem, DisplayLevel } from "../../runtime/types";
import { TimelineTurn, sortableActivityTime, timelineItemToWorkingActivity } from "./timeline-utils";

export const TimelineTurnGroup = memo(function TimelineTurnGroup({
  turn,
  displayLevel,
  onOpenInspector,
  onInspectActivity,
  selectedActivityId,
  targetTimelineItemId,
}: {
  turn: TimelineTurn;
  displayLevel: DisplayLevel;
  onOpenInspector: () => void;
  onInspectActivity: (activity: AgentTimelineActivity) => void;
  selectedActivityId?: string;
  targetTimelineItemId?: string;
}) {
  const { t } = useTranslation();
  return (
    <section className="timeline-turn" aria-label={turn.label}>
      <div className="timeline-turn-rail" aria-hidden="true" />
      <div className="timeline-turn-body">
        <div className="timeline-turn-header">
          {turn.kind === "runtime" ? (
            <span
              className="timeline-turn-icon"
              data-tooltip={turn.label}
              data-tooltip-pos="bottom"
            >
              <Bot size={14} aria-label={turn.label} />
            </span>
          ) : (
            <span
              className="timeline-turn-icon"
              data-tooltip={turn.label}
              data-tooltip-pos="bottom"
            >
              <User size={14} aria-label={turn.label} />
            </span>
          )}
          <time>{formatDisplayTime(turn.timestamp)}</time>
        </div>
        {turn.items.map((item, index) => (
          <TimelineMessage
            compactAssistant={item.kind === "assistant" && turn.items[index - 1]?.kind === "assistant"}
            displayLevel={displayLevel}
            item={item}
            key={item.id}
            onOpenInspector={onOpenInspector}
            onInspectActivity={onInspectActivity}
            selectedActivityId={selectedActivityId}
            targetTimelineItemId={targetTimelineItemId}
          />
        ))}
      </div>
    </section>
  );
});

const TimelineMessage = memo(function TimelineMessage({
  item,
  compactAssistant,
  displayLevel,
  onOpenInspector,
  onInspectActivity,
  selectedActivityId,
  targetTimelineItemId,
}: {
  item: AgentTimelineItem;
  compactAssistant: boolean;
  displayLevel: DisplayLevel;
  onOpenInspector: () => void;
  onInspectActivity: (activity: AgentTimelineActivity) => void;
  selectedActivityId?: string;
  targetTimelineItemId?: string;
}) {
  const { t } = useTranslation();
  const isRuntimeItem = isRuntimeActivityItem(item);
  const selectedAgentId = useRuntimeStore((s) => s.selectedAgentId);
  const showFileBrowser = useRuntimeStore((s) => s.showFileBrowser);
  const activities =
    isRuntimeItem && item.meta === "activity"
      ? (item.activities ?? [])
      : isRuntimeItem
        ? [timelineItemToWorkingActivity(item), ...(item.activities ?? [])]
        : (item.activities ?? []);
  if (isRuntimeItem) {
    return (
      <article
        className={`message activity-message${targetTimelineItemId === item.id ? " is-targeted" : ""}`}
        data-timeline-item-id={item.id}
      >
        {activities.length ? (
          <ActivityTrail
            activities={activities}
            displayLevel={displayLevel}
            onOpenInspector={onOpenInspector}
            onInspectActivity={onInspectActivity}
            selectedActivityId={selectedActivityId}
          />
        ) : null}
      </article>
    );
  }

  const timelineMeta = formatTimelineMeta(item.meta, displayLevel);
  const inspectItem = () => onInspectActivity(timelineItemToWorkingActivity(item));
  const workspaceImageRefs = useMemo(
    () => extractWorkspaceImageRefs(item.body),
    [item.body],
  );
  const openFirstImage = () => {
    const first = workspaceImageRefs[0];
    if (!first || !selectedAgentId) return;
    showFileBrowser(selectedAgentId, first.workspaceId, undefined, undefined, first.path);
  };
  return (
    <article
      className={`message ${item.kind}${compactAssistant ? " is-compact" : ""}${targetTimelineItemId === item.id ? " is-targeted" : ""}`}
      data-timeline-item-id={item.id}
    >
      <div className="bubble">
        <TimelineItemContent item={item} />
        <TimelineItemDetail detail={item.detail} />
      </div>
      <div className="message-actions" aria-label={t("agent.messageActions")}>
        <button className="message-action" type="button" title={t("agent.copyMessage")} onClick={() => copyMessageText(item.body)}>
          ⧉
        </button>
        {workspaceImageRefs.length > 0 ? (
          <button className="message-action" type="button" title={t("fileBrowser.openInFileBrowser")} onClick={openFirstImage}>
            <ImageIcon size={14} />
          </button>
        ) : null}
        <button className="message-action" type="button" title={t("agent.inspectMessage")} onClick={inspectItem}>
          ⓘ
        </button>
      </div>
      {activities.length ? (
        <ActivityTrail
          activities={activities}
          displayLevel={displayLevel}
          onOpenInspector={onOpenInspector}
          onInspectActivity={onInspectActivity}
          selectedActivityId={selectedActivityId}
        />
      ) : null}
      {!compactAssistant && timelineMeta ? (
        <div className="message-meta">
          <span>{timelineMeta}</span>
        </div>
      ) : null}
    </article>
  );
});

function copyMessageText(text: string): void {
  if (!navigator.clipboard) return;
  void navigator.clipboard.writeText(text);
}
function extractWorkspaceImageRefs(text: string): WorkspaceImageRef[] {
  const refs: WorkspaceImageRef[] = [];
  const re = /workspace:\/\/[^\s"')\]]+/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(text)) !== null) {
    const ref = parseWorkspaceImageRef(match[0]);
    if (ref) refs.push(ref);
  }
  return refs;
}
function TimelineItemContent({ item }: { item: AgentTimelineItem }) {
  return <MarkdownContent text={item.body} compact={false} />;
}

function TimelineItemDetail({ detail, compact = false }: { detail?: AgentTimelineItem["detail"]; compact?: boolean }) {
  if (!detail) return null;
  if (compact) {
    return (
      <details className={`message-detail ${detail.tone ?? "data"} is-collapsed`}>
        <summary>{detail.label}</summary>
        <pre>{detail.text}</pre>
      </details>
    );
  }
  return (
    <div className={`message-detail ${detail.tone ?? "data"}`}>
      <span>{detail.label}</span>
      <pre>{detail.text}</pre>
    </div>
  );
}

function isRuntimeActivityItem(item: Pick<AgentTimelineItem, "kind">): boolean {
  return item.kind === "tool" || item.kind === "event" || item.kind === "system";
}

function ActivityTrail({
  activities,
  displayLevel,
  onOpenInspector,
  onInspectActivity,
  selectedActivityId,
}: {
  activities: AgentTimelineActivity[];
  displayLevel: DisplayLevel;
  onOpenInspector: () => void;
  onInspectActivity: (activity: AgentTimelineActivity) => void;
  selectedActivityId?: string;
}) {
  const { t } = useTranslation();
  const visibleActivities = activities;
  const hiddenCount = activities.length - visibleActivities.length;

  return (
    <div className="activity-trail" aria-label={t("agent.agentActivity")}>
      {visibleActivities.map((activity) => {
        const row = (
          <button
            className="activity-row"
            type="button"
            aria-pressed={selectedActivityId === activity.id}
            onClick={() => onInspectActivity(activity)}
          >
            <span className="activity-icon" aria-label={activity.label} title={activity.label}>
              {activityIcon(activity)}
            </span>
            <span className="activity-body">{activity.body}</span>
            {activity.stateEvolution ? (
              <span className="activity-state-evolution">{activity.stateEvolution.map((s) => s.replace(/^Task\s+/, "")).join(" → ")}</span>
            ) : null}
          </button>
        );

        return (
          <div className={`activity-item ${activity.kind}${selectedActivityId === activity.id ? " is-selected" : ""}`} key={activity.id}>
            {row}
            {displayLevel === "debug" ? (
              <div className="activity-meta">
                <span>{activity.meta}</span>
              </div>
            ) : null}
            {displayLevel === "debug" ? <TimelineItemDetail detail={activity.detail} /> : null}
          </div>
        );
      })}
      {hiddenCount > 0 ? <div className="activity-more">{t("agent.earlierActivities", { count: hiddenCount })}</div> : null}
    </div>
  );
}

function activityIcon(activity: AgentTimelineActivity): ReactNode {
  const text = `${activity.label} ${activity.meta} ${activity.detail?.tone ?? ""}`;
  if (/failed|error|exit\s+[1-9]/i.test(text)) return <CircleAlert size={12} />;
  if (/wait/i.test(text)) return <Clock size={12} />;
  if (activity.detail?.tone === "diff" || /patch/i.test(text)) return <Diamond size={12} />;
  if (activity.detail?.tone === "command" || /command|exec/i.test(text)) return <ChevronRight size={12} />;
  if (activity.detail?.tone === "output") return <Equal size={12} />;
  if (activity.kind === "tool") return <Zap size={12} />;
  if (activity.kind === "event") return <RefreshCw size={12} />;
  return <CircleAlert size={12} />;
}

export function WorkingIndicator({
  activities,
  agent,
  displayLevel,
  onInspectActivity,
  onOpenOverview,
}: {
  activities: AgentTimelineActivity[];
  agent: AgentSummary;
  displayLevel: DisplayLevel;
  onInspectActivity: (activity: AgentTimelineActivity) => void;
  onOpenOverview: () => void;
}) {
  const { t } = useTranslation();
  const parts = [
    agent.currentWork?.objective,
    agent.activeTaskCount ? `` : undefined,
  ].filter(Boolean);

  if (displayLevel !== "info" || activities.length === 0) {
    return (
      <button className="working-indicator compact" type="button" onClick={onOpenOverview}>
        <span className="working-activity-dot" aria-hidden="true" />
        <strong>{t("agent.working")}</strong>
        {parts.length ? <span>{parts.join(" · ")}</span> : null}
      </button>
    );
  }

  return (
    <div className="working-indicator detail">
      <button className="working-activity-header" type="button" onClick={onOpenOverview}>
        <span className="working-activity-dot" aria-hidden="true" />
        <strong>{t("agent.working")}</strong>
        {parts.length ? <small>{parts.join(" · ")}</small> : null}
      </button>
      <div className="working-activity-list">
        {activities.map((activity) => (
          <button
            className={`working-activity-item ${activity.kind} slot-${workingActivitySlot(activity)}`}
            key={activity.id}
            type="button"
            onClick={() => onInspectActivity(activity)}
          >
            <span className="working-activity-icon" aria-label={workingActivityLabel(activity)} title={workingActivityLabel(activity)}>
              {workingActivityIcon(activity)}
            </span>
            <span>{workingActivityBody(activity)}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

function workingActivitySlot(activity: AgentTimelineActivity): "assistant" | "action" {
  return activity.kind === "assistant" ? "assistant" : "action";
}

function workingActivityLabel(activity: AgentTimelineActivity): string {
  return workingActivitySlot(activity) === "assistant" ? i18next.t("agent.assistantMessage") : i18next.t("agent.action");
}

function workingActivityIcon(activity: AgentTimelineActivity): ReactNode {
  return workingActivitySlot(activity) === "assistant" ? <Sparkles size={12} /> : <ChevronRight size={12} />;
}

function workingActivityBody(activity: AgentTimelineActivity): string {
  if (workingActivitySlot(activity) === "action") {
    return trimActivityLine(activity.body || activity.label, 120);
  }
  const detail = activity.detail?.text
    ?.split("\n")
    .map((line) => line.trim())
    .find(Boolean);
  return trimActivityLine(detail || activity.body || activity.label, 120);
}

function trimActivityLine(value: string, maxLength: number): string {
  const normalized = value.replace(/\s+/g, " ").trim();
  if (normalized.length <= maxLength) return normalized;
  return `${normalized.slice(0, Math.max(0, maxLength - 1)).trimEnd()}…`;
}

function formatDisplayTime(value: string): string {
  if (!value) return "—";
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return value || "—";
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  }).format(parsed);
}

function formatTimelineMeta(meta: string, displayLevel: DisplayLevel): string {
  if (isLowValueAssistantEventMeta(meta)) return "";
  if (displayLevel === "debug") return `${meta} · debug`;
  const parts = meta
    .split(" · ")
    .map((part) => part.trim())
    .filter((part) => part && !/^event #\d+$/i.test(part));
  if (displayLevel === "verbose") return parts.join(" · ") || meta.split(" · ")[0] || meta;
  return parts[0] || meta;
}

function isLowValueAssistantEventMeta(meta: string): boolean {
  return meta.startsWith("assistant_round_recorded") || meta.startsWith("brief_created");
}
