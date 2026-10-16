// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import type { AgentAccessStatus } from "../../runtime/useAgentAccess";
import { AgentOverviewPlaceholder } from "./AgentOverviewPlaceholder";

const roots: Root[] = [];
beforeEach(() => vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true));
afterEach(async () => {
  for (const root of roots.splice(0)) await act(async () => root.unmount());
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
});

async function renderPanel(status: AgentAccessStatus = "loading", open = true, mode: "normal" | "expanded" = "normal") {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  roots.push(root);
  const actions = { onClose: vi.fn(), onToggleMode: vi.fn(), onRetry: vi.fn() };
  await act(async () => root.render(
    <AgentOverviewPlaceholder status={status} error={status === "error" ? "offline" : undefined} open={open} mode={mode} full={false} {...actions} />,
  ));
  return { host, ...actions };
}

describe("AgentOverviewPlaceholder", () => {
  it("keeps the overview header and accessible loading status with decorative skeletons", async () => {
    const { host, onClose } = await renderPanel();
    expect(host.querySelector(".panel-header strong")?.textContent).toBe("Agent overview");
    expect(host.querySelector('[aria-busy="true"]')).not.toBeNull();
    expect(host.querySelector('[role="status"]')?.textContent).toBe("Loading agent information…");
    expect(host.querySelectorAll('[aria-hidden="true"] .agent-overview-skeleton')).toHaveLength(4);
    expect(host.querySelectorAll(".panel-body button")).toHaveLength(0);
    await act(async () => (host.querySelector('button[aria-label="Close side panel"]') as HTMLButtonElement).click());
    expect(onClose).toHaveBeenCalledOnce();
  });

  it.each(["error", "unauthorized", "missing", "deleting", "deleted", "replaced"] as const)("shows %s instead of a permanent skeleton and permits retry", async (status) => {
    const { host, onRetry } = await renderPanel(status);
    expect(host.querySelector('[aria-busy="false"]')).not.toBeNull();
    expect(host.querySelector(".agent-overview-skeleton")).toBeNull();
    if (status === "error") expect(host.textContent).toContain("offline");
    await act(async () => (host.querySelector(".panel-body button") as HTMLButtonElement).click());
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it("honors the closed state and does not intercept shortcuts while closed", async () => {
    const { host, onClose, onToggleMode } = await renderPanel("loading", false);
    expect(host.querySelector("aside")?.hidden).toBe(true);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    document.dispatchEvent(new KeyboardEvent("keydown", { key: ".", ctrlKey: true }));
    expect(onClose).not.toHaveBeenCalled();
    expect(onToggleMode).not.toHaveBeenCalled();
  });

  it("steps down expanded mode on Escape without closing", async () => {
    const { onClose, onToggleMode } = await renderPanel("loading", true, "expanded");
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(onToggleMode).toHaveBeenCalledOnce();
    expect(onClose).not.toHaveBeenCalled();
  });
});
