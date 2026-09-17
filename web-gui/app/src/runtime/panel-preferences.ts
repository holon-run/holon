import type { RightPanelView } from "./types";

/** Layout and file locations only: never persist tool output, messages or credentials. */
const KEY = "holon.webGui.contextPanel.v1";
type RestorablePanelView = Extract<RightPanelView, { kind: "agent_overview" | "file_browser" }>;
export interface PanelPreferences { open: boolean; mode: "normal" | "expanded"; view?: RestorablePanelView }
function restorableView(value: unknown): RestorablePanelView | undefined {
  if (!value || typeof value !== "object") return undefined;
  const view = value as Record<string, unknown>;
  if (typeof view.agentId !== "string") return undefined;
  if (view.kind === "file_browser" && typeof view.workspaceId === "string") {
    return { kind: "file_browser", agentId: view.agentId, workspaceId: view.workspaceId,
      ...Object.fromEntries(["executionRootId", "initialPath", "initialFilePath", "fragment"].filter((key) => typeof view[key] === "string").map((key) => [key, view[key]])),
    };
  }
  return { kind: "agent_overview", agentId: view.agentId };
}
export function readPanelPreferences(): PanelPreferences {
  try {
    const value = JSON.parse(localStorage.getItem(KEY) ?? "null");
    return { open: value?.open === true, mode: value?.mode === "expanded" ? "expanded" : "normal", view: restorableView(value?.view) };
  } catch { return { open: false, mode: "normal" }; }
}
export function writePanelPreferences(value: PanelPreferences): void {
  try { localStorage.setItem(KEY, JSON.stringify({ open: value.open, mode: value.mode, view: restorableView(value.view) })); } catch { /* storage unavailable */ }
}
export function rememberPanelView(view: RightPanelView | undefined): void {
  const current = readPanelPreferences();
  writePanelPreferences({ ...current, view: restorableView(view) });
}

export function rememberPanelFileLocation(view: Extract<RightPanelView, { kind: "file_browser" }>): void {
  const current = readPanelPreferences();
  if (current.view?.kind !== "file_browser" || current.view.agentId !== view.agentId || current.view.workspaceId !== view.workspaceId) return;
  if (current.view.initialFilePath === view.initialFilePath && current.view.initialPath === view.initialPath && current.view.executionRootId === view.executionRootId && current.view.fragment === view.fragment) return;
  writePanelPreferences({ ...current, view });
}
