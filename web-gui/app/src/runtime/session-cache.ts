/**
 * Remote-key computation shared by the independent browser caches.
 */

import type { RuntimeConnectionConfig } from "./types";

export function currentRemoteKey(config: RuntimeConnectionConfig): string {
  if (config.mode === "local") return "local";
  return config.baseUrl?.trim().replace(/\/+$/, "") || "remote";
}
