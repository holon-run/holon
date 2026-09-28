import { describe, expect, it } from "vitest";

import { pairingIssueUrl, pairingLink } from "./PairingCard";
import { tailscaleServeUrl } from "./TailscaleServeCard";
import type { RuntimeConnection } from "../../runtime/types";

describe("pairingLink", () => {
  it("keeps the one-time ticket in the fragment, not the request URL", () => {
    const link = new URL(pairingLink("https://holon.example:8080", "secret/+=="));
    expect(link.origin).toBe("https://holon.example:8080");
    expect(link.pathname).toBe("/login");
    expect(link.search).toBe("");
    expect(new URLSearchParams(link.hash.slice(1)).get("pair")).toBe("secret/+==");
  });
});

describe("settings runtime endpoints", () => {
  const connection: RuntimeConnection = {
    mode: "remote", source: "http", summary: "remote", baseUrl: "https://runtime.example:8080",
  };
  it("uses the selected remote API base for pairing and Tailscale requests", () => {
    expect(pairingIssueUrl(connection)).toBe("https://runtime.example:8080/api/auth/pairing/issue");
    expect(tailscaleServeUrl(connection, "/enable")).toBe("https://runtime.example:8080/api/control/network/tailscale/serve/enable");
  });
  it("uses the shared same-origin API base for local connections", () => {
    const local = { ...connection, mode: "local" as const, baseUrl: undefined };
    expect(pairingIssueUrl(local)).toBe("/api/auth/pairing/issue");
    expect(tailscaleServeUrl(local)).toBe("/api/control/network/tailscale/serve");
  });
});
