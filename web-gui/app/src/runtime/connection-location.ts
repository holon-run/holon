import { resolveRuntimeApiBase } from "./client";
import type { RuntimeConnectionConfig } from "./types";

/** Describes the connection address, never asserts filesystem locality. */
export function connectionLocation(config: RuntimeConnectionConfig, origin: string) {
  const url = new URL(resolveRuntimeApiBase(config) || origin, origin);
  return { host: url.host, origin: url.origin,
    loopback: isLoopbackHostname(url.hostname),
    sameOrigin: url.origin === new URL(origin).origin };
}

export function isLoopbackHostname(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/\.$/, "");
  return host === "localhost" || host.endsWith(".localhost") || /^127\.\d+\.\d+\.\d+$/.test(host)
    || host === "[::1]" || host.startsWith("[::ffff:7f");
}
