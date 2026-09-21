import type { components } from "./generated/openapi";

export type SessionEventEnvelope = Partial<components["schemas"]["StreamEventEnvelope"]>;

export function canApplySessionEvent(event: SessionEventEnvelope): boolean {
  if (typeof event.type !== "string" || event.type.length === 0) {
    return false;
  }
  // The server marks typed events with a projection effect only after
  // registry validation. Without that signal, retain the event as a
  // diagnostic instead of projecting a future payload by kind alone.
  if (
    event.payload_schema &&
    event.payload_schema !== "holon.runtime_event.legacy" &&
    typeof event.projection_effect !== "string"
  ) {
    return false;
  }
  return true;
}
