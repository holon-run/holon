/**
 * Cross-tab invalidation hint for browser-local read state.
 *
 * The IndexedDB record is the source of truth between tabs; this channel
 * only nudges other tabs of the same browser profile to re-read it. Tabs in
 * different browser profiles never share the channel or the database.
 */

export const READ_STATE_BUS_CHANNEL = "holon.webGui.eventLedger.readStates.v1";

export interface ReadStateBusMessage {
  kind: "read_state_changed";
  remoteKey: string;
  agentId: string;
}

function isReadStateBusMessage(value: unknown): value is ReadStateBusMessage {
  if (typeof value !== "object" || value === null) return false;
  const message = value as { kind?: unknown; remoteKey?: unknown; agentId?: unknown };
  return (
    message.kind === "read_state_changed" &&
    typeof message.remoteKey === "string" &&
    typeof message.agentId === "string" &&
    message.remoteKey.length > 0 &&
    message.agentId.length > 0
  );
}

export class ReadStateBus {
  private readonly channel: BroadcastChannel | null;
  private disposed = false;

  constructor(onMessage: (message: ReadStateBusMessage) => void) {
    this.channel =
      typeof BroadcastChannel === "undefined"
        ? null
        : new BroadcastChannel(READ_STATE_BUS_CHANNEL);
    this.channel?.addEventListener("message", (event) => {
      if (this.disposed) return;
      if (isReadStateBusMessage(event.data)) onMessage(event.data);
    });
  }

  get available(): boolean {
    return this.channel != null;
  }

  /** Broadcast a refresh hint; delivery is best-effort. */
  publish(message: ReadStateBusMessage): void {
    if (this.disposed) return;
    try {
      this.channel?.postMessage(message);
    } catch {
 // A closed or failed channel only costs the hint; tabs still converge on
 // the database during their next refresh.
    }
  }

  dispose(): void {
    this.disposed = true;
    try {
      this.channel?.close();
    } catch {
      // Already closed.
    }
  }
}
