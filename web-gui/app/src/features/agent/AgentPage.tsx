import {
  ArrowUp,
  LoaderCircle,
  Paperclip,
  Unplug,
} from "lucide-react";
import {
  useEffect, useLayoutEffect, useMemo, useRef, useState,
  type DragEvent, type FormEvent, type KeyboardEvent, type MutableRefObject, type RefObject,
} from "react";
import { useVirtualizer, type VirtualItem, type Virtualizer } from "@tanstack/react-virtual";

import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import { deriveAgentDisplayStatus } from "../../runtime/agent-status";
import { debugAgentSessionEvents, filterTimelineByDisplayLevel } from "../../runtime/session-reducer";
import { TimelineTurnGroup, WorkingIndicator } from "./AgentTimeline";
import { collectWorkingActivitiesForCurrentTurn, groupTimelineTurns, itemHasEventSeq, type TimelineTurn } from "./timeline-utils";
import { useTranslation } from "react-i18next";
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
  detailLoading?: boolean;
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
  resumeRevision?: number;
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
const BOTTOM_SCROLL_THRESHOLD = 96;
const COMPOSER_DRAFT_STORAGE_PREFIX = "holon.webGui.composerDraft.v1";
const COMPOSER_TEXTAREA_MAX_HEIGHT = 320;
const MESSAGE_LIST_BOTTOM_SAFE_SPACE = 96;

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

export function attachmentKindForFile(file: Pick<File, "type">): OperatorPromptAttachment["kind"] {
  return file.type.startsWith("image/") ? "image" : "file";
}

export function resizeComposerTextarea(textarea: HTMLTextAreaElement): void {
  textarea.style.height = "auto";
  const nextHeight = Math.min(textarea.scrollHeight, COMPOSER_TEXTAREA_MAX_HEIGHT);
  textarea.style.height = `${nextHeight}px`;
  textarea.style.overflowY = textarea.scrollHeight > COMPOSER_TEXTAREA_MAX_HEIGHT ? "auto" : "hidden";
}

export interface ScrollAnchor {
  key: VirtualItem["key"];
  index: number;
  offset: number;
}

export function captureScrollAnchor(virtualItems: Pick<VirtualItem, "key" | "index" | "start" | "size">[], scrollTop: number): ScrollAnchor | null {
  const anchorItem = virtualItems.find((item) => item.start + item.size > scrollTop);
  return anchorItem
    ? { key: anchorItem.key, index: anchorItem.index, offset: Math.max(0, scrollTop - anchorItem.start) }
    : null;
}

export function restoredScrollTop(
  anchor: ScrollAnchor | null,
  anchorIndex: number | undefined,
  offsetForIndex: (index: number) => number | undefined,
  fallbackTop: number,
  contentOffset = 0,
): number {
  if (!anchor || anchorIndex == null) return fallbackTop;
  const start = offsetForIndex(anchorIndex);
  return start == null ? fallbackTop : Math.max(0, contentOffset + start + anchor.offset);
}

export function timelineLayoutRevision(turns: TimelineTurn[]): string {
  let hash = 2166136261;
  let fieldCount = 0;
  const add = (value: unknown) => {
    const text = value == null ? "" : String(value);
    fieldCount += 1;
    hash = fnv1a(`${text.length}:${text}`, hash);
  };
  const addItem = (item: AgentTimelineItem | AgentTimelineActivity) => {
    add(item.id);
    add(item.kind);
    add(item.label);
    add(item.body);
    add(item.meta);
    add(item.minDisplayLevel);
    add(item.detail?.label);
    add(item.detail?.text);
    add(item.detail?.tone);
    add(item.executionMeta?.outcome);
    add(item.executionMeta?.exitStatus);
    add(item.executionMeta?.durationMs);
    add(item.executionMeta?.outputTruncated);
    add(item.executionMeta?.taskId);
    add(item.statusTrail?.length ?? 0);
    for (const step of item.statusTrail ?? []) {
      add(step.status);
      add(step.timestamp);
    }
  };

  for (const turn of turns) {
    add(turn.id);
    add(turn.kind);
    add(turn.label);
    add(turn.timestamp);
    add(turn.items.length);
    for (const item of turn.items) {
      addItem(item);
      add(item.activities?.length ?? 0);
      for (const activity of item.activities ?? []) {
        addItem(activity);
      }
    }
  }

  return `${turns.length}:${fieldCount}:${hash.toString(36)}`;
}

function fnv1a(text: string, initialHash: number): number {
  let hash = initialHash;
  for (let index = 0; index < text.length; index += 1) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return hash >>> 0;
}

function useReconciledVirtualMeasurements({
  virtualizer,
  scrollElementRef,
  contentElementRef,
  layoutRevision,
  stickToBottomRef,
  pendingAnchorRef,
  anchorIndexByKey,
  scrollToBottom,
}: {
  virtualizer: Virtualizer<HTMLDivElement, Element>;
  scrollElementRef: RefObject<HTMLDivElement | null>;
  contentElementRef: RefObject<HTMLDivElement | null>;
  layoutRevision: string;
  stickToBottomRef: MutableRefObject<boolean>;
  pendingAnchorRef: MutableRefObject<ScrollAnchor | null>;
  anchorIndexByKey: ReadonlyMap<string, number>;
  scrollToBottom: () => void;
}) {
  const scrollToBottomRef = useRef(scrollToBottom);
  const rafRef = useRef<{ measure: number | null; restore: number | null }>({ measure: null, restore: null });

  useLayoutEffect(() => {
    scrollToBottomRef.current = scrollToBottom;
  }, [scrollToBottom]);

  useLayoutEffect(() => {
    const list = scrollElementRef.current;
    if (!list) return;

    const wasAtBottom = stickToBottomRef.current || isScrolledNearBottom(list);
    const contentOffset = contentElementRef.current?.offsetTop ?? 0;
    const virtualScrollTop = Math.max(0, list.scrollTop - contentOffset);
    const measuredAnchor = virtualizer.getVirtualItemForOffset(virtualScrollTop);
    const anchor =
      pendingAnchorRef.current ??
      (wasAtBottom || !measuredAnchor ? null : captureScrollAnchor([measuredAnchor], virtualScrollTop));
    pendingAnchorRef.current = null;
    const fallbackTop = list.scrollTop;
    cancelReconciledMeasurement(rafRef.current);

    rafRef.current.measure = window.requestAnimationFrame(() => {
      rafRef.current.measure = null;
      virtualizer.measure();
      rafRef.current.restore = window.requestAnimationFrame(() => {
        rafRef.current.restore = null;
        const currentList = scrollElementRef.current;
        if (!currentList) return;
        if (wasAtBottom) {
          scrollToBottomRef.current();
          return;
        }
        stickToBottomRef.current = false;
        const anchorIndex = anchor ? anchorIndexByKey.get(String(anchor.key)) ?? anchor.index : undefined;
        currentList.scrollTop = restoredScrollTop(
          anchor,
          anchorIndex,
          (index) => virtualizer.getOffsetForIndex(index, "start")?.[0],
          fallbackTop,
          contentElementRef.current?.offsetTop ?? 0,
        );
      });
    });

    return () => cancelReconciledMeasurement(rafRef.current);
  }, [anchorIndexByKey, contentElementRef, layoutRevision, pendingAnchorRef, scrollElementRef, stickToBottomRef, virtualizer]);
}

function cancelReconciledMeasurement(raf: { measure: number | null; restore: number | null }): void {
  if (raf.measure !== null) {
    window.cancelAnimationFrame(raf.measure);
    raf.measure = null;
  }
  if (raf.restore !== null) {
    window.cancelAnimationFrame(raf.restore);
    raf.restore = null;
  }
}

export function AgentPage({
  agent,
  detail,
  detailLoading,
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
  resumeRevision = 0,
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
  const [composerDragActive, setComposerDragActive] = useState(false);
  const [modelPickerOpen, setModelPickerOpen] = useState(false);
  const [changingModel, setChangingModel] = useState<string | null>(null);
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [selectedReasoningEffort, setSelectedReasoningEffort] = useState("auto");
  const [reasoningPopoverOpen, setReasoningPopoverOpen] = useState(false);
  const [visibleTimelineItemLimit, setVisibleTimelineItemLimit] = useState(() => defaultTimelineItemLimit("info"));
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const virtualWrapperRef = useRef<HTMLDivElement | null>(null);
  const composerTextareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const dragCounterRef = useRef(0);
  const preserveScrollRef = useRef<ScrollAnchor | null>(null);
  const stickToBottomRef = useRef(true);
  const autoStickToBottomRef = useRef(false);
  const scheduledBottomScrollRef = useRef<number | null>(null);
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
  const timelineTurnIndexById = useMemo(
    () => new Map(timelineTurns.map((turn, index) => [turn.id, index])),
    [timelineTurns],
  );
  const targetTimelineItemId = useMemo(() => timeline.find((item) => itemHasEventSeq(item, targetEventSeq))?.id, [targetEventSeq, timeline]);
  const rowVirtualizer = useVirtualizer({
    count: timelineTurns.length,
    getScrollElement: () => messageListRef.current,
    estimateSize: () => 320,
    paddingEnd: MESSAGE_LIST_BOTTOM_SAFE_SPACE,
    overscan: 4,
    getItemKey: (index) => timelineTurns[index]?.id ?? `empty:${index}`,
  });
  const trimmedPrompt = prompt.trim();
  const canSendPrompt = (trimmedPrompt.length > 0 || attachments.length > 0) && !sendingPrompt;
  const newestTimelineItem = timeline[timeline.length - 1];
  const timelineVersion = `${timeline.length}:${newestTimelineItem?.id ?? ""}:${timeline[0]?.id ?? ""}:${detail?.events?.length ?? 0}:${hasOlderEvents}`;
  const timelineLayoutVersion = useMemo(() => `${resumeRevision}:${timelineLayoutRevision(timelineTurns)}`, [resumeRevision, timelineTurns]);
  const hasHiddenTimelineItems = timeline.length >= visibleTimelineItemLimit && sourceTimeline.length > visibleTimelineItemLimit;
  const groupedModelOptions = useMemo(() => groupModelOptionsByProvider(modelCatalog.options), [modelCatalog.options]);
  const activeModelOption = useMemo(() => modelCatalog.options.find((option) => option.routeRef === activeAgent.model), [activeAgent.model, modelCatalog.options]);
  const activeModelSupportsReasoning = activeModelOption?.supportsReasoningEffort ?? Boolean(activeAgent.modelReasoningEffort);
  const activeReasoningBadge = activeModelSupportsReasoning ? (activeAgent.modelReasoningEffort ?? "auto") : undefined;
  const activeModelTitle = modelButtonTitle(activeAgent.model, activeReasoningBadge, activeAgent.modelSource === "agent_override");
  const activeProviderGroup = activeModelOption
    ? (activeModelOption.endpoint === "default"
        ? activeModelOption.providerFamily
        : `${activeModelOption.providerFamily} / ${activeModelOption.endpoint}`)
    : undefined;
  const currentProvider = selectedProvider ?? activeProviderGroup ?? groupedModelOptions[0]?.provider ?? "runtime";
  const currentProviderModels = groupedModelOptions.find((group) => group.provider === currentProvider)?.models ?? [];

  useEffect(() => {
    setVisibleTimelineItemLimit(defaultTimelineItemLimit(displayLevel));
    setModelPickerOpen(false);
    setReasoningPopoverOpen(false);
    setSelectedProvider(null);
    setSelectedReasoningEffort(activeAgent.modelReasoningEffort ?? "auto");
  }, [activeAgent.id, displayLevel]);

  useEffect(() => {
    return () => {
      if (scheduledBottomScrollRef.current !== null) {
        window.cancelAnimationFrame(scheduledBottomScrollRef.current);
      }
    };
  }, []);

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

  function scrollToConversationBottom() {
    const list = messageListRef.current;
    if (!list) return;

    stickToBottomRef.current = true;
    autoStickToBottomRef.current = true;

    const lastTurnIndex = timelineTurns.length - 1;
    const scrollNow = () => {
      const currentList = messageListRef.current;
      if (!currentList) return;
      if (lastTurnIndex >= 0) {
        rowVirtualizer.scrollToIndex(lastTurnIndex, { align: "end", behavior: "auto" });
      }
      currentList.scrollTop = currentList.scrollHeight;
    };

    if (scheduledBottomScrollRef.current !== null) {
      window.cancelAnimationFrame(scheduledBottomScrollRef.current);
      scheduledBottomScrollRef.current = null;
    }

    scrollNow();
    scheduledBottomScrollRef.current = window.requestAnimationFrame(() => {
      scrollNow();
      scheduledBottomScrollRef.current = window.requestAnimationFrame(() => {
        scrollNow();
        scheduledBottomScrollRef.current = null;
        autoStickToBottomRef.current = false;
        const currentList = messageListRef.current;
        if (currentList) {
          stickToBottomRef.current = isScrolledNearBottom(currentList);
        }
      });
    });
  }

  useLayoutEffect(() => {
    preserveScrollRef.current = null;
    stickToBottomRef.current = true;
    rowVirtualizer.measure();
    scrollToConversationBottom();
  }, [activeAgent.id]);

  useLayoutEffect(() => {
    const list = messageListRef.current;
    if (!list) return;

    if (stickToBottomRef.current) {
      scrollToConversationBottom();
    }
  }, [timelineVersion]);

  useReconciledVirtualMeasurements({
    virtualizer: rowVirtualizer,
    scrollElementRef: messageListRef,
    contentElementRef: virtualWrapperRef,
    layoutRevision: timelineLayoutVersion,
    stickToBottomRef,
    pendingAnchorRef: preserveScrollRef,
    anchorIndexByKey: timelineTurnIndexById,
    scrollToBottom: scrollToConversationBottom,
  });

  useLayoutEffect(() => {
    if (!targetTimelineItemId) return;
    const list = messageListRef.current;
    if (!list) return;
    stickToBottomRef.current = false;

    // Target item already in DOM — scroll directly.
    const target = list.querySelector<HTMLElement>(`[data-timeline-item-id="${cssEscape(targetTimelineItemId)}"]`);
    if (target) {
      target.scrollIntoView({ block: "center" });
      return;
    }

    // Target item is virtualized out of view — scroll its turn into view first.
    const turnIndex = timelineTurns.findIndex((turn) => turn.items.some((item) => item.id === targetTimelineItemId));
    if (turnIndex < 0) return;
    rowVirtualizer.scrollToIndex(turnIndex, { align: "center" });
    const timer = setTimeout(() => {
      list.querySelector<HTMLElement>(`[data-timeline-item-id="${cssEscape(targetTimelineItemId)}"]`)?.scrollIntoView({ block: "center" });
    }, 0);
    return () => clearTimeout(timer);
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
      next.push({
        kind: attachmentKindForFile(file),
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

  function composerDragHasFiles(event: DragEvent<HTMLFormElement>): boolean {
    const items = event.dataTransfer?.items;
    return Boolean(items && Array.from(items).some((item) => item.kind === "file"));
  }

  function handleComposerDragEnter(event: DragEvent<HTMLFormElement>) {
    if (sendingPrompt || !composerDragHasFiles(event)) return;
    event.preventDefault();
    dragCounterRef.current += 1;
    setComposerDragActive(true);
  }

  function handleComposerDragOver(event: DragEvent<HTMLFormElement>) {
    if (sendingPrompt || !composerDragHasFiles(event)) return;
    event.preventDefault();
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = "copy";
    }
  }

  function handleComposerDragLeave(event: DragEvent<HTMLFormElement>) {
    if (sendingPrompt || !composerDragHasFiles(event)) return;
    event.preventDefault();
    dragCounterRef.current = Math.max(0, dragCounterRef.current - 1);
    if (dragCounterRef.current === 0) {
      setComposerDragActive(false);
    }
  }

  async function handleComposerDrop(event: DragEvent<HTMLFormElement>) {
    if (sendingPrompt || !composerDragHasFiles(event)) return;
    event.preventDefault();
    dragCounterRef.current = 0;
    setComposerDragActive(false);
    await handleAttachmentFiles(event.dataTransfer?.files ?? null);
  }

  function handleMessageListScroll() {
    const list = messageListRef.current;
    if (!list) return;
    if (autoStickToBottomRef.current) {
      stickToBottomRef.current = true;
      return;
    }
    stickToBottomRef.current = isScrolledNearBottom(list);
  }

  async function handleLoadOlderEvents() {
    const list = messageListRef.current;
    if (list) {
      const contentOffset = virtualWrapperRef.current?.offsetTop ?? 0;
      const virtualScrollTop = Math.max(0, list.scrollTop - contentOffset);
      const anchorItem = rowVirtualizer.getVirtualItemForOffset(virtualScrollTop);
      preserveScrollRef.current =
        list.scrollTop > TOP_SCROLL_THRESHOLD && anchorItem
          ? captureScrollAnchor([anchorItem], virtualScrollTop)
          : null;
      stickToBottomRef.current = false;
    }
    try {
      await onLoadOlderEvents();
      setVisibleTimelineItemLimit((limit) => limit + HISTORY_PAGE_VISIBLE_INCREMENT);
    } catch {
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

    setChangingModel(option.routeRef);
    try {
      await onSetModel(option.routeRef, option.supportsReasoningEffort && reasoningEffort !== "auto" ? reasoningEffort : undefined);
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
    setReasoningPopoverOpen(false);
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
            {timelineTurns.length > 0 ? (
              <div
                ref={virtualWrapperRef}
                className="message-list-virtual-wrapper"
                style={{ height: rowVirtualizer.getTotalSize(), position: "relative" }}
              >
                {rowVirtualizer.getVirtualItems().map((vi) => {
                  const turn = timelineTurns[vi.index];
                  if (!turn) return null;
                  return (
                    <div
                      key={vi.key}
                      className="message-list-virtual-item"
                      data-index={vi.index}
                      ref={rowVirtualizer.measureElement}
                      style={{
                        position: "absolute",
                        top: 0,
                        left: 0,
                        width: "100%",
                        transform: `translateY(${vi.start}px)`,
                      }}
                    >
                      <TimelineTurnGroup
                        displayLevel={displayLevel}
                        onOpenInspector={onOpenInspector}
                        onInspectActivity={onInspectActivity}
                        selectedActivityId={selectedActivityId}
                        targetTimelineItemId={targetTimelineItemId}
                        turn={turn}
                      />
                    </div>
                  );
                })}
              </div>
            ) : null}
            {timeline.length === 0 ? (
              detailLoading ? (
                <div className="conversation-loading" role="status" aria-label={t("common.loading")}>
                  <LoaderCircle size={24} className="spin" />
                </div>
              ) : (
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
              )
            ) : null}
          </div>

          {isWorking ? (
            <div className="working-indicator-slot">
              <WorkingIndicator
                activities={workingActivities}
                agent={activeAgent}
                displayLevel={displayLevel}
                onInspectActivity={onInspectActivity}
                onOpenOverview={onOpenInspector}
              />
            </div>
          ) : null}

          <form
            className={composerDragActive ? "composer composer--drag" : "composer"}
            aria-label={t("agent.sendInputAria", { id: activeAgent.id })}
            onSubmit={handleSubmit}
            onDragEnter={handleComposerDragEnter}
            onDragOver={handleComposerDragOver}
            onDragLeave={handleComposerDragLeave}
            onDrop={handleComposerDrop}
          >
            {composerDragActive ? (
              <div className="composer-drop-overlay" aria-hidden="true">
                <span>{t("agent.dropImageHint")}</span>
              </div>
            ) : null}
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
              multiple
              disabled={sendingPrompt}
              onChange={(event) => void handleAttachmentFiles(event.target.files)}
            />
            {attachments.length > 0 ? (
              <div className="composer-attachments" aria-label="Attachments">
                {attachments.map((attachment, index) => (
                  <span className="composer-attachment" key={`${attachment.name ?? attachment.kind}:${index}`}>
                    {attachment.name ?? `${attachment.kind} ${index + 1}`}
                    <button
                      type="button"
                      aria-label={`Remove ${attachment.name ?? `${attachment.kind} ${index + 1}`}`}
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
                  aria-label="Attach files"
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
                            {["auto", ...(activeModelOption?.reasoningEffortOptions ?? [])].map((effort) => (
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
                                className={`model-option ${option.routeRef === activeAgent.model ? "is-active" : ""}`}
                                key={option.routeRef}
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
                                  {changingModel === option.routeRef ? <em>{t("common.saving")}</em> : null}
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
    const provider = option.endpoint === "default"
      ? option.providerFamily
      : `${option.providerFamily} / ${option.endpoint}`;
    const models = groups.get(provider) ?? [];
    models.push(option);
    groups.set(provider, models);
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

function isScrolledNearBottom(list: HTMLElement): boolean {
  return list.scrollHeight - list.scrollTop - list.clientHeight < BOTTOM_SCROLL_THRESHOLD;
}

function defaultTimelineItemLimit(displayLevel: DisplayLevel): number {
  if (displayLevel === "debug") return DEFAULT_DEBUG_TIMELINE_ITEM_LIMIT;
  if (displayLevel === "verbose") return DEFAULT_VERBOSE_TIMELINE_ITEM_LIMIT;
  return DEFAULT_INFO_TIMELINE_ITEM_LIMIT;
}

function isAgentWorking(agent: AgentSummary, sendingPrompt: boolean, t: TFunction): boolean {
  return sendingPrompt || deriveAgentDisplayStatus(agent, t).tone === "running";
}

function cssEscape(value: string): string {
  if (typeof CSS !== "undefined" && CSS.escape) return CSS.escape(value);
  return value.replace(/["\\]/g, "\\$&");
}

