import { DEFAULT_LANGUAGE_MODE, LANGUAGE_MODE_STORAGE_KEY, type LanguageMode } from "./types";

/** Read the persisted language mode from localStorage, falling back to the default. */
export function readStoredLanguageMode(): LanguageMode {
  try {
    const raw = localStorage.getItem(LANGUAGE_MODE_STORAGE_KEY);
    if (raw === "system" || raw === "en" || raw === "zh-CN") {
      return raw;
    }
  } catch {
    // localStorage unavailable
  }
  return DEFAULT_LANGUAGE_MODE;
}
