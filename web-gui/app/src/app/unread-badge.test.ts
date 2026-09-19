import { describe, expect, it } from "vitest";

import { formatUnreadBadge } from "./App";

describe("formatUnreadBadge", () => {
  it("renders plain counts for complete unread views", () => {
    expect(formatUnreadBadge(1, false)).toBe("1");
    expect(formatUnreadBadge(42, false)).toBe("42");
    expect(formatUnreadBadge(99, false)).toBe("99");
  });

  it("marks truncated counts as lower bounds without exceeding the cap", () => {
    expect(formatUnreadBadge(1, true)).toBe("1+");
    expect(formatUnreadBadge(99, true)).toBe("99+");
    expect(formatUnreadBadge(100, true)).toBe("99+");
    expect(formatUnreadBadge(250, true)).toBe("99+");
  });

  it("caps complete counts above 99 at 99+", () => {
    expect(formatUnreadBadge(100, false)).toBe("99+");
    expect(formatUnreadBadge(9999, false)).toBe("99+");
  });
});
