// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ConversationStateView, ConversationTurnSummary } from "@holon/conversation-sdk";
import type { AgentSummary } from "../../runtime/types";
import type { ConversationSessionModel } from "../../runtime/conversation-view-model";
import { buildConversationSessionModel } from "../../runtime/conversation-view-model";
import { acquireConversationScope, conversationViewport, disposeAllConversationScopes } from "../../runtime/conversation-scope-store";
import "../../i18n";
import { AgentPage, type ConversationTimelineBundle, readStoredComposerDraft, writeStoredComposerDraft } from "./AgentPage";

// Exercise AgentPage's real effects and scroll handlers with deterministic
// layout, isolating them from the timeline's independent disclosure rendering.
vi.mock("./ConversationTimeline", () => ({
  ConversationSyncStatus: () => null,
  ConversationTimeline: ({ model }: { model: ConversationSessionModel }) => (
    <>{model.turns.map((turn) => (
      <section key={turn.turnId} data-conversation-anchor={`turn:${turn.turnId}`}>
        {turn.turnId === "located" ? <>
          <div data-conversation-anchor="input:earlier" />
          <article data-conversation-anchor="brief:target" />
        </> : <div data-conversation-anchor="input:next" />}
      </section>
    ))}</>
  ),
}));
vi.mock("./CurrentWorkBar", () => ({ CurrentWorkBar: () => null }));

let root: Root;
let host: HTMLDivElement;
let growth: number;
let shift: number;
let observers: TestResizeObserver[];

class TestResizeObserver {
  constructor(readonly callback: ResizeObserverCallback) { observers.push(this); }
  observe() {}
  unobserve() {}
  disconnect() { observers = observers.filter((observer) => observer !== this); }
}

function model(version = 1, ids = ["located", "next"]) {
  const turns: ConversationTurnSummary[] = ids.map((id, index) => ({
    turn_id: id, key: { turn_index: index + 1, turn_id: id }, revision: version,
    presentation_class: "operator", inputs: [], execution: { kind: "active" },
    result: { kind: "pending" }, settled: false, attention: null,
    detail_coverage: { kind: "complete" }, brief_ids: [],
  }));
  const view: ConversationStateView = {
    scope: {
      remote_id: "local", agent_id: "alpha", generation: 1, runtime_id: "runtime",
      event_log_epoch: "epoch", visibility_scope_id: "scope", schema_version: 1, query_version: 1,
    },
    turns, pending_inputs: [], details: [], next_before_cursor: null,
    has_more: true, checkpoint: "checkpoint" as never, through_seq: version, reset_reason: null,
  };
  return buildConversationSessionModel({
    status: { kind: "ready" }, view, historyState: { kind: "idle" },
    briefs: new Map(), briefLoadStates: new Map(), detailLoadStates: new Map(),
  });
}

function bundle(): ConversationTimelineBundle {
  return {
    model: model(), browsing: true, hasNewerHistory: true, newerHistoryState: { kind: "idle" },
    location: { requestId: 1, kind: "ready", anchor: "brief:target", turnId: "located" },
    onLoadOlderHistory: vi.fn(), onLoadNewerHistory: vi.fn(),
    onLoadBrief: vi.fn(), onLoadDetail: vi.fn(), onLoadOlderActivities: vi.fn(), onRetry: vi.fn(),
    briefRecord: () => null, briefLoadState: () => null, detailLoadState: () => ({ kind: "idle" }),
  };
}

const onRead = vi.fn(() => true);
const onSendPrompt = vi.fn(async () => {});
async function render(conversation: ConversationTimelineBundle, overrides: {
  parentAgentId?: string; parentTaskId?: string; sendDisabledReason?: string; promptError?: string;
  sendingPrompt?: boolean;
} = {}) {
  await act(async () => root.render(<AgentPage
    agent={{ id: "alpha", model: "model" } as AgentSummary} conversation={conversation}
    detail={null} sendingPrompt={false} modelCatalog={{ source: "fixture", options: [] }}
    modelCatalogLoading={false} conversationReady
    onRefreshModels={vi.fn()} onSetModel={vi.fn()} onClearModel={vi.fn()}
    onRetrySync={vi.fn()} onSendPrompt={onSendPrompt} onAbortCurrentRun={vi.fn()}
    onConversationRead={onRead} onOpenWorkItem={vi.fn()} onOpenTask={vi.fn()}
    {...overrides}
  />));
  return host.querySelector<HTMLDivElement>(".message-list")!;
}

async function resize() {
  await act(async () => {
    for (const observer of [...observers]) observer.callback([], observer as unknown as ResizeObserver);
  });
}

async function userScroll(list: HTMLElement, top: number) {
  await act(async () => {
    list.dispatchEvent(new WheelEvent("wheel", { bubbles: true }));
    list.scrollTop = top;
    list.dispatchEvent(new Event("scroll"));
  });
}

beforeEach(() => {
  growth = 0;
  shift = 0;
  observers = [];
  onRead.mockClear();
  onSendPrompt.mockClear();
  writeStoredComposerDraft("alpha", "");
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("ResizeObserver", TestResizeObserver);
  // Do not advance frames: fast commits must work inside the active gesture.
  vi.spyOn(window, "requestAnimationFrame").mockReturnValue(1);
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("message-list") ? 400 : 0;
  });
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("message-list") ? 2000 : 0;
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    if (this.classList.contains("message-list")) return { top: 100, bottom: 500, height: 400, right: 600 } as DOMRect;
    const anchor = this.dataset.conversationAnchor;
    const coordinates: Record<string, [number, number]> = {
      "turn:located": [100, 1100 + growth], "input:earlier": [700, 50],
      "brief:target": [800 + growth, 100], "turn:next": [1650, 300], "input:next": [1700, 100],
    };
    if (!anchor || !coordinates[anchor]) return { top: 0, bottom: 0, height: 0 } as DOMRect;
    const [position, height] = coordinates[anchor];
    const top = 100 + position + shift - (this.closest<HTMLElement>(".message-list")?.scrollTop ?? 0);
    return { top, bottom: top + height, height } as DOMRect;
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  disposeAllConversationScopes(false);
  host.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("AgentPage reading-position lifecycle", () => {
  it("restores a warm scope's reading anchor after unmount without retaining old DOM nodes", async () => {
    const scopeKey = "local#alice::alpha#incarnation:1";
    acquireConversationScope({ key: scopeKey, agentId: "alpha", baseUrl: "/api", readOnly: true });
    const conversation = { ...bundle(), scopeKey, location: undefined };
    const first = await render(conversation);
    await userScroll(first, 800);
    await act(async () => root.render(null));
    const saved = conversationViewport(scopeKey);
    expect(saved?.scrollTop).toBe(800);
    expect(saved?.anchors.length).toBeGreaterThan(0);
    expect(saved?.anchors[0]).not.toHaveProperty("element");
    const restored = await render(conversation);
    expect(restored).not.toBe(first);
    expect(restored.scrollTop).toBe(800);
    growth = 100;
    await resize();
    expect(restored.scrollTop).toBe(900);
  });

  it("shows a refresh failure only above the conversation without blocking sends", async () => {
    const retry = vi.fn();
    const notice = "Agent status refresh is delayed";
    writeStoredComposerDraft("alpha", "My draft");
    await render({ ...bundle(), accessRefreshNotice: notice, onRetryAccess: retry });
    const banner = host.querySelector(".conversation-sync-status .conversation-status-banner[role=status]");
    expect(banner?.querySelector("span")?.textContent).toBe(notice);
    expect(banner?.querySelector("span")?.title).toBe(notice);
    expect(banner?.querySelector(":scope > svg[aria-hidden=true]")).not.toBeNull();
    expect(banner?.querySelector("button")?.getAttribute("type")).toBe("button");
    expect(banner?.querySelector("button svg[aria-hidden=true]")).not.toBeNull();
    expect(host.querySelector<HTMLTextAreaElement>(".composer textarea")?.disabled).toBe(false);
    expect(host.querySelector(".composer-status")).toBeNull();
    expect(host.querySelector<HTMLButtonElement>(".send-button")?.disabled).toBe(false);
    await act(async () => banner?.querySelector<HTMLButtonElement>("button")?.click());
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it("shows a refresh delay without a manual retry while background retries continue", async () => {
    await render({ ...bundle(), accessRefreshNotice: "Agent status refresh is delayed" });
    const banner = host.querySelector(".conversation-sync-status .conversation-status-banner[role=status]");
    expect(banner?.textContent).toBe("Agent status refresh is delayed");
    expect(banner?.querySelector("button")).toBeNull();
  });

  it.each([undefined, "supervisor"])("keeps real composer status without a permanent input warning (parent=%s)", async (parentAgentId) => {
    await render(bundle(), { parentAgentId, sendDisabledReason: "Stopped by operator" });
    expect(host.querySelector(".composer-status[role=note]")).toBeNull();
    expect(host.querySelector(".composer-status[role=status]")?.textContent).toBe("Stopped by operator");
    expect(host.querySelector<HTMLTextAreaElement>(".composer textarea")?.disabled).toBe(false);
    await render(bundle(), { parentAgentId, promptError: "Prompt admission failed" });
    expect(host.querySelector(".composer-status[role=alert]")?.textContent).toBe("Prompt admission failed");
    expect(host.querySelector<HTMLTextAreaElement>(".composer textarea")?.disabled).toBe(false);
    expect(host.textContent).not.toContain("Messages go directly to this subagent");
  });

  it.each(["Stopped by operator", "Input is disabled"])(
    "keeps drafts editable but blocks every send path while %s", async (sendDisabledReason) => {
      writeStoredComposerDraft("alpha", "My draft");
      await render(bundle(), { sendDisabledReason });
      const textarea = host.querySelector<HTMLTextAreaElement>(".composer textarea")!;
      const fileInput = host.querySelector<HTMLInputElement>(".composer-file-input")!;
      const send = host.querySelector<HTMLButtonElement>(".send-button")!;
      expect(textarea.disabled).toBe(false);
      expect(fileInput.disabled).toBe(false);
      expect(textarea.value).toBe("My draft");
      expect(send.disabled).toBe(true);
      await act(async () => {
        textarea.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
        host.querySelector("form.composer")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
        send.click();
      });
      expect(onSendPrompt).not.toHaveBeenCalled();
      expect(readStoredComposerDraft("alpha")).toBe("My draft");

      await render(bundle());
      expect(textarea.value).toBe("My draft");
      expect(send.disabled).toBe(false);
      expect(onSendPrompt).not.toHaveBeenCalled();
      await act(async () => send.click());
      expect(onSendPrompt).toHaveBeenCalledExactlyOnceWith("My draft", []);
      expect(textarea.value).toBe("");
      expect(readStoredComposerDraft("alpha")).toBe("");
    },
  );

  it("keeps the in-flight send snapshot locked until submission completes", async () => {
    writeStoredComposerDraft("alpha", "Sending draft");
    await render(bundle(), { sendingPrompt: true });
    expect(host.querySelector<HTMLTextAreaElement>(".composer textarea")?.disabled).toBe(true);
    expect(host.querySelector<HTMLInputElement>(".composer-file-input")?.disabled).toBe(true);
    expect(host.querySelector<HTMLButtonElement>(".send-button")?.disabled).toBe(true);
    await render(bundle());
    expect(host.querySelector<HTMLTextAreaElement>(".composer textarea")?.value).toBe("Sending draft");
    expect(host.querySelector<HTMLTextAreaElement>(".composer textarea")?.disabled).toBe(false);
    expect(onSendPrompt).not.toHaveBeenCalled();
  });

  it("keeps the initial located source fixed through same-turn hydration without a view commit", async () => {
    const list = await render(bundle());
    const target = host.querySelector<HTMLElement>('[data-conversation-anchor="brief:target"]')!;
    expect(list.scrollTop).toBe(650);
    expect(target.getBoundingClientRect().top).toBe(250);
    growth = 300;
    await resize();
    expect(list.scrollTop).toBe(950);
    expect(target.getBoundingClientRect().top).toBe(250);
    expect(onRead).not.toHaveBeenCalled();
  });

  it("restores a fast scroll-triggered older page commit while user intent is active", async () => {
    const conversation = bundle();
    const list = await render(conversation);
    await userScroll(list, 100);
    expect(conversation.onLoadOlderHistory).toHaveBeenCalledTimes(1);
    // Keep the corrected offset inside the older-page threshold, so an
    // unfiltered correction event really would request another page.
    shift = 20;
    await render({ ...conversation, model: model(2) });
    expect(list.scrollTop).toBe(120);
    // The correction's event must not become another pagination gesture.
    await act(async () => list.dispatchEvent(new Event("scroll")));
    expect(conversation.onLoadOlderHistory).toHaveBeenCalledTimes(1);
    await resize();
    expect(list.scrollTop).toBe(120);
    await act(async () => {
      list.scrollTop = 110;
      list.dispatchEvent(new Event("scroll"));
    });
    expect(conversation.onLoadOlderHistory).toHaveBeenCalledTimes(2);
    expect(onRead).not.toHaveBeenCalled();
  });

  it("preserves a surviving source through newer paging and whole-page eviction at unchanged total height", async () => {
    const conversation = bundle();
    const list = await render(conversation);
    await userScroll(list, 1640);
    expect(conversation.onLoadNewerHistory).toHaveBeenCalledTimes(1);
    const target = host.querySelector<HTMLElement>('[data-conversation-anchor="input:next"]')!;
    expect(target.getBoundingClientRect().top).toBe(160);
    shift = -600;
    await render({ ...conversation, model: model(2, ["next"]) });
    expect(list.scrollTop).toBe(1040);
    expect(host.querySelector<HTMLElement>('[data-conversation-anchor="input:next"]')!.getBoundingClientRect().top).toBe(160);
    await resize();
    expect(list.scrollTop).toBe(1040);
    expect(onRead).not.toHaveBeenCalled();
  });

  it("accepts a real momentum scroll immediately after restoration and preserves its new offset", async () => {
    const conversation = bundle();
    const list = await render(conversation);
    await userScroll(list, 690);
    growth = 120;
    await resize();
    expect(list.scrollTop).toBe(810);
    // No new wheel and no animation frame: this is the same gesture.
    await act(async () => {
      list.scrollTop = 850;
      list.dispatchEvent(new Event("scroll"));
    });
    const target = host.querySelector<HTMLElement>('[data-conversation-anchor="brief:target"]')!;
    expect(target.getBoundingClientRect().top).toBe(170);
    growth = 200;
    await resize();
    expect(list.scrollTop).toBe(930);
    expect(target.getBoundingClientRect().top).toBe(170);
    expect(onRead).not.toHaveBeenCalled();
  });
});
