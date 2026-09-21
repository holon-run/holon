import { describe, expect, it } from "vitest";

import { canApplySessionEvent } from "./session-events";

describe("runtime event schema compatibility", () => {
  it("accepts registered and future kinds after the stream boundary is validated", () => {
    expect(canApplySessionEvent({ type: "brief_created" })).toBe(true);
    expect(canApplySessionEvent({ type: "future_event" })).toBe(true);
    expect(canApplySessionEvent({ type: "" })).toBe(false);
    expect(canApplySessionEvent({})).toBe(false);
  });
});
