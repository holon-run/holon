import { beforeEach, describe, expect, it } from "vitest";

import {
  clearRuntimeTraceRecords,
  buildRuntimeTraceDiagnosticBundle,
  createRuntimeTrace,
  exportRuntimeTraceRecords,
  getRuntimeTraceRecords,
  isRuntimeTraceEnabled,
  setRuntimeTraceEnabled,
  startRuntimeSpan,
} from "./runtime-trace";

describe("runtime trace", () => {
  beforeEach(() => {
    setRuntimeTraceEnabled(true, { clear: true });
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

  it("does not collect spans while diagnostics are disabled", () => {
    setRuntimeTraceEnabled(false, { clear: true });
    const trace = createRuntimeTrace("agent.open");
    startRuntimeSpan(trace, "cache.read").end();

    expect(isRuntimeTraceEnabled()).toBe(false);
    expect(getRuntimeTraceRecords()).toHaveLength(0);
  });

  it("builds an agent-scoped diagnostic bundle without connection secrets", () => {
    const first = createRuntimeTrace("agent.open", { agentId: "agent-a" });
    const second = createRuntimeTrace("agent.open", { agentId: "agent-b" });
    startRuntimeSpan(first, "cache.read").end();
    startRuntimeSpan(second, "request.refresh").end();

    expect(buildRuntimeTraceDiagnosticBundle({
      agentId: "agent-a",
      guiVersion: "1.2.3",
      mode: "production",
      connection: { mode: "remote", source: "http", connected: true },
      exportedAt: "2026-07-23T00:00:00.000Z",
    })).toEqual({
      schemaVersion: 1,
      exportedAt: "2026-07-23T00:00:00.000Z",
      gui: { version: "1.2.3", mode: "production" },
      agentId: "agent-a",
      connection: { mode: "remote", source: "http", connected: true },
      trace: {
        enabled: true,
        recordCount: 1,
        records: [expect.objectContaining({ agentId: "agent-a", name: "cache.read" })],
      },
    });
  });
});
