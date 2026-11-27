export const SETTINGS_TAB_KEY = "holon.webGui.settingsTab.v1";
export const SETTINGS_TABS = [
  "general",
  "access",
  "providers",
  "models",
  "vision",
  "search",
  "decision",
  "advanced",
] as const;
export type SettingsTabKey = typeof SETTINGS_TABS[number];

export const SETTINGS_TAB_LABEL_KEYS: Record<SettingsTabKey, string> = {
  general: "settings.tabGeneral",
  access: "settings.tabAccess",
  providers: "settings.tabProviders",
  models: "settings.tabModels",
  vision: "settings.tabVision",
  search: "settings.tabSearch",
  decision: "settings.tabDecision",
  advanced: "settings.tabAdvanced",
};

export function readSettingsTab(search = ""): SettingsTabKey {
  const requested = new URLSearchParams(search).get("tab");
  const explicit = SETTINGS_TABS.find((tab) => tab === requested);
  if (explicit) return explicit;
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
