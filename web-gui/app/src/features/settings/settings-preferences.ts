export const SETTINGS_TAB_KEY = "holon.webGui.settingsTab.v1";
export const SETTINGS_TABS = ["general", "models", "vision", "decision", "search", "advanced"] as const;
export type SettingsTabKey = typeof SETTINGS_TABS[number];

export function readSettingsTab(): SettingsTabKey {
  try {
    const value = window.localStorage.getItem(SETTINGS_TAB_KEY);
    return SETTINGS_TABS.find((tab) => tab === value) ?? "general";
  } catch {
    return "general";
  }
}

export function rememberSettingsTab(tab: SettingsTabKey): void {
  try {
    window.localStorage.setItem(SETTINGS_TAB_KEY, tab);
  } catch {
    // Settings navigation remains available when browser storage is blocked.
  }
}
