/**
 * Cross-tab server revalidation notification.
 *
 * This channel is only a best-effort hint that another tab changed server
 * state. It is not a local state source; subscribers must revalidate through
 * the server API after receiving a notification.
 */

export const READ_STATE_BUS_CHANNEL = "holon.webGui.eventLedger.readStates.v1";

export interface ReadStateBusMessage {
  kind: "server_revalidation_required";
  remoteKey: string;
  agentId: string;
}

function isReadStateBusMessage(value: unknown): value is ReadStateBusMessage {
  if (typeof value !== "object" || value === null) return false;
  const message = value as { kind?: unknown; remoteKey?: unknown; agentId?: unknown };
  return (
    message.kind === "server_revalidation_required" &&
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
    try {
      this.channel = typeof BroadcastChannel === "undefined" ? null : new BroadcastChannel(READ_STATE_BUS_CHANNEL);
    } catch {
      this.channel = null;
    }
    this.channel?.addEventListener("message", (event) => {
      if (this.disposed) return;
      if (isReadStateBusMessage(event.data)) onMessage(event.data);
    });
  }

  get available(): boolean {
    return this.channel != null;
  }

  /** Broadcast a best-effort hint that subscribers should revalidate via API. */
  publish(message: ReadStateBusMessage): void {
    if (this.disposed) return;
    try {
      this.channel?.postMessage(message);
    } catch {
      // A failed channel only loses the hint; API refresh remains authoritative.
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
