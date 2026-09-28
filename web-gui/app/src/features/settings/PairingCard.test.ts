import { describe, expect, it } from "vitest";

import { pairingLink } from "./PairingCard";

describe("pairingLink", () => {
  it("keeps the one-time ticket in the fragment, not the request URL", () => {
    const link = new URL(pairingLink("https://holon.example:8080", "secret/+=="));
    expect(link.origin).toBe("https://holon.example:8080");
    expect(link.pathname).toBe("/login");
    expect(link.search).toBe("");
    expect(new URLSearchParams(link.hash.slice(1)).get("pair")).toBe("secret/+==");
  });
});
