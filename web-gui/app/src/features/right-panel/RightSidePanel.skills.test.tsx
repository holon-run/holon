// @vitest-environment happy-dom
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import { ClipboardProvider } from "../../components/ClipboardProvider";
import { useRuntimeStore } from "../../runtime/runtime-store";
import type { AgentSummary, SkillCatalogState } from "../../runtime/types";
import { RightSidePanel } from "./RightSidePanel";

vi.mock("../../runtime/useSelectedAgentRefresh", () => ({
  refreshSelectedAgentResources: vi.fn(),
}));

const agent: AgentSummary = {
  id: "web", incarnation: 1, badge: "W", profile: "", lifecycle: "idle",
  focusSummary: "", workspace: "", attention: "", model: "", footer: "",
  subtitle: "", lastBrief: "", lastTurnTime: "", pending: 0, activeTaskCount: 0,
  waitingCount: 0, posture: "idle", postureReason: "",
};

const roots: Root[] = [];
const initialState = useRuntimeStore.getState();
beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(new Date("2026-10-31T09:00:00Z"));
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
});
afterEach(async () => {
  for (const root of roots.splice(0)) await act(async () => root.unmount());
  useRuntimeStore.setState(initialState, true);
  document.body.innerHTML = "";
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("RightSidePanel skill manager", () => {
  it("revalidates an empty catalog on entry after TTL, deduplicates fresh entries, and preserves forced refresh", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      let body: unknown;
      if (path.endsWith("/skills/catalog")) body = { catalog: [] };
      else if (path.endsWith("/handshake")) body = { capabilities: [
        "agents.roster-snapshot.v1", "agents.projection-snapshot.v1",
        "events.projection-effect.v1", "briefs.atomic-created-event.v1",
      ] };
      else if (path.endsWith("/agents/snapshot")) body = {
        contract_version: 1, runtime_id: "runtime-1", event_log_epoch: "epoch-1",
        visibility_scope_id: "scope-1", agents: [],
      };
      else if (path.endsWith("/agents/list")) body = [];
      else throw new Error(`Unexpected request: ${path}`);
      return new Response(JSON.stringify(body), { headers: { "content-type": "application/json" } });
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();
    const refreshAvailableSkills = vi.fn((options?: { force?: boolean }) =>
      useRuntimeStore.getState().refreshSkillCatalog(options));
    await refreshAvailableSkills({ force: false });
    const availableSkillCatalog: SkillCatalogState = useRuntimeStore.getState().skillCatalog;
    const noop = () => {};
    const props: ComponentProps<typeof RightSidePanel> = {
      agent,
      connection: { mode: "local", summary: "", source: "http" },
      availableSkillCatalog,
      availableSkillCatalogLoading: false,
      open: true,
      mode: "normal",
      full: false,
      width: 400,
      onResize: noop,
      onToggleMode: noop,
      onOpenWorkItemDetail: noop,
      onOpenTask: noop,
      onRefreshAgentSkills: noop,
      onRefreshAvailableSkills: refreshAvailableSkills,
      onEnableAgentSkill: noop,
      onDisableAgentSkill: noop,
      onOpenSkill: noop,
      onShowAgentOverview: noop,
      onNavigateBack: noop,
      onSelectView: noop,
      onBrowseFiles: noop,
      onClose: noop,
    };
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    roots.push(root);
    await act(async () => root.render(<ClipboardProvider><RightSidePanel {...props} /></ClipboardProvider>));

    async function openManager() {
      const button = host.querySelector<HTMLButtonElement>(".agent-skill-actions button:last-child");
      expect(button).not.toBeNull();
      await act(async () => button!.click());
      expect(host.querySelector(".agent-skill-manager")).not.toBeNull();
    }

    await openManager();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(refreshAvailableSkills).toHaveBeenLastCalledWith({ force: false });
    await act(async () => host.querySelector<HTMLButtonElement>(".panel-actions button[aria-label]")!.click());
    await openManager();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(refreshAvailableSkills).toHaveBeenCalledTimes(3);

    vi.setSystemTime(Date.now() + 60_000);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await act(async () => host.querySelector<HTMLButtonElement>(".panel-actions button[aria-label]")!.click());
    await openManager();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(refreshAvailableSkills).toHaveBeenLastCalledWith({ force: false });

    await act(async () => host.querySelector<HTMLButtonElement>(".agent-skill-manager-head button")!.click());
    expect(refreshAvailableSkills).toHaveBeenCalledTimes(5);
    expect(refreshAvailableSkills).toHaveBeenLastCalledWith();
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(String(fetchMock.mock.calls.at(-1)?.[0])).toMatch(/\/api\/skills\/catalog$/);
  });
});
