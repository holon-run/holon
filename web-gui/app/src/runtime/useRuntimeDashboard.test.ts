import { describe, expect, it } from "vitest";

import { bootstrapConnectRetryDelayMs } from "./useRuntimeDashboard";

describe("bootstrapConnectRetryDelayMs", () => {
  it("uses deterministic capped exponential backoff", () => {
    expect([1, 2, 3, 4, 5, 6].map(bootstrapConnectRetryDelayMs)).toEqual([
      1_000,
      2_000,
      4_000,
      8_000,
      15_000,
      15_000,
    ]);
  });
});
