import type { RuntimeNotification } from "./notification-sync";

/** Availability of the page Notification API in this browser. */
export type SystemNotificationPermission = "unsupported" | NotificationPermission;

export interface SystemNotificationDeps {
  permission: () => SystemNotificationPermission;
  requestPermission: () => Promise<SystemNotificationPermission>;
  /**
   * Runs `deliver` only when this tab owns the id; false means another tab
   * claimed it. Tabs read the same hint burst independently, so this is the
   * only cross-tab exclusion point.
   */
  claim: (id: string, deliver: () => void) => Promise<boolean>;
  show: (item: RuntimeNotification) => void;
}

/** Long enough to cover the read skew between tabs hearing the same hint. */
const CLAIM_HOLD_MS = 10_000;

/** Delivers each unseen item once, from a single tab. */
export function createSystemNotifications({
  permission, requestPermission, claim, show,
}: SystemNotificationDeps) {
  const shown = new Set<string>();
  return {
    permission,
    requestPermission,
    async deliver(items: RuntimeNotification[]) {
      if (permission() !== "granted") return;
      // Dedupe the batch first: a repeated id must not race its own claim.
      const unique = new Map(items.map((item) => [item.id, item]));
      const fresh = [...unique.values()].filter((item) => !shown.has(item.id));
      // Claims are independent, so one held claim never delays a sibling item.
      await Promise.all(fresh.map((item) => claim(item.id, () => {
        if (shown.has(item.id)) return;
        shown.add(item.id);
        show(item);
      })));
    },
  };
}

export function readSystemNotificationPermission(): SystemNotificationPermission {
  if (typeof window === "undefined" || !window.isSecureContext || !("Notification" in window)) {
    return "unsupported";
  }
  return Notification.permission;
}

export async function requestSystemNotificationPermission(): Promise<SystemNotificationPermission> {
  const current = readSystemNotificationPermission();
  if (current !== "default") return current;
  try {
    return await Notification.requestPermission();
  } catch {
    return readSystemNotificationPermission();
  }
}

/**
 * Claims an id through a per-id Web Lock. The lock is held briefly, so a closed
 * or reloaded tab never pins the name; without the Locks API the browser cannot
 * coordinate tabs and dedupe is then per tab only.
 */
export function claimSystemNotification(id: string, deliver: () => void): Promise<boolean> {
  const locks = typeof navigator === "undefined" ? undefined : navigator.locks;
  if (!locks) {
    deliver();
    return Promise.resolve(true);
  }
  return locks
    .request(`holon:system-notification:${id}`, { ifAvailable: true }, async (lock) => {
      if (!lock) return false;
      deliver();
      await new Promise((resolve) => setTimeout(resolve, CLAIM_HOLD_MS));
      return true;
    })
    .then((claimed) => claimed === true)
    .catch(() => false);
}
