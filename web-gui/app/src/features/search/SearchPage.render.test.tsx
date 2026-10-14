// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import i18next from "i18next";

import "../../i18n";
import type { AgentSummary, SearchResponse, SearchResultItem } from "../../runtime/types";
import { SearchPage } from "./SearchPage";

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  window.history.replaceState(null, "", "/search");
  await i18next.changeLanguage("en");
});
afterEach(() => vi.unstubAllGlobals());

function hit(sourceRef: string, eventSeq?: number): SearchResultItem {
  return {
    agentId: "alpha", resultType: "message", kind: sourceRef.split(":")[0],
    locator: { sourceRef, eventSeq }, preview: "needle", createdAt: "2026-10-14T00:00:00Z",
  };
}

function response(results: SearchResultItem[]): SearchResponse {
  return {
    query: "needle", limit: 20, results,
    indexStatus: {
      freshness: "fresh", cursor: 1, highWatermark: 1, lag: 0,
      indexingNeeded: false, resultsMayBeIncomplete: false,
      consumptionWasLimited: false, skippedErrorCount: 0,
    },
  };
}

async function mount(results: SearchResultItem[]) {
  const container = document.createElement("div");
  const root = createRoot(container);
  const onOpenAgent = vi.fn();
  const onSearch = vi.fn(async () => undefined);
  const onLoadResultContent = vi.fn(async () => undefined);
  await act(async () => root.render(<SearchPage
    agents={[{ id: "alpha" }] as AgentSummary[]} selectedAgentId="alpha"
    search={response(results)} loading={false}
    resultContentBySourceRef={{}} resultContentLoadingBySourceRef={{}} resultContentErrorBySourceRef={{}}
    onOpenAgent={onOpenAgent} onSearch={onSearch} onLoadResultContent={onLoadResultContent}
  />));
  return { container, root, onOpenAgent, onSearch, onLoadResultContent };
}

describe("SearchPage canonical source navigation", () => {
  it("passes an opaque canonical source to location without requiring an event sequence", async () => {
    const sourceRef = "message:msg/中文+%#1";
    const view = await mount([hit(sourceRef)]);
    try {
      const locate = [...view.container.querySelectorAll("button")].find((button) => button.textContent === "Locate message");
      expect(locate).toBeDefined();
      await act(async () => locate!.click());
      expect(view.onOpenAgent).toHaveBeenCalledExactlyOnceWith("alpha", sourceRef);
      expect(view.container.querySelector("input")?.value).toBe("needle");
      expect(view.container.querySelectorAll(".search-result-card")).toHaveLength(1);
    } finally {
      await act(async () => view.root.unmount());
    }
  });

  it.each(["task:task-1", "turn:turn-1", "tool_execution:exec-1:output:batch:0"])(
    "keeps source-only %s viewable without advertising precise location", async (sourceRef) => {
    const view = await mount([hit(sourceRef, 42)]);
    try {
      expect([...view.container.querySelectorAll("button")].some((button) => button.textContent?.startsWith("Locate"))).toBe(false);
      const details = view.container.querySelector("details")!;
      await act(async () => {
        details.open = true;
        details.dispatchEvent(new Event("toggle"));
      });
      expect(view.onLoadResultContent).toHaveBeenCalledWith(sourceRef);
      expect(view.onOpenAgent).not.toHaveBeenCalled();
    } finally {
      await act(async () => view.root.unmount());
    }
  });

  it("submits default message/brief filters and explicit all-records scope to the server", async () => {
    const view = await mount([]);
    try {
      const form = view.container.querySelector("form")!;
      await act(async () => form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
      expect(view.onSearch).toHaveBeenLastCalledWith("needle", {
        agentIds: ["alpha"], includeAllWorkspaces: false, limit: 20, types: ["message", "brief"],
      });
      const scope = view.container.querySelectorAll("select")[1];
      await act(async () => {
        scope.value = "all";
        scope.dispatchEvent(new Event("change", { bubbles: true }));
      });
      await act(async () => form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
      expect(view.onSearch).toHaveBeenLastCalledWith("needle", {
        agentIds: ["alpha"], includeAllWorkspaces: false, limit: 20, types: [],
      });
      expect(new URLSearchParams(window.location.search).get("q")).toBe("needle");
      expect(new URLSearchParams(window.location.search).get("scope")).toBe("all");
    } finally {
      await act(async () => view.root.unmount());
    }
  });
});
