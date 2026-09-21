import { describe, expect, it } from "vitest";

import { canApplySessionEvent } from "./session-events";

describe("runtime event schema compatibility", () => {
  it("accepts legacy kinds and registry-marked typed events", () => {
    expect(canApplySessionEvent({ type: "brief_created" })).toBe(true);
    expect(
      canApplySessionEvent({
        type: "brief_created",
        payload_schema: "holon.runtime_event.brief_created",
        projection_effect: "display_invalidation",
      }),
    ).toBe(true);
    expect(
      canApplySessionEvent({
        type: "future_event",
        payload_schema: "holon.runtime_event.future",
      }),
    ).toBe(false);
    expect(canApplySessionEvent({ type: "" })).toBe(false);
    expect(canApplySessionEvent({})).toBe(false);
  });
});
