/**
 * English translation resources.
 *
 * Phase 0 skeleton: covers Settings language selector and common UI labels.
 * Subsequent phases add keys per page/domain as strings are migrated.
 */
const en = {
  settings: {
    language: {
      label: "Language",
      description: "Interface display language",
      systemResolved: "System language: {{language}}",
      system: "System",
      english: "English",
      chineseSimplified: "简体中文",
    },
    general: {
      label: "General",
      description: "connection and runtime basics",
    },
    models: { label: "Models", description: "defaults and provider keys" },
    vision: { label: "Vision", description: "image observation model" },
    search: { label: "Search", description: "routing and search providers" },
    advanced: { label: "Advanced", description: "diagnostics and raw config" },
  },
  common: {
    refreshing: "Refreshing…",
    refresh: "Refresh",
  },
};

export default en;
export type EnResource = typeof en;
