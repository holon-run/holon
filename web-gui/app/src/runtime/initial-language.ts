import { resolveLanguage } from "../i18n/resolve";
import { SUPPORTED_LANGUAGES, type LanguageMode } from "../i18n/types";

/**
 * One-time response-language hint for a new agent's first default bootstrap
 * reply, derived from the UI language at creation time.
 *
 * Explicit modes use their own tag; `system` follows the browser language
 * mapping the UI already applies. `undefined` means no hint could be
 * derived, and agent creation must still proceed.
 */
export function initialResponseLanguage(mode: LanguageMode): string | undefined {
  const resolved = resolveLanguage(mode);
  return SUPPORTED_LANGUAGES.includes(resolved) ? resolved : undefined;
}
