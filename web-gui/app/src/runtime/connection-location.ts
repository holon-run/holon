import { resolveRuntimeApiBase } from "./client";
import type { RuntimeConnectionConfig } from "./types";

/** Describes the connection address, never asserts filesystem locality. */
export function connectionLocation(config: RuntimeConnectionConfig, origin: string) {
  const url = new URL(resolveRuntimeApiBase(config) || origin, origin);
  return { host: url.host, origin: url.origin,
    loopback: ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname),
    sameOrigin: url.origin === new URL(origin).origin };
}
