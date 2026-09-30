import { afterEach, describe, expect, it, vi } from "vitest";

import { readSettingsTab, rememberSettingsTab, SETTINGS_TAB_KEY, SETTINGS_TABS } from "./settings-preferences";

afterEach(() => vi.unstubAllGlobals());

describe("settings tab preference", () => {
  it.each([null, "", "obsolete", "toString"])("defaults to General for %s", (value) => {
    vi.stubGlobal("window", { localStorage: { getItem: () => value } });
    expect(readSettingsTab()).toBe("general");
  });

  it.each(SETTINGS_TABS)("remembers %s", (tab) => {
    const stored = new Map<string, string>();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (key: string) => stored.get(key) ?? null,
        setItem: (key: string, value: string) => stored.set(key, value),
      },
    });
    rememberSettingsTab(tab);
    expect(stored.get(SETTINGS_TAB_KEY)).toBe(tab);
    expect(readSettingsTab()).toBe(tab);
  });

  it("still works without browser storage", () => {
    vi.stubGlobal("window", {
      localStorage: {
        getItem: () => { throw new Error("blocked"); },
        setItem: () => { throw new Error("blocked"); },
      },
    });
    expect(readSettingsTab()).toBe("general");
    expect(() => rememberSettingsTab("models")).not.toThrow();
  });

  it.each(SETTINGS_TABS)("explicit %s overrides the remembered tab", (tab) => {
    vi.stubGlobal("window", { localStorage: { getItem: () => "advanced" } });
    expect(readSettingsTab(`?tab=${tab}`)).toBe(tab);
  });

  it.each(["?tab=obsolete", "?tab=toString", "?tab="])("ignores invalid deep links: %s", (search) => {
    vi.stubGlobal("window", { localStorage: { getItem: () => "vision" } });
    expect(readSettingsTab(search)).toBe("vision");
  });

  it("opens explicit tabs even when storage is blocked", () => {
    vi.stubGlobal("window", {
      localStorage: { getItem: () => { throw new Error("blocked"); } },
    });
    expect(readSettingsTab("?tab=models")).toBe("models");
    expect(readSettingsTab("?tab=invalid")).toBe("general");
  });
});
