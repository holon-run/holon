import { useMemo, useSyncExternalStore } from "react";

export interface ModelPreferences { favorites: string[]; recent: string[] }
const EVENT = "holon:model-preferences";
export function modelPreferencesKey(origin: string): string {
  return `holon.webGui.modelPreferences.v1:${encodeURIComponent(origin)}`;
}
export function decodeModelPreferences(raw: string | null): ModelPreferences {
  const empty = { favorites: [], recent: [] };
  if (!raw || raw.length > 200_000) return empty;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return empty;
    const data = parsed as Record<string, unknown>;
    const list = (value: unknown, limit: number): string[] => Array.isArray(value)
      ? [...new Set(value.filter((item): item is string => typeof item === "string" && item.length > 0 && item.length <= 2048))].slice(0, limit) : [];
    return { favorites: list(data.favorites, 100), recent: list(data.recent, 12) };
  } catch { return empty; }
}
function snapshot(): string | null {
  if (typeof window === "undefined") return null;
  try { return window.localStorage.getItem(modelPreferencesKey(window.location.origin)); } catch { return null; }
}
function subscribe(listener: () => void) {
  const storage = (event: StorageEvent) => {
    if (event.key === null || event.key === modelPreferencesKey(window.location.origin)) listener();
  };
  window.addEventListener(EVENT, listener);
  window.addEventListener("storage", storage);
  return () => { window.removeEventListener(EVENT, listener); window.removeEventListener("storage", storage); };
}
function update(change: (state: ModelPreferences) => ModelPreferences): boolean {
  if (typeof window === "undefined") return false;
  try {
    window.localStorage.setItem(modelPreferencesKey(window.location.origin), JSON.stringify(change(decodeModelPreferences(snapshot()))));
    window.dispatchEvent(new Event(EVENT));
    return true;
  } catch { return false; }
}
export function toggleFavorite(route: string): boolean {
  return update((state) => ({ ...state, favorites: state.favorites.includes(route)
    ? state.favorites.filter((item) => item !== route) : [route, ...state.favorites].slice(0, 100) }));
}
export function rememberModel(route: string): void {
  update((state) => ({ ...state, recent: [route, ...state.recent.filter((item) => item !== route)].slice(0, 12) }));
}
export function useModelPreferences(): ModelPreferences {
  const raw = useSyncExternalStore(subscribe, snapshot, () => null);
  return useMemo(() => decodeModelPreferences(raw), [raw]);
}
