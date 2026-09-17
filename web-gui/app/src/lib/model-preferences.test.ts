import { afterEach, describe, expect, it, vi } from "vitest";
import { decodeModelPreferences, modelPreferencesKey, rememberModel, toggleFavorite } from "./model-preferences";
afterEach(() => vi.unstubAllGlobals());
describe("model preferences", () => {
  it("ignores malformed, oversized and invalid storage fields and deduplicates", () => {
    for (const raw of [null, "{", "null", "x".repeat(200_001)]) expect(decodeModelPreferences(raw)).toEqual({ favorites: [], recent: [] });
    expect(decodeModelPreferences(JSON.stringify({ favorites: [null, 5, "", "route", "route"], recent: ["route2"] }))).toEqual({ favorites: ["route"], recent: ["route2"] });
    expect(decodeModelPreferences(JSON.stringify({ recent: Array.from({ length: 30 }, (_, i) => String(i)) })).recent).toHaveLength(12);
  });
  it("isolates origins, preserves favorite route identity and bounds recents", () => {
    const data = new Map<string, string>();
    const location = { origin: "http://localhost:7878" };
    const dispatchEvent = vi.fn();
    vi.stubGlobal("window", { location, dispatchEvent, localStorage: { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value) } });
    toggleFavorite("openai@default/model");
    toggleFavorite("openai@private/model");
    for (let i = 0; i < 20; i++) rememberModel(`model-${i}`);
    rememberModel("model-18");
    const state = decodeModelPreferences(data.get(modelPreferencesKey(location.origin))!);
    expect(state.favorites).toEqual(["openai@private/model", "openai@default/model"]);
    expect(state.recent).toHaveLength(12);
    expect(state.recent[0]).toBe("model-18");
    location.origin = "https://another.test";
    rememberModel("other");
    expect(decodeModelPreferences(data.get(modelPreferencesKey(location.origin))!)).toEqual({ favorites: [], recent: ["other"] });
    expect(dispatchEvent).toHaveBeenCalled();
  });
  it("does not throw when browser storage is blocked", () => {
    vi.stubGlobal("window", { location: { origin: "http://test" }, get localStorage() { throw new Error("denied"); } });
    expect(toggleFavorite("route")).toBe(false);
    expect(() => rememberModel("route")).not.toThrow();
  });
});
