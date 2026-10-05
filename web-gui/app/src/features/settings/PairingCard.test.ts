import { describe, expect, it } from "vitest";

import { pairingIssueUrl, pairingLink, pairingOrigin, validServeOrigin } from "./PairingCard";
import type { ServeStatus } from "./useServeStatus";
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

describe("verified Serve destination", () => {
  const status: ServeStatus = {
    desired_enabled: false, available: true, connected: true, status_known: true,
    serving: true, conflict: false, hostname: "holon.example.ts.net",
    serve_url: "https://holon.example.ts.net", message: "",
  };
  it("uses observed serving status, not desired exposure", () => {
    expect(validServeOrigin(status)).toBe(status.serve_url);
  });
  it.each([
    { available: false }, { connected: false }, { status_known: false },
    { serving: false }, { conflict: true }, { hostname: undefined },
    { hostname: "other.example.ts.net" }, { serve_url: "http://holon.example.ts.net" },
    { serve_url: "https://user@holon.example.ts.net" },
    { serve_url: "https://holon.example.ts.net/path" },
    { serve_url: "https://holon.example.ts.net?x=1" },
    { serve_url: "https://holon.example.ts.net#x" },
  ])("rejects unusable status %j", (change) => {
    expect(validServeOrigin({ ...status, ...change })).toBeUndefined();
  });
});

describe("device-accessible pairing origins", () => {
  it.each([
    "http://192.168.1.10:7878",
    "https://holon.example.ts.net",
    "http://100.64.0.1:7878",
    "http://[fd7a:115c:a1e0::1]:7878",
  ])("accepts %s", (origin) => {
    expect(pairingOrigin(origin)).toBe(origin);
  });

  it.each([
    "", "not a URL", "http://localhost:7878", "http://localhost.:7878",
    "http://foo.localhost", "http://127.0.0.1:7878", "http://127.2.3.4",
    "http://0.0.0.0:7878", "http://[::1]:7878", "http://[::]:7878",
    "http://[::ffff:127.0.0.1]", "http://[::ffff:0.0.0.0]",
    "ftp://192.168.1.10", "https://user:password@holon.example",
    "https://holon.example/path", "https://holon.example?token=secret", "https://holon.example#secret",
    "https://holon.example/path/..", "https://holon.example/?", "https://holon.example/#",
  ])("rejects %s", (origin) => {
    expect(pairingOrigin(origin)).toBeUndefined();
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
