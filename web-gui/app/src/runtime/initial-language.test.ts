import { afterEach, describe, expect, it, vi } from "vitest";

import { initialResponseLanguage } from "./initial-language";

describe("initialResponseLanguage", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("uses the explicitly selected UI language", () => {
    expect(initialResponseLanguage("zh-CN")).toBe("zh-CN");
    expect(initialResponseLanguage("en")).toBe("en");
  });

  it("follows the browser language mapping in system mode", () => {
    vi.stubGlobal("navigator", { language: "zh-CN" });
    expect(initialResponseLanguage("system")).toBe("zh-CN");

    vi.stubGlobal("navigator", { language: "en-GB" });
    expect(initialResponseLanguage("system")).toBe("en");

    vi.stubGlobal("navigator", { language: "ja-JP" });
    expect(initialResponseLanguage("system")).toBe("en");
  });
});
