import {
  ArrowUp,
  Bot,
  CircleAlert,
  Clock,
  Diamond,
  ChevronRight,
  Equal,
  LoaderCircle,
  Paperclip,
  Sparkles,
  RefreshCw,
  Unplug,
  User,
  Zap,
} from "lucide-react";
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";

import { MarkdownContent } from "../../components/MarkdownContent";
import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import { deriveAgentDisplayStatus } from "../../runtime/agent-status";
import { debugAgentSessionEvents, filterTimelineByDisplayLevel } from "../../runtime/session-reducer";
import { useTranslation } from "react-i18next";
import i18next from "i18next";
import type { TFunction } from "i18next";
import type {
  AgentDetail,
  AgentSummary,
  AgentTimelineActivity,
  AgentTimelineItem,
  DisplayLevel,
  RuntimeModelCatalog,
  RuntimeModelOption,
} from "../../runtime/types";
import type { OperatorPromptAttachment } from "../../runtime/client";

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
  targetEventSeq?: number;
  onRefreshModels: () => Promise<void>;
  onSetModel: (model: string, reasoningEffort?: string) => Promise<void>;
  onClearModel: () => Promise<void>;
  onLoadOlderEvents: () => Promise<void>;
  onSendPrompt: (text: string, attachments?: OperatorPromptAttachment[]) => Promise<void>;
  onOpenInspector: () => void;
  onInspectActivity: (activity: AgentTimelineActivity) => void;
  selectedActivityId?: string;
}

const DEFAULT_INFO_TIMELINE_ITEM_LIMIT = 12;
const DEFAULT_VERBOSE_TIMELINE_ITEM_LIMIT = 160;
const DEFAULT_DEBUG_TIMELINE_ITEM_LIMIT = 220;
const HISTORY_PAGE_VISIBLE_INCREMENT = 80;
const TOP_SCROLL_THRESHOLD = 16;
const COMPOSER_DRAFT_STORAGE_PREFIX = "holon.webGui.composerDraft.v1";
const COMPOSER_TEXTAREA_MAX_HEIGHT = 320;

export function storedComposerDraftKey(agentId: string): string {
  return `${COMPOSER_DRAFT_STORAGE_PREFIX}:${encodeURIComponent(agentId)}`;
}

export function readStoredComposerDraft(agentId: string): string {
  if (typeof window === "undefined") return "";
  try {
    return window.localStorage.getItem(storedComposerDraftKey(agentId)) ?? "";
  } catch {
    return "";
  }
}

export function writeStoredComposerDraft(agentId: string, prompt: string): void {
  if (typeof window === "undefined") return;
  try {
    const key = storedComposerDraftKey(agentId);
    if (prompt.length > 0) {
      window.localStorage.setItem(key, prompt);
    } else {
      window.localStorage.removeItem(key);
    }
  } catch {
    // Ignore storage failures; the in-memory draft still applies.
  }
}

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read file."));
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      resolve(result.includes(",") ? result.slice(result.indexOf(",") + 1) : result);
    };
    reader.readAsDataURL(file);
  });
}

export function resizeComposerTextarea(textarea: HTMLTextAreaElement): void {
  textarea.style.height = "auto";
  const nextHeight = Math.min(textarea.scrollHeight, COMPOSER_TEXTAREA_MAX_HEIGHT);
  textarea.style.height = `${nextHeight}px`;
  textarea.style.overflowY = textarea.scrollHeight > COMPOSER_TEXTAREA_MAX_HEIGHT ? "auto" : "hidden";
}

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
  targetEventSeq,
  onRefreshModels,
  onSetModel,
  onClearModel,
  onLoadOlderEvents,
  onSendPrompt,
  onOpenInspector,
  onInspectActivity,
  selectedActivityId,
}: AgentPageProps) {
  const { t } = useTranslation();
  const [prompt, setPrompt] = useState(() => readStoredComposerDraft(agent.id));
  const [attachments, setAttachments] = useState<OperatorPromptAttachment[]>([]);
  const [modelPickerOpen, setModelPickerOpen] = useState(false);
  const [changingModel, setChangingModel] = useState<string | null>(null);
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [selectedReasoningEffort, setSelectedReasoningEffort] = useState("auto");
  const [reasoningPopoverOpen, setReasoningPopoverOpen] = useState(false);
  const [visibleTimelineItemLimit, setVisibleTimelineItemLimit] = useState(() => defaultTimelineItemLimit("info"));
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const composerTextareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const preserveScrollRef = useRef<{ height: number; top: number } | null>(null);
  const stickToBottomRef = useRef(true);
  const activeAgent = detail?.agent ?? agent;
  const sourceTimeline = detail?.timeline ?? [];
  const sourceEvents = detail?.events ?? [];
  const timeline = useMemo(
    () => {
      if (displayLevel === "debug" && sourceEvents.length > 0) {
        return debugAgentSessionEvents(sourceEvents, {
          itemLimit: visibleTimelineItemLimit,
        });
      }
      return filterTimelineByDisplayLevel(sourceTimeline, displayLevel, {
        itemLimit: visibleTimelineItemLimit,
      });
    },
    [displayLevel, sourceEvents, sourceTimeline, visibleTimelineItemLimit],
  );
  const isWorking = isAgentWorking(activeAgent, sendingPrompt, t);
  const workingActivities = useMemo(() => (isWorking ? collectWorkingActivitiesForCurrentTurn(sourceTimeline) : []), [isWorking, sourceTimeline]);
  const timelineTurns = useMemo(() => groupTimelineTurns(timeline), [timeline]);
  const targetTimelineItemId = useMemo(() => timeline.find((item) => itemHasEventSeq(item, targetEventSeq))?.id, [targetEventSeq, timeline]);
  const trimmedPrompt = prompt.trim();
  const canSendPrompt = (trimmedPrompt.length > 0 || attachments.length > 0) && !sendingPrompt;
  const newestTimelineItem = timeline[timeline.length - 1];
  const timelineVersion = `${timeline.length}:${newestTimelineItem?.id ?? ""}:${timeline[0]?.id ?? ""}:${detail?.events?.length ?? 0}:${hasOlderEvents}`;
  const hasHiddenTimelineItems = timeline.length >= visibleTimelineItemLimit && sourceTimeline.length > visibleTimelineItemLimit;
  const groupedModelOptions = useMemo(() => groupModelOptionsByProvider(modelCatalog.options), [modelCatalog.options]);
  const activeModelOption = useMemo(() => modelCatalog.options.find((option) => option.model === activeAgent.model), [activeAgent.model, modelCatalog.options]);
  const activeModelSupportsReasoning = activeModelOption?.supportsReasoningEffort ?? Boolean(activeAgent.modelReasoningEffort);
  const activeReasoningBadge = activeModelSupportsReasoning ? (activeAgent.modelReasoningEffort ?? "auto") : undefined;
  const activeModelTitle = modelButtonTitle(activeAgent.model, activeReasoningBadge, activeAgent.modelSource === "agent_override");
  const currentProvider = selectedProvider ?? activeModelOption?.provider ?? groupedModelOptions[0]?.provider ?? "runtime";
  const currentProviderModels = groupedModelOptions.find((group) => group.provider === currentProvider)?.models ?? [];

  useEffect(() => {
    setVisibleTimelineItemLimit(defaultTimelineItemLimit(displayLevel));
    setModelPickerOpen(false);
    setReasoningPopoverOpen(false);
    setSelectedProvider(null);
    setSelectedReasoningEffort(activeAgent.modelReasoningEffort ?? "auto");
  }, [activeAgent.id, displayLevel]);

  useEffect(() => {
    setPrompt(readStoredComposerDraft(activeAgent.id));
    setAttachments([]);
  }, [activeAgent.id]);

  useLayoutEffect(() => {
    const textarea = composerTextareaRef.current;
    if (textarea) {
      resizeComposerTextarea(textarea);
    }
  }, [prompt, activeAgent.id]);

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

  useLayoutEffect(() => {
    if (!targetTimelineItemId) return;
    const list = messageListRef.current;
    const target = list?.querySelector<HTMLElement>(`[data-timeline-item-id="${cssEscape(targetTimelineItemId)}"]`);
    if (!target) return;
    stickToBottomRef.current = false;
    target.scrollIntoView({ block: "center" });
  }, [targetTimelineItemId, timelineVersion]);

  async function sendDraftPrompt() {
    if (!canSendPrompt) return;
    try {
      await onSendPrompt(trimmedPrompt, attachments);
      writeStoredComposerDraft(activeAgent.id, "");
      setPrompt("");
      setAttachments([]);
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

  function handlePromptChange(value: string) {
    setPrompt(value);
    writeStoredComposerDraft(activeAgent.id, value);
  }

  async function handleAttachmentFiles(files: FileList | null) {
    if (!files || files.length === 0) return;
    const next: OperatorPromptAttachment[] = [];
    for (const file of Array.from(files)) {
      if (!file.type.startsWith("image/")) continue;
      next.push({
        kind: "image",
        name: file.name,
        mediaType: file.type,
        dataBase64: await fileToBase64(file),
      });
    }
    if (next.length > 0) {
      setAttachments((current) => [...current, ...next]);
    }
    if (fileInputRef.current) {
      fileInputRef.current.value = "";
    }
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
    if (opening) setReasoningPopoverOpen(false);
    setModelPickerOpen(opening);
    if (opening && !modelCatalogLoading && modelCatalog.options.length === 0) {
      void onRefreshModels();
    }
  }

  async function handleSelectModel(option: RuntimeModelOption, reasoningEffort = selectedReasoningEffort) {
    if (!option.available || changingModel) return;

    // When switching to a non-reasoning model, reset thinking display to auto.
    if (!option.supportsReasoningEffort) {
      setSelectedReasoningEffort("auto");
      reasoningEffort = "auto";
    }

    setChangingModel(option.model);
    try {
      await onSetModel(option.model, option.supportsReasoningEffort && reasoningEffort !== "auto" ? reasoningEffort : undefined);
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

  async function handleReasoningChange(effort: string) {
    setSelectedReasoningEffort(effort);
    if (changingModel || !activeModelSupportsReasoning) return;
    setChangingModel("reasoning:" + effort);
    try {
      await onSetModel(activeAgent.model, effort !== "auto" ? effort : undefined);
    } catch {
      // Store exposes the user-facing error.
    } finally {
      setChangingModel(null);
    }
  }

  return (
    <section className="page agent-page" aria-label={t("agent.conversationAria")}>
      <div className="agent-workbench">
        <section className="conversation-pane">
          <div className="message-list" ref={messageListRef} onScroll={handleMessageListScroll}>
            {hasOlderEvents || hasHiddenTimelineItems ? (
              <div className="history-loader">
                <Button type="button" size="sm" variant="secondary" disabled={loadingOlderEvents} onClick={handleLoadOlderEvents}>
                  {loadingOlderEvents ? t("agent.loadingEarlier") : t("agent.loadEarlier")}
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
                targetTimelineItemId={targetTimelineItemId}
                turn={turn}
              />
            ))}
            {isWorking ? (
              <WorkingIndicator
                activities={workingActivities}
                agent={activeAgent}
                displayLevel={displayLevel}
                onInspectActivity={onInspectActivity}
                onOpenOverview={onOpenInspector}
              />
            ) : null}
            {timeline.length === 0 ? (
              <EmptyState
                className="conversation-empty"
                icon="↵"
                title={t("agent.noActivity")}
                description={
                  displayLevel === "info"
                    ? t("agent.conversationEmpty")
                    : t("agent.noEventsYet")
                }
              />
            ) : null}
          </div>

          <form className="composer" aria-label={t("agent.sendInputAria", { id: activeAgent.id })} onSubmit={handleSubmit}>
            <textarea
              ref={composerTextareaRef}
              rows={2}
              placeholder={t("agent.sendInputPlaceholder", { id: activeAgent.id })}
              value={prompt}
              disabled={sendingPrompt}
              onChange={(event) => handlePromptChange(event.target.value)}
              onKeyDown={handleComposerKeyDown}
            />
            <input
              ref={fileInputRef}
              className="composer-file-input"
              type="file"
              accept="image/*"
              multiple
              disabled={sendingPrompt}
              onChange={(event) => void handleAttachmentFiles(event.target.files)}
            />
            {attachments.length > 0 ? (
              <div className="composer-attachments" aria-label="Image attachments">
                {attachments.map((attachment, index) => (
                  <span className="composer-attachment" key={`${attachment.name ?? "image"}:${index}`}>
                    {attachment.name ?? `image ${index + 1}`}
                    <button
                      type="button"
                      aria-label={`Remove ${attachment.name ?? `image ${index + 1}`}`}
                      onClick={() => setAttachments((current) => current.filter((_, itemIndex) => itemIndex !== index))}
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            ) : null}
            {promptError ? (
              <div className="composer-status" role="alert">
                {promptError}
              </div>
            ) : null}
            <div className="composer-toolbar">
              <div className="composer-right">
                <Button
                  className="attachment-button"
                  type="button"
                  size="icon"
                  variant="ghost"
                  aria-label="Attach image"
                  disabled={sendingPrompt}
                  onClick={() => fileInputRef.current?.click()}
                >
                  <Paperclip size={16} />
                </Button>
                <div className="model-picker">
                  <Button
                    className="model-button"
                    type="button"
                    variant="secondary"
                    aria-expanded={modelPickerOpen}
                    aria-label={activeModelTitle}
                    title={activeModelTitle}
                    onClick={toggleModelPicker}
                  >
                    <span className="model-button-label">{shortModelLabel(activeAgent.model)}</span>
                    <span aria-hidden="true">⌄</span>
                  </Button>
                  {activeModelSupportsReasoning ? (
                    <div className="thinking-picker">
                      <Button
                        className="thinking-button"
                        type="button"
                        variant="ghost"
                        aria-expanded={reasoningPopoverOpen}
                        aria-label={t("agent.thinkingAria", { level: activeReasoningBadge ?? "auto" })}
                        title={t("agent.thinkingLevelValue", { level: activeReasoningBadge ?? "auto" })}
                        onClick={() => setReasoningPopoverOpen((prev) => !prev)}
                      >
                        <svg className="thinking-icon" width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
                          <path d="M8 1.5L6 6H2.5L5.5 8.5L4 13L8 10L12 13L10.5 8.5L13.5 6H10L8 1.5Z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" fill="none"/>
                        </svg>
                        <small>{titleCase(activeReasoningBadge ?? "auto")}</small>
                      </Button>
                      {reasoningPopoverOpen ? (
                        <div className="thinking-popover" role="dialog" aria-label={t("agent.thinkingLevel")}>
                          <div className="reasoning-options">
                            {["auto", "low", "medium", "high", "xhigh"].map((effort) => (
                              <button
                                className={`${(activeReasoningBadge ?? "auto") === effort ? "is-active" : ""} ${changingModel === "reasoning:" + effort ? "is-saving" : ""}`}
                                key={effort}
                                type="button"
                                disabled={changingModel !== null}
                                onClick={() => void handleReasoningChange(effort)}
                              >
                                {titleCase(effort)}
                              </button>
                            ))}
                          </div>
                        </div>
                      ) : null}
                    </div>
                  ) : null}
                  {modelPickerOpen ? (
                    <div className="model-menu" role="dialog" aria-label={t("agent.switchModelAria")}>
                      <div className="model-menu-header">
                        <div>
                          <strong>{t("agent.switchModel")}</strong>
                          <span>{t("agent.switchModelHint")}</span>
                        </div>
                        <Button type="button" size="sm" variant="ghost" disabled={modelCatalogLoading} onClick={() => void onRefreshModels()}>
                          {modelCatalogLoading ? t("common.loading") : t("common.refresh")}
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
                          <strong>{t("agent.runtimeDefault")}</strong>
                          <small>{t("agent.clearOverride")}</small>
                        </span>
                        {changingModel === "runtime-default" ? <em>{t("common.saving")}</em> : null}
                      </button>
                      <div className="model-picker-grid">
                        <div className="model-picker-section model-picker-providers" aria-label={t("agent.providersAria")}>
                          <span>
                            <b>{t("agent.step1")}</b>
                            Provider
                          </span>
                          <div className="model-provider-list">
                            {groupedModelOptions.map((group) => (
                              <button
                                className={`model-provider-option ${group.provider === currentProvider ? "is-active" : ""}`}
                                key={group.provider}
                                type="button"
                                onClick={() => setSelectedProvider(group.provider)}
                              >
                                <strong>{group.provider}</strong>
                                <small>
                                  {group.availableCount}/{group.models.length} {t("agent.available")}
                                </small>
                              </button>
                            ))}
                          </div>
                        </div>
                        <div className="model-picker-section model-picker-models" aria-label={t("agent.providerModelsAria", { provider: currentProvider })}>
                          <span>
                            <b>{t("agent.step2")}</b>
                            {currentProvider} models
                          </span>
                          <div className="model-options" role="listbox" aria-label={t("agent.providerModelsAria", { provider: currentProvider })}>
                            {currentProviderModels.map((option) => (
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
                                  {option.supportsReasoningEffort ? <small>{t("agent.reasoningMeta")}</small> : null}
                                  {!option.available ? <small>{t("agent.unavailableMeta")}</small> : null}
                                  {changingModel === option.model ? <em>{t("common.saving")}</em> : null}
                                </span>
                              </button>
                            ))}
                          </div>
                        </div>
                      </div>
                      {!modelCatalogLoading && modelCatalog.options.length === 0 ? (
                        <EmptyState
                          className="model-picker-empty"
                          icon={<Unplug size={20} />}
                          title={t("agent.noModelCatalog")}
                          description={t("agent.modelRefreshDesc")}
                        />
                      ) : null}
                    </div>
                  ) : null}
                </div>
                <Button className="send-button" type="submit" size="icon" variant="accent" aria-label={t("common.send")} disabled={!canSendPrompt}>
                  {sendingPrompt ? <LoaderCircle size={16} className="animate-spin" /> : <ArrowUp size={16} />}
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

function modelButtonTitle(model: string, reasoningEffort: string | undefined, isModelOverride: boolean): string {
  const details = [model];
  if (reasoningEffort) details.push(`reasoning effort: ${reasoningEffort}`);
  if (isModelOverride) details.push("model override");
  return details.join(" · ");
}

function groupModelOptionsByProvider(options: RuntimeModelOption[]): Array<{ provider: string; availableCount: number; models: RuntimeModelOption[] }> {
  const groups = new Map<string, RuntimeModelOption[]>();
  for (const option of options) {
    const models = groups.get(option.provider) ?? [];
    models.push(option);
    groups.set(option.provider, models);
  }
  return Array.from(groups.entries())
    .map(([provider, models]) => ({
      provider,
      availableCount: models.filter((model) => model.available).length,
      models: models.sort((left, right) => Number(right.available) - Number(left.available) || left.displayName.localeCompare(right.displayName)),
    }))
    .sort((left, right) => Number(right.availableCount > 0) - Number(left.availableCount > 0) || left.provider.localeCompare(right.provider));
}

function titleCase(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

function defaultTimelineItemLimit(displayLevel: DisplayLevel): number {
  if (displayLevel === "debug") return DEFAULT_DEBUG_TIMELINE_ITEM_LIMIT;
  if (displayLevel === "verbose") return DEFAULT_VERBOSE_TIMELINE_ITEM_LIMIT;
  return DEFAULT_INFO_TIMELINE_ITEM_LIMIT;
}

function isAgentWorking(agent: AgentSummary, sendingPrompt: boolean, t: TFunction): boolean {
  return sendingPrompt || deriveAgentDisplayStatus(agent, t).tone === "running";
}

function collectWorkingActivitiesForCurrentTurn(timeline: AgentTimelineItem[]): AgentTimelineActivity[] {
  let currentTurnStart = -1;
  for (let index = timeline.length - 1; index >= 0; index -= 1) {
    if (timeline[index]?.kind === "operator" || isTurnStartedItem(timeline[index]!)) {
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
    rawEvent: item.rawEvent,
    debug: item.debug,
  };
}

interface TimelineTurn {
  id: string;
  label: string;
  kind: "operator" | "runtime";
  timestamp: string;
  items: AgentTimelineItem[];
}

function groupTimelineTurns(timeline: AgentTimelineItem[]): TimelineTurn[] {
  const turns: TimelineTurn[] = [];
  let current: TimelineTurn | undefined;

  for (const item of timeline) {
    const isTurnBoundary = isTurnStartedItem(item);
    const isOperatorBoundary = item.kind === "operator";
    if (!current || isOperatorBoundary || isTurnBoundary) {
      const triggerLabel = isTurnBoundary ? item.body : undefined;
      current = {
        id: isOperatorBoundary || isTurnBoundary ? `turn:${item.id}` : `activity:${item.id}`,
        kind: isOperatorBoundary ? "operator" : "runtime",
        label: isOperatorBoundary
          ? i18next.t("agent.operatorTurn")
          : isTurnBoundary
            ? triggerLabel || i18next.t("agent.turn")
            : i18next.t("agent.runtimeActivity"),
        timestamp: item.timestamp,
        items: isTurnBoundary ? [] : [item],
      };
      turns.push(current);
      continue;
    }
    if (isTurnStartedItem(item)) continue;
    current.items.push(item);
  }

  const nonEmpty = turns.filter((turn) => turn.items.length > 0);
  return nonEmpty.length === turns.length ? turns : nonEmpty;
}

function isTurnStartedItem(item: AgentTimelineItem): boolean {
  return item.meta.startsWith("turn_started");
}

function itemHasEventSeq(item: AgentTimelineItem, eventSeq: number | undefined): boolean {
  if (eventSeq == null) return false;
  if (rawEventSeq(item.rawEvent) === eventSeq) return true;
  return (item.activities ?? []).some((activity) => rawEventSeq(activity.rawEvent) === eventSeq);
}

function rawEventSeq(rawEvent: unknown): number | undefined {
  return typeof rawEvent === "object" && rawEvent !== null && "event_seq" in rawEvent && typeof rawEvent.event_seq === "number"
    ? rawEvent.event_seq
    : undefined;
}

function cssEscape(value: string): string {
  if (typeof CSS !== "undefined" && CSS.escape) return CSS.escape(value);
  return value.replace(/["\\]/g, "\\$&");
}

const TimelineTurnGroup = memo(function TimelineTurnGroup({
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

function WorkingIndicator({
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

function sortableActivityTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : 0;
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
