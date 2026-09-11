import { describe, expect, it } from "vitest";
import { truncateToWidth } from "./utils";

describe("truncateToWidth", () => {
  it("keeps text that fits unchanged", () => {
    expect(truncateToWidth("holon-web", 40)).toBe("holon-web");
    expect(truncateToWidth("汉".repeat(20), 40)).toBe("汉".repeat(20));
  });

  it("truncates long latin text at the width limit", () => {
    expect(truncateToWidth("a".repeat(45), 40)).toBe(`${"a".repeat(40)}…`);
  });

  it("counts CJK glyphs as double width", () => {
    const output = truncateToWidth("汉".repeat(30), 40);
    expect(output).toBe(`${"汉".repeat(20)}…`);
  });

  it("cuts mixed text at the combined width", () => {
    const output = truncateToWidth(`${"汉".repeat(10)}${"a".repeat(30)}`, 40);
    expect(output).toBe(`${"汉".repeat(10)}${"a".repeat(20)}…`);
  });

  it("trims trailing spaces before appending the ellipsis", () => {
    expect(truncateToWidth(`${"a".repeat(39)} bbbbb`, 40)).toBe(`${"a".repeat(39)}…`);
  });
});
