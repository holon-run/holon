import { describe, expect, it, vi } from "vitest";

import { generateUuid } from "./uuid";

describe("generateUuid", () => {
  it("uses crypto.randomUUID when available", () => {
    const randomUUID = vi.fn(() => "123e4567-e89b-42d3-a456-426614174000");

    expect(generateUuid({ randomUUID } as unknown as Crypto)).toBe(
      "123e4567-e89b-42d3-a456-426614174000",
    );
    expect(randomUUID).toHaveBeenCalledOnce();
  });

  it("generates a UUID v4 with getRandomValues when randomUUID is unavailable", () => {
    const getRandomValues = vi.fn((bytes: Uint8Array) => {
      bytes.set([
        0x00, 0x11, 0x22, 0x33,
        0x44, 0x55,
        0x66, 0x77,
        0x08, 0x99,
        0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
      ]);
      return bytes;
    });

    expect(generateUuid({ getRandomValues } as unknown as Crypto)).toBe(
      "00112233-4455-4677-8899-aabbccddeeff",
    );
    expect(getRandomValues).toHaveBeenCalledOnce();
  });

  it("fails clearly when no cryptographically secure generator is available", () => {
    expect(() => generateUuid({} as Crypto)).toThrow(
      "Secure random number generation is unavailable",
    );
  });
});
