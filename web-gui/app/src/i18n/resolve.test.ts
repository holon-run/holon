import { describe, expect, it } from "vitest";

import { resolveBrowserLanguage } from "./resolve";

describe("resolveBrowserLanguage", () => {
  it("maps zh variants to zh-CN", () => {
    expect(resolveBrowserLanguage("zh-CN")).toBe("zh-CN");
    expect(resolveBrowserLanguage("zh-Hans")).toBe("zh-CN");
    expect(resolveBrowserLanguage("zh-TW")).toBe("zh-CN");
    expect(resolveBrowserLanguage("zh")).toBe("zh-CN");
  });

  it("maps en variants to en", () => {
    expect(resolveBrowserLanguage("en-US")).toBe("en");
    expect(resolveBrowserLanguage("en-GB")).toBe("en");
    expect(resolveBrowserLanguage("en")).toBe("en");
  });

  it("falls back to en for unsupported languages", () => {
    expect(resolveBrowserLanguage("ja-JP")).toBe("en");
    expect(resolveBrowserLanguage("fr-FR")).toBe("en");
    expect(resolveBrowserLanguage("de")).toBe("en");
  });

  it("is case-insensitive", () => {
    expect(resolveBrowserLanguage("ZH-CN")).toBe("zh-CN");
    expect(resolveBrowserLanguage("EN-us")).toBe("en");
  });
});

describe("resolveLanguage", () => {
  it("returns the concrete mode directly", async () => {
    const { resolveLanguage } = await import("./resolve");
    expect(resolveLanguage("en")).toBe("en");
    expect(resolveLanguage("zh-CN")).toBe("zh-CN");
  });
});
