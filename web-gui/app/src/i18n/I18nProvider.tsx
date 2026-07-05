import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";

import i18n from "./config";
import { readStoredLanguageMode } from "./storage";
import { resolveLanguage } from "./resolve";
import { LANGUAGE_MODE_STORAGE_KEY, type LanguageMode, type ResolvedLanguage } from "./types";

interface I18nSettings {
  /** Persisted user preference (may be "system"). */
  languageMode: LanguageMode;
  /** The concrete language after resolving "system". */
  resolvedLanguage: ResolvedLanguage;
  /** Human-readable name for the resolved language, used in Settings. */
  resolvedLanguageLabel: string;
  /** Change the persisted mode; also switches i18next immediately. */
  setLanguageMode: (mode: LanguageMode) => void;
}

const LANGUAGE_LABELS: Record<ResolvedLanguage, string> = {
  en: "English",
  "zh-CN": "简体中文",
};

/**
 * Manages language mode persistence and drives i18next language changes.
 *
 * The component renders children immediately — it does not gate rendering on
 * language loading. i18next is initialized synchronously in config.ts with
 * bundled resources, so there is no async loading phase.
 */
export function I18nProvider({ children }: { children: ReactNode }) {
  const [languageMode, setLanguageModeState] = useState<LanguageMode>(readStoredLanguageMode);
  const [resolvedLanguage, setResolvedLanguage] = useState<ResolvedLanguage>(() => resolveLanguage(readStoredLanguageMode()));

  const handleModeChange = useCallback((mode: LanguageMode) => {
    const resolved = resolveLanguage(mode);
    setLanguageModeState(mode);
    setResolvedLanguage(resolved);
    try {
      localStorage.setItem(LANGUAGE_MODE_STORAGE_KEY, mode);
    } catch {
      // localStorage unavailable
    }
    void i18n.changeLanguage(resolved);
  }, []);

  // Sync when i18next language changes from external sources (e.g. dev tools).
  useEffect(() => {
    const handler = (lng: string) => {
      if (lng === "en" || lng === "zh-CN") {
        setResolvedLanguage(lng);
      }
    };
    i18n.on("languageChanged", handler);
    return () => {
      i18n.off("languageChanged", handler);
    };
  }, []);

  return (
    <I18nSettingsContext.Provider
      value={{
        languageMode,
        resolvedLanguage,
        resolvedLanguageLabel: LANGUAGE_LABELS[resolvedLanguage],
        setLanguageMode: handleModeChange,
      }}
    >
      {children}
    </I18nSettingsContext.Provider>
  );
}

const I18nSettingsContext = createContext<I18nSettings | null>(null);

export function useI18nSettings(): I18nSettings {
  const ctx = useContext(I18nSettingsContext);
  if (!ctx) {
    throw new Error("useI18nSettings must be used within I18nProvider");
  }
  return ctx;
}
