import { beforeEach, describe, expect, it } from "vitest";

import {
  clearRuntimeTraceRecords,
  createRuntimeTrace,
  exportRuntimeTraceRecords,
  getRuntimeTraceRecords,
  startRuntimeSpan,
} from "./runtime-trace";

describe("runtime trace", () => {
  beforeEach(() => {
    clearRuntimeTraceRecords();
  });

  it("records structured spans without business payloads", () => {
    const trace = createRuntimeTrace("agent.open", {
      agentId: "agent-a",
      trigger: "agent.open",
    });
    startRuntimeSpan(trace, "cache.read", { cacheHit: true }).end("ok", {
      eventCount: 12,
    });

    expect(getRuntimeTraceRecords().at(-1)).toMatchObject({
      traceId: trace.traceId,
      name: "cache.read",
      agentId: "agent-a",
      trigger: "agent.open",
      outcome: "ok",
      attributes: { cacheHit: true, eventCount: 12 },
    });
    expect(exportRuntimeTraceRecords()).not.toContain("messageBody");
  });

  it("keeps only the newest 500 records", () => {
    const trace = createRuntimeTrace("agent.open");
    for (let index = 0; index < 510; index += 1) {
      startRuntimeSpan(trace, `span.${index}`).end();
    }

    expect(getRuntimeTraceRecords()).toHaveLength(500);
    expect(getRuntimeTraceRecords()[0]?.name).toBe("span.10");
  });
});
