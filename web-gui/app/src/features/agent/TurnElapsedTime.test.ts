import { describe, expect, it } from "vitest";
import { formatTurnDuration, turnElapsedMs } from "./TurnElapsedTime";

const startedAt = "2026-09-16T00:00:00Z";
const start = Date.parse(startedAt);
const active = { execution: { kind: "active" as const }, startedAt };
const terminal = { ...active, execution: { kind: "terminal" as const, outcome: "completed" as const } };

describe("turn elapsed time", () => {
  it("uses the persisted start instead of mount time and clamps clock skew", () => {
    expect(turnElapsedMs(active, start + 23000)).toBe(23000);
    expect(turnElapsedMs(active, start - 1000)).toBe(0);
  });
  it("freezes on authoritative terminal duration even while results are pending", () => {
    const turn = { ...terminal, completedAt: "2026-09-16T00:01:30Z", durationMs: 83000 };
    expect(turnElapsedMs(turn, start + 500000)).toBe(83000);
    expect(turnElapsedMs({ ...turn, durationMs: 0 }, start)).toBe(0);
  });
  it("uses a known end as fallback but never lets terminal time keep ticking", () => {
    expect(turnElapsedMs({ ...terminal, completedAt: "2026-09-16T00:01:23Z" }, start)).toBe(83000);
    expect(turnElapsedMs(terminal, start + 500000)).toBeNull();
    expect(turnElapsedMs({ execution: active.execution }, start)).toBeNull();
    expect(turnElapsedMs({ ...active, startedAt: "invalid" }, start)).toBeNull();
  });
  it.each([[0, "0:00"], [999, "0:00"], [23000, "0:23"], [83000, "1:23"], [3599000, "59:59"], [3600000, "1:00:00"], [90061000, "25:01:01"]])("formats %i ms as %s", (ms, text) => {
    expect(formatTurnDuration(ms as number)).toBe(text);
  });
});
