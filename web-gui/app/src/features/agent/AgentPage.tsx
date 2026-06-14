import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent } from "react";

import { MarkdownContent } from "../../components/MarkdownContent";
import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import { filterTimelineByDisplayLevel } from "../../runtime/session-reducer";
import type {
  AgentDetail,
  AgentSummary,
  AgentTimelineActivity,
  AgentTimelineItem,
  DisplayLevel,
  RuntimeModelCatalog,
  RuntimeModelOption,
} from "../../runtime/types";

interface AgentPageProps {
  agent: AgentSummary;
  detail: AgentDetail | null;
  displayLevel: DisplayLevel;
  sendingPrompt: boolean;
  hasOlderEvents: boolean;
  loadingOlderEvents: boolean;
  promptError?: string;
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  modelCatalogError?: string;
  historyError?: string;
  onRefreshModels: () => Promise<void>;
  onSetModel: (model: string, reasoningEffort?: string) => Promise<void>;
  onClearModel: () => Promise<void>;
  onLoadOlderEvents: () => Promise<void>;
  onSendPrompt: (text: string) => Promise<void>;
  onOpenInspector: () => void;
  onInspectActivity: (activity: AgentTimelineActivity) => void;
  selectedActivityId?: string;
}

const DEFAULT_INFO_TIMELINE_ITEM_LIMIT = 12;
const DEFAULT_VERBOSE_TIMELINE_ITEM_LIMIT = 160;
const DEFAULT_DEBUG_TIMELINE_ITEM_LIMIT = 220;
const HISTORY_PAGE_VISIBLE_INCREMENT = 80;
const TOP_SCROLL_THRESHOLD = 16;

export function AgentPage({
  agent,
  detail,
  displayLevel,
  sendingPrompt,
  hasOlderEvents,
  loadingOlderEvents,
  promptError,
  modelCatalog,
  modelCatalogLoading,
  modelCatalogError,
  historyError,
  onRefreshModels,
  onSetModel,
  onClearModel,
  onLoadOlderEvents,
  onSendPrompt,
  onOpenInspector,
  onInspectActivity,
  selectedActivityId,
}: AgentPageProps) {
  const [prompt, setPrompt] = useState("");
  const [modelPickerOpen, setModelPickerOpen] = useState(false);
  const [changingModel, setChangingModel] = useState<string | null>(null);
  const [visibleTimelineItemLimit, setVisibleTimelineItemLimit] = useState(() => defaultTimelineItemLimit("info"));
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const preserveScrollRef = useRef<{ height: number; top: number } | null>(null);
  const stickToBottomRef = useRef(true);
  const activeAgent = detail?.agent ?? agent;
  const sourceTimeline = useMemo(() => detail?.timeline ?? fallbackTimeline(activeAgent), [activeAgent, detail?.timeline]);
  const timeline = useMemo(
    () =>
      filterTimelineByDisplayLevel(sourceTimeline, displayLevel, {
        itemLimit: visibleTimelineItemLimit,
      }),
    [displayLevel, sourceTimeline, visibleTimelineItemLimit],
  );
  const isWorking = isAgentWorking(activeAgent, sendingPrompt);
  const workingActivities = useMemo(() => (isWorking ? collectWorkingActivitiesForCurrentTurn(sourceTimeline) : []), [isWorking, sourceTimeline]);
  const timelineTurns = useMemo(() => groupTimelineTurns(timeline), [timeline]);
  const trimmedPrompt = prompt.trim();
  const canSendPrompt = trimmedPrompt.length > 0 && !sendingPrompt;
  const newestTimelineItem = timeline[timeline.length - 1];
  const timelineVersion = `${timeline.length}:${newestTimelineItem?.id ?? ""}:${timeline[0]?.id ?? ""}:${detail?.events?.length ?? 0}:${hasOlderEvents}`;
  const hasHiddenTimelineItems = timeline.length >= visibleTimelineItemLimit && sourceTimeline.length > visibleTimelineItemLimit;

  useEffect(() => {
    setVisibleTimelineItemLimit(defaultTimelineItemLimit(displayLevel));
    setModelPickerOpen(false);
  }, [activeAgent.id, displayLevel]);

  useLayoutEffect(() => {
    const list = messageListRef.current;
    if (!list) return;

    const preserved = preserveScrollRef.current;
    if (preserved) {
      list.scrollTop = list.scrollHeight - preserved.height + preserved.top;
      preserveScrollRef.current = null;
      return;
    }

    if (stickToBottomRef.current) {
      list.scrollTop = list.scrollHeight;
    }
  }, [timelineVersion]);

  async function sendDraftPrompt() {
    if (!canSendPrompt) return;
    try {
      await onSendPrompt(trimmedPrompt);
      setPrompt("");
    } catch {
      // Keep the draft in place; runtime-store exposes the user-facing error.
    }
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await sendDraftPrompt();
  }

  async function handleComposerKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return;
    event.preventDefault();
    await sendDraftPrompt();
  }

  function handleMessageListScroll() {
    const list = messageListRef.current;
    if (!list) return;
    stickToBottomRef.current = list.scrollHeight - list.scrollTop - list.clientHeight < 96;
  }

  async function handleLoadOlderEvents() {
    const list = messageListRef.current;
    if (list) {
      preserveScrollRef.current =
        list.scrollTop > TOP_SCROLL_THRESHOLD ? { height: list.scrollHeight, top: list.scrollTop } : null;
      stickToBottomRef.current = false;
    }
    setVisibleTimelineItemLimit((limit) => limit + HISTORY_PAGE_VISIBLE_INCREMENT);
    try {
      await onLoadOlderEvents();
    } catch {
      setVisibleTimelineItemLimit((limit) =>
        Math.max(defaultTimelineItemLimit(displayLevel), limit - HISTORY_PAGE_VISIBLE_INCREMENT),
      );
      preserveScrollRef.current = null;
    }
  }

  function toggleModelPicker() {
    const opening = !modelPickerOpen;
    setModelPickerOpen(opening);
    if (opening && !modelCatalogLoading && modelCatalog.options.length === 0) {
      void onRefreshModels();
    }
  }

  async function handleSelectModel(option: RuntimeModelOption) {
    if (!option.available || changingModel) return;
    setChangingModel(option.model);
    try {
      await onSetModel(option.model);
      setModelPickerOpen(false);
    } catch {
      // Store exposes the user-facing error.
    } finally {
      setChangingModel(null);
    }
  }

  async function handleClearModel() {
    if (changingModel) return;
    setChangingModel("runtime-default");
    try {
      await onClearModel();
      setModelPickerOpen(false);
    } catch {
      // Store exposes the user-facing error.
    } finally {
      setChangingModel(null);
    }
  }

  return (
    <section className="page agent-page" aria-label="Agent conversation">
      <div className="agent-workbench">
        <section className="conversation-pane">
          <div className="message-list" ref={messageListRef} onScroll={handleMessageListScroll}>
            {hasOlderEvents || hasHiddenTimelineItems ? (
              <div className="history-loader">
                <Button type="button" size="sm" variant="secondary" disabled={loadingOlderEvents} onClick={handleLoadOlderEvents}>
                  {loadingOlderEvents ? "Loading earlier…" : "Load earlier"}
                </Button>
              </div>
            ) : null}
            {historyError ? (
              <div className="history-status" role="alert">
                {historyError}
              </div>
            ) : null}
            {timelineTurns.map((turn) => (
              <TimelineTurnGroup
                displayLevel={displayLevel}
                key={turn.id}
                onOpenInspector={onOpenInspector}
                onInspectActivity={onInspectActivity}
                selectedActivityId={selectedActivityId}
                turn={turn}
              />
            ))}
            {displayLevel === "info" && isWorking && workingActivities.length > 0 ? (
              <WorkingActivityPanel activities={workingActivities} />
            ) : null}
            {displayLevel !== "info" && isWorking ? <WorkingStatusMarker agent={activeAgent} /> : null}
            {timeline.length === 0 ? (
              <EmptyState
                className="conversation-empty"
                icon="↵"
                title="No conversation activity yet"
                description={
                  displayLevel === "info"
                    ? "Send the first operator message, or switch to Verbose/Debug if you want to inspect low-level runtime events."
                    : "No runtime events are available for this agent yet. Try refreshing the session or sending an operator message."
                }
              />
            ) : null}
          </div>

          <form className="composer" aria-label={`Send operator input to ${activeAgent.id}`} onSubmit={handleSubmit}>
            <textarea
              rows={2}
              placeholder={`Send operator input to ${activeAgent.id}...`}
              value={prompt}
              disabled={sendingPrompt}
              onChange={(event) => setPrompt(event.target.value)}
              onKeyDown={handleComposerKeyDown}
            />
            {promptError ? (
              <div className="composer-status" role="alert">
                {promptError}
              </div>
            ) : null}
            <div className="composer-toolbar">
              <div className="composer-left">
                <Button type="button" size="icon" variant="ghost" aria-label="Attach">
                  ＋
                </Button>
              </div>
              <div className="composer-right">
                <div className="model-picker">
                  <Button className="model-button" type="button" variant="secondary" aria-expanded={modelPickerOpen} onClick={toggleModelPicker}>
                    <span>{shortModelLabel(activeAgent.model)}</span>
                    {activeAgent.modelSource === "agent_override" ? <small>override</small> : null}
                    <span aria-hidden="true">⌄</span>
                  </Button>
                  {modelPickerOpen ? (
                    <div className="model-menu" role="dialog" aria-label="Switch agent model">
                      <div className="model-menu-header">
                        <div>
                          <strong>Switch model</strong>
                          <span>Applies immediately when idle; otherwise on the next run.</span>
                        </div>
                        <Button type="button" size="sm" variant="ghost" disabled={modelCatalogLoading} onClick={() => void onRefreshModels()}>
                          {modelCatalogLoading ? "Loading…" : "Refresh"}
                        </Button>
                      </div>
                      {modelCatalogError ? (
                        <div className="model-picker-status" role="alert">
                          {modelCatalogError}
                        </div>
                      ) : null}
                      <button
                        className={`model-option ${activeAgent.modelSource !== "agent_override" ? "is-active" : ""}`}
                        type="button"
                        disabled={changingModel !== null || activeAgent.modelSource !== "agent_override"}
                        onClick={handleClearModel}
                      >
                        <span>
                          <strong>Runtime default</strong>
                          <small>Clear agent override</small>
                        </span>
                        {changingModel === "runtime-default" ? <em>Saving…</em> : null}
                      </button>
                      <div className="model-options" role="listbox" aria-label="Available models">
                        {modelCatalog.options.map((option) => (
                          <button
                            className={`model-option ${option.model === activeAgent.model ? "is-active" : ""}`}
                            key={option.model}
                            type="button"
                            disabled={!option.available || changingModel !== null}
                            title={option.unavailableReason ?? option.model}
                            onClick={() => void handleSelectModel(option)}
                          >
                            <span>
                              <strong>{option.displayName}</strong>
                              <small>{option.model}</small>
                            </span>
                            <span className="model-option-meta">
                              {option.supportsReasoningEffort ? <small>reasoning</small> : null}
                              {!option.available ? <small>unavailable</small> : null}
                              {changingModel === option.model ? <em>Saving…</em> : null}
                            </span>
                          </button>
                        ))}
                      </div>
                      {!modelCatalogLoading && modelCatalog.options.length === 0 ? (
                        <EmptyState
                          className="model-picker-empty"
                          icon="⌁"
                          title="No model catalog yet"
                          description="Refresh the runtime model list, or keep using the current agent model."
                        />
                      ) : null}
                    </div>
                  ) : null}
                </div>
                <Button className="send-button" type="submit" size="icon" variant="accent" aria-label="Send" disabled={!canSendPrompt}>
                  {sendingPrompt ? "…" : "↑"}
                </Button>
              </div>
            </div>
          </form>
        </section>
      </div>
    </section>
  );
}

function shortModelLabel(model: string): string {
  const parts = model.split("/");
  return parts[parts.length - 1] || model;
}

function defaultTimelineItemLimit(displayLevel: DisplayLevel): number {
  if (displayLevel === "debug") return DEFAULT_DEBUG_TIMELINE_ITEM_LIMIT;
  if (displayLevel === "verbose") return DEFAULT_VERBOSE_TIMELINE_ITEM_LIMIT;
  return DEFAULT_INFO_TIMELINE_ITEM_LIMIT;
}

function isAgentWorking(agent: AgentSummary, sendingPrompt: boolean): boolean {
  const lifecycle = agent.lifecycle.toLowerCase();
  return sendingPrompt || Boolean(agent.currentRunId) || lifecycle === "awake-running";
}

function collectWorkingActivitiesForCurrentTurn(timeline: AgentTimelineItem[]): AgentTimelineActivity[] {
  let currentTurnStart = -1;
  for (let index = timeline.length - 1; index >= 0; index -= 1) {
    if (timeline[index]?.kind === "operator") {
      currentTurnStart = index;
      break;
    }
  }
  return collectWorkingActivities(currentTurnStart >= 0 ? timeline.slice(currentTurnStart + 1) : timeline);
}

function collectWorkingActivities(timeline: AgentTimelineItem[]): AgentTimelineActivity[] {
  const byId = new Map<string, AgentTimelineActivity>();
  for (const item of timeline) {
    if (isLiveWorkingActivity(item)) {
      byId.set(item.id, timelineItemToWorkingActivity(item));
    }
    for (const activity of item.activities ?? []) {
      if (!isLiveWorkingActivity(activity)) continue;
      byId.set(activity.id, activity);
    }
  }
  const latestBySlot = new Map<"assistant" | "action", AgentTimelineActivity>();
  for (const activity of byId.values()) {
    const slot = activity.kind === "assistant" ? "assistant" : "action";
    const current = latestBySlot.get(slot);
    if (!current || sortableActivityTime(activity.timestamp) >= sortableActivityTime(current.timestamp)) {
      latestBySlot.set(slot, activity);
    }
  }
  return Array.from(latestBySlot.values()).sort(
    (left, right) => sortableActivityTime(left.timestamp) - sortableActivityTime(right.timestamp),
  );
}

function isLiveWorkingActivity(activity: Pick<AgentTimelineActivity, "label" | "meta" | "minDisplayLevel">): boolean {
  if (activity.minDisplayLevel === "info") return false;
  const eventType = activity.meta.split(" · ")[0];
  return (
    eventType === "assistant_round_recorded" ||
    eventType === "text_only_round_observed" ||
    eventType === "message_processing_started" ||
    eventType === "tool_executed" ||
    eventType === "tool_execution_failed"
  );
}

function timelineItemToWorkingActivity(item: AgentTimelineItem): AgentTimelineActivity {
  return {
    id: item.id,
    kind: item.kind,
    label: item.label,
    body: item.body,
    timestamp: item.timestamp,
    meta: item.meta,
    minDisplayLevel: item.minDisplayLevel,
    sourceIds: item.sourceIds,
    detail: item.detail,
    debug: item.debug,
  };
}

interface TimelineTurn {
  id: string;
  label: string;
  timestamp: string;
  items: AgentTimelineItem[];
}

function groupTimelineTurns(timeline: AgentTimelineItem[]): TimelineTurn[] {
  const turns: TimelineTurn[] = [];
  let current: TimelineTurn | undefined;

  for (const item of timeline) {
    if (!current || item.kind === "operator") {
      current = {
        id: item.kind === "operator" ? `turn:${item.id}` : `activity:${item.id}`,
        label: item.kind === "operator" ? "Operator turn" : "Runtime activity",
        timestamp: item.timestamp,
        items: [],
      };
      turns.push(current);
    }
    current.items.push(item);
  }

  return turns;
}

const TimelineTurnGroup = memo(function TimelineTurnGroup({
  turn,
  displayLevel,
  onOpenInspector,
  onInspectActivity,
  selectedActivityId,
}: {
  turn: TimelineTurn;
  displayLevel: DisplayLevel;
  onOpenInspector: () => void;
  onInspectActivity: (activity: AgentTimelineActivity) => void;
  selectedActivityId?: string;
}) {
  return (
    <section className="timeline-turn" aria-label={turn.label}>
      <div className="timeline-turn-rail" aria-hidden="true" />
      <div className="timeline-turn-body">
        <div className="timeline-turn-header">
          <span className="sr-only">{turn.label}</span>
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
}: {
  item: AgentTimelineItem;
  compactAssistant: boolean;
  displayLevel: DisplayLevel;
  onOpenInspector: () => void;
  onInspectActivity: (activity: AgentTimelineActivity) => void;
  selectedActivityId?: string;
}) {
  const isRuntimeItem = isRuntimeActivityItem(item);
  const activities =
    isRuntimeItem && item.meta === "activity"
      ? (item.activities ?? [])
      : isRuntimeItem
        ? [timelineItemToWorkingActivity(item), ...(item.activities ?? [])]
        : (item.activities ?? []);
  if (isRuntimeItem) {
    return (
      <article className="message activity-message">
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

  return (
    <article className={`message ${item.kind}${compactAssistant ? " is-compact" : ""}`}>
      <div className="bubble">
        <TimelineItemContent item={item} />
        <TimelineItemDetail detail={item.detail} />
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
      {!compactAssistant ? (
        <div className="message-meta">
          <span>{formatTimelineMeta(item.meta, displayLevel)}</span>
          {displayLevel !== "info" ? (
            <button className="copy-action" type="button" onClick={onOpenInspector}>
              inspect
            </button>
          ) : null}
        </div>
      ) : null}
    </article>
  );
});

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
  const visibleActivities = activities;
  const hiddenCount = activities.length - visibleActivities.length;

  return (
    <div className="activity-trail" aria-label="Agent activity">
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
          </button>
        );

        return (
          <div className={`activity-item ${activity.kind}${selectedActivityId === activity.id ? " is-selected" : ""}`} key={activity.id}>
            {row}
            {displayLevel === "debug" ? (
              <div className="activity-meta">
                <span>{activity.meta}</span>
                <button className="copy-action" type="button" onClick={onOpenInspector}>
                  inspect
                </button>
              </div>
            ) : null}
            {displayLevel === "debug" ? <TimelineItemDetail detail={activity.detail} /> : null}
          </div>
        );
      })}
      {hiddenCount > 0 ? <div className="activity-more">+{hiddenCount} earlier activities</div> : null}
    </div>
  );
}

function activityIcon(activity: AgentTimelineActivity): string {
  const text = `${activity.label} ${activity.meta} ${activity.detail?.tone ?? ""}`;
  if (/failed|error|exit\s+[1-9]/i.test(text)) return "!";
  if (/wait/i.test(text)) return "…";
  if (activity.detail?.tone === "diff" || /patch/i.test(text)) return "◇";
  if (activity.detail?.tone === "command" || /command|exec/i.test(text)) return "›";
  if (activity.detail?.tone === "output") return "≡";
  if (activity.kind === "tool") return "⌁";
  if (activity.kind === "event") return "↻";
  return "·";
}

function WorkingActivityPanel({ activities }: { activities: AgentTimelineActivity[] }) {
  return (
    <aside className="working-activity-panel" aria-label="Working activity">
      <div className="working-activity-header">
        <span>Working activity</span>
        <small>Runtime signals not shown in Info timeline</small>
      </div>
      <div className="working-activity-list">
        {activities.map((activity) => (
          <div className={`working-activity-item ${activity.kind}`} key={activity.id}>
            <span className="working-activity-dot" aria-hidden="true" />
            <strong>{workingActivityLabel(activity)}</strong>
            <span>{workingActivityBody(activity)}</span>
            <time>{formatDisplayTime(activity.timestamp)}</time>
          </div>
        ))}
      </div>
    </aside>
  );
}

function workingActivityLabel(activity: AgentTimelineActivity): string {
  return activity.kind === "assistant" ? "Assistant" : "Action";
}

function workingActivityBody(activity: AgentTimelineActivity): string {
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

function WorkingStatusMarker({ agent }: { agent: AgentSummary }) {
  const parts = [
    agent.currentWork?.objective,
    agent.activeTaskCount ? `${agent.activeTaskCount} active task${agent.activeTaskCount === 1 ? "" : "s"}` : undefined,
    agent.pending ? `${agent.pending} queued` : undefined,
  ].filter(Boolean);

  return (
    <div className="working-status-marker" role="status">
      <span className="working-activity-dot" aria-hidden="true" />
      <strong>Working</strong>
      {parts.length ? <span>{parts.join(" · ")}</span> : null}
    </div>
  );
}

function fallbackTimeline(agent: AgentSummary): AgentTimelineItem[] {
  if (!hasVisibleBrief(agent.lastBrief)) return [];

  return [
    {
      id: `${agent.id}-fallback-brief`,
      kind: "assistant",
      label: "Latest brief",
      body: agent.lastBrief,
      timestamp: agent.lastTurnTime,
      meta: "brief",
      minDisplayLevel: "info",
      sourceIds: [`${agent.id}-fallback-brief`],
      debug: JSON.stringify(agent, null, 2),
    },
  ];
}

function hasVisibleBrief(value: string): boolean {
  const normalized = value.trim().toLowerCase();
  return Boolean(normalized) && !normalized.startsWith("no recent brief");
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

function sortableActivityTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function formatTimelineMeta(meta: string, displayLevel: DisplayLevel): string {
  if (displayLevel === "debug") return `${meta} · debug`;
  const parts = meta
    .split(" · ")
    .map((part) => part.trim())
    .filter((part) => part && !/^event #\d+$/i.test(part));
  if (displayLevel === "verbose") return parts.join(" · ") || meta.split(" · ")[0] || meta;
  return parts[0] || meta;
}
