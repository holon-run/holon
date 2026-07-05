import "./config";

export { I18nProvider, useI18nSettings } from "./I18nProvider";
export { resolveBrowserLanguage, resolveLanguage } from "./resolve";
export {
  DEFAULT_LANGUAGE_MODE, LANGUAGE_MODE_OPTIONS, LANGUAGE_MODE_STORAGE_KEY, SUPPORTED_LANGUAGES, isLanguageMode,
  type LanguageMode, type ResolvedLanguage,
} from "./types";
