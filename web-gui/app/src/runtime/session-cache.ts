/**
 * Remote-key computation and legacy cache init for the pre-read-model session
 * cache. Conversation content now flows from the conversation read model; the
 * legacy per-agent content records are cleared on init (see
 * cacheClearRemoteSessions) instead of being read or written.
 */

import { ensureCacheSchemaVersion } from "./idb-cache";
import type { RuntimeConnectionConfig } from "./types";

export function currentRemoteKey(config: RuntimeConnectionConfig): string {
  if (config.mode === "local") return "local";
  return config.baseUrl?.trim().replace(/\/+$/, "") || "remote";
}

export async function initSessionCache(): Promise<boolean> {
  return ensureCacheSchemaVersion();
}
