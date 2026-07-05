import type { LanguageMode, ResolvedLanguage } from "./types";
import { SUPPORTED_LANGUAGES } from "./types";

/**
 * Resolve a browser language string (e.g. from `navigator.language`) to one
 * of the supported app languages. Falls back to `"en"` when no match is found.
 *
 * Examples:
 *   "zh-CN", "zh-Hans", "zh" → "zh-CN"
 *   "en-US", "en"            → "en"
 *   "ja-JP"                  → "en" (fallback)
 */
export function resolveBrowserLanguage(browserLanguage: string): ResolvedLanguage {
  const tag = browserLanguage.toLowerCase();
  if (tag.startsWith("zh")) {
    return "zh-CN";
  }
  if (tag.startsWith("en")) {
    return "en";
  }
  return "en";
}

/**
 * Resolve a {@link LanguageMode} to the concrete language i18next should use.
 */
export function resolveLanguage(mode: LanguageMode): ResolvedLanguage {
  if (mode !== "system") {
    return mode;
  }
  const navLang = typeof navigator !== "undefined" ? navigator.language : "en";
  return resolveBrowserLanguage(navLang);
}

export { SUPPORTED_LANGUAGES };
