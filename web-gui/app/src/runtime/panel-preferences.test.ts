import { afterEach, describe, expect, it, vi } from "vitest";
import { readPanelPreferences, rememberPanelView, writePanelPreferences } from "./panel-preferences";

afterEach(() => vi.unstubAllGlobals());
describe("panel preferences", () => {
  it("restores open and expanded layout plus file location without storing contents", () => {
    let stored: string | null = null;
    vi.stubGlobal("localStorage", { getItem: () => stored, setItem: (_key: string, value: string) => { stored = value; } });
    writePanelPreferences({ open: true, mode: "expanded" });
    rememberPanelView({ kind: "file_browser", agentId: "a", workspaceId: "w", initialFilePath: "notes.md" });
    expect(readPanelPreferences()).toMatchObject({ open: true, mode: "expanded", view: { kind: "file_browser", initialFilePath: "notes.md" } });
    rememberPanelView({ kind: "activity_inspector", agentId: "a", activity: { body: "private message" } } as never);
    expect(stored).not.toContain("private message");
    expect(readPanelPreferences().view).toEqual({ kind: "agent_overview", agentId: "a" });
  });
  it("falls back safely when storage is blocked or corrupt", () => {
    vi.stubGlobal("localStorage", { getItem: () => "broken", setItem: () => { throw new Error("blocked"); } });
    expect(readPanelPreferences()).toEqual({ open: false, mode: "normal" });
    expect(() => writePanelPreferences({ open: true, mode: "normal" })).not.toThrow();
  });
});
