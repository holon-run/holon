import { describe, expect, it } from "vitest";

import en from "./resources/en";
import zhCN from "./resources/zh-CN";

/** Recursively collect all leaf key paths from a nested object. */
function collectKeys(obj: Record<string, unknown>, prefix = ""): string[] {
  const keys: string[] = [];
  for (const [key, value] of Object.entries(obj)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (value !== null && typeof value === "object" && !Array.isArray(value)) {
      keys.push(...collectKeys(value as Record<string, unknown>, path));
    } else {
      keys.push(path);
    }
  }
  return keys;
}

describe("i18n resource key completeness", () => {
  const enKeys = collectKeys(en).sort();
  const zhKeys = collectKeys(zhCN).sort();

  it("en and zh-CN have the same set of keys", () => {
    const missingInZh = enKeys.filter((key) => !zhKeys.includes(key));
    const missingInEn = zhKeys.filter((key) => !enKeys.includes(key));
    expect(
      { missingInZh, missingInEn },
      "Translation key mismatch between en and zh-CN resources",
    ).toEqual({ missingInZh: [], missingInEn: [] });
  });
});
