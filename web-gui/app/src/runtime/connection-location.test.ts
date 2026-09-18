import { describe, expect, it } from "vitest";
import { connectionLocation } from "./connection-location";

describe("connection address, not filesystem locality", () => {
  it("does not call same-origin remote hosting local", () => {
    expect(connectionLocation({ mode: "local" }, "https://holon.example.com")).toEqual({ host: "holon.example.com", origin: "https://holon.example.com", loopback: false, sameOrigin: true });
  });
  it.each(["localhost", "127.0.0.1", "[::1]"])("recognizes %s as a loopback address", (host) => {
    expect(connectionLocation({ mode: "local" }, `http://${host}:7878`).loopback).toBe(true);
  });
  it("uses the actual API endpoint for separately configured connections", () => {
    expect(connectionLocation({ mode: "remote", baseUrl: "https://server.example/api" }, "http://localhost:3000")).toEqual({ host: "server.example", origin: "https://server.example", loopback: false, sameOrigin: false });
  });
});
