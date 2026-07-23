import { describe, expect, it } from "vitest";

import type { RuntimeTraceRecord } from "../../runtime/runtime-trace";
import { filterRuntimeTraceRecords, runtimeTraceDiagnosticFilename } from "./RuntimeTracePanel";

const records: RuntimeTraceRecord[] = [
  {
    traceId: "trace-a",
    spanId: "span-a",
    name: "cache.read",
    trigger: "agent.open",
    startedAt: "2026-07-23T00:00:00.000Z",
    durationMs: 2,
    outcome: "ok",
    attributes: { cacheHit: true },
  },
  {
    traceId: "trace-b",
    spanId: "span-b",
    name: "request.refresh",
    startedAt: "2026-07-23T00:00:01.000Z",
    durationMs: 10,
    outcome: "error",
    attributes: { status: 500 },
  },
];

describe("filterRuntimeTraceRecords", () => {
  it("filters by span, trigger, attributes, and outcome", () => {
    expect(filterRuntimeTraceRecords(records, { query: "agent.open" })).toEqual([records[0]]);
    expect(filterRuntimeTraceRecords(records, { query: "500" })).toEqual([records[1]]);
    expect(filterRuntimeTraceRecords(records, { outcome: "error" })).toEqual([records[1]]);
  });
});

describe("runtimeTraceDiagnosticFilename", () => {
  it("sanitizes the agent id and uses an ISO timestamp", () => {
    expect(runtimeTraceDiagnosticFilename("agent/a", new Date("2026-07-23T01:02:03.000Z")))
      .toBe("holon-runtime-trace-agent_a-2026-07-23T01-02-03.000Z.json");
  });
});
