import { describe, expect, it } from "vitest";

import { currentRemoteKey } from "./session-cache";

describe("currentRemoteKey", () => {
  it("returns 'local' for local mode", () => {
    expect(currentRemoteKey({ mode: "local" })).toBe("local");
  });

  it("returns normalized baseUrl for remote mode", () => {
    expect(currentRemoteKey({ mode: "remote", baseUrl: "https://example.com/" })).toBe("https://example.com");
    expect(currentRemoteKey({ mode: "remote", baseUrl: "https://example.com///" })).toBe("https://example.com");
  });

  it("returns 'remote' for empty baseUrl", () => {
    expect(currentRemoteKey({ mode: "remote", baseUrl: "" })).toBe("remote");
    expect(currentRemoteKey({ mode: "remote", baseUrl: undefined })).toBe("remote");
  });
});
