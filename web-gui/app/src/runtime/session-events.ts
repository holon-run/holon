import type { components } from "./generated/openapi";

export type SessionEventEnvelope = Partial<components["schemas"]["StreamEventEnvelope"]>;

export function canApplySessionEvent(event: SessionEventEnvelope): boolean {
  // The stream/page boundary validates the contract version once. Event
  // kinds are intentionally open-ended here: the server registry is the
  // source of truth, and unknown events remain durable diagnostics instead
  // of being rejected by a client-side copy of that registry.
  return typeof event.type === "string" && event.type.length > 0;
}
