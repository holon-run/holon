import { describe, expect, it } from "vitest";

import { canApplySessionEvent } from "./session-events";

describe("runtime event schema compatibility", () => {
  it("applies legacy events and only matching v2 registry schemas", () => {
    expect(canApplySessionEvent({ type: "future_event" })).toBe(true);
    expect(
      canApplySessionEvent({
        type: "brief_created",
        contract_version: 2,
        payload_schema: "holon.runtime_event.brief_created",
        payload_schema_version: 1,
      }),
    ).toBe(true);
    expect(
      canApplySessionEvent({
        type: "brief_created",
        contract_version: 2,
        payload_schema: "holon.runtime_event.future_brief",
        payload_schema_version: 1,
      }),
    ).toBe(false);
    expect(
      canApplySessionEvent({
        type: "brief_created",
        contract_version: 3,
        payload_schema: "holon.runtime_event.brief_created",
        payload_schema_version: 1,
      }),
    ).toBe(false);
  });
});
