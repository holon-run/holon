import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import en from "./resources/en";
import zhCN from "./resources/zh-CN";
import { resolveLanguage } from "./resolve";
import { readStoredLanguageMode } from "./storage";

const initialLanguage = readStoredLanguageMode();

void i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    "zh-CN": { translation: zhCN },
  },
  lng: resolveLanguage(initialLanguage),
  fallbackLng: "en",
  supportedLngs: ["en", "zh-CN"],
  interpolation: {
    // React already escapes by default; we do not escape interpolation values.
    escapeValue: false,
  },
  returnObjects: false,
  // Avoid warnings during the initial render before the Provider mounts.
  react: {
    bindI18n: "languageChanged loaded",
    useSuspense: false,
  },
});

export default i18n;

export function changeLanguage(lng: "en" | "zh-CN"): Promise<unknown> {
  return i18n.changeLanguage(lng);
}

