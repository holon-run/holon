/**
 * Language mode persisted in localStorage and shown in Settings.
 * "system" resolves the actual language from the browser at runtime.
 */
export type LanguageMode = "system" | "en" | "zh-CN";

/**
 * The concrete language used by i18next after resolving a {@link LanguageMode}.
 * Only supported app languages appear here.
 */
export type ResolvedLanguage = "en" | "zh-CN";

/** Languages the app currently ships translation resources for. */
export const SUPPORTED_LANGUAGES: readonly ResolvedLanguage[] = ["en", "zh-CN"];

/** Options shown in the Settings language selector. */
export const LANGUAGE_MODE_OPTIONS: readonly LanguageMode[] = ["system", "en", "zh-CN"];

export const DEFAULT_LANGUAGE_MODE: LanguageMode = "system";

export const LANGUAGE_MODE_STORAGE_KEY = "holon.webGui.languageMode.v1";

export function isLanguageMode(value: unknown): value is LanguageMode {
  return value === "system" || value === "en" || value === "zh-CN";
}
