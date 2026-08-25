import { describe, expect, it } from "vitest";

import { canApplySessionEvent } from "./session-events";

describe("runtime event schema compatibility", () => {
  it("applies legacy events and matching current registry schemas", () => {
    expect(canApplySessionEvent({ type: "future_event" })).toBe(true);

    const currentEvents = [
      ["message_enqueued", "holon.runtime_event.message_lifecycle"],
      ["message_processing_started", "holon.runtime_event.message_lifecycle"],
      ["brief_created", "holon.runtime_event.brief_created"],
      ["task_created", "holon.runtime_event.task_lifecycle"],
      ["task_status_updated", "holon.runtime_event.task_lifecycle"],
      ["task_result_received", "holon.runtime_event.task_lifecycle"],
      ["work_item_written", "holon.runtime_event.work_item_lifecycle"],
      ["agent_state_changed", "holon.runtime_event.agent_state_changed"],
    ] as const;

    for (const [type, payloadSchema] of currentEvents) {
      expect(
        canApplySessionEvent({
          type,
          contract_version: 3,
          payload_schema: payloadSchema,
          payload_schema_version: 1,
        }),
      ).toBe(true);
    }
  });

  it("rejects mismatched schemas and future contract versions", () => {
    expect(
      canApplySessionEvent({
        type: "brief_created",
        contract_version: 3,
        payload_schema: "holon.runtime_event.future_brief",
        payload_schema_version: 1,
      }),
    ).toBe(false);
    expect(
      canApplySessionEvent({
        type: "brief_created",
        contract_version: 4,
        payload_schema: "holon.runtime_event.brief_created",
        payload_schema_version: 1,
      }),
    ).toBe(false);
    expect(
      canApplySessionEvent({
        type: "brief_created",
        contract_version: 3,
        payload_schema: "holon.runtime_event.brief_created",
        payload_schema_version: 2,
      }),
    ).toBe(false);
  });
});
