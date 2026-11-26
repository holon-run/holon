import i18next from "i18next";
import { create } from "zustand";

import { authorizationHeaders, resolveRuntimeApiBase } from "./client";
import {
  createNotificationSync,
  emptyNotifications,
  NotificationHttpError,
  type NotificationSnapshot,
  type RuntimeNotification,
} from "./notification-sync";
import { getRuntimeConnectionConfig, useRuntimeStore } from "./runtime-store";
import {
  claimSystemNotification,
  createSystemNotifications,
  readSystemNotificationPermission,
  requestSystemNotificationPermission,
  type SystemNotificationPermission,
} from "./system-notifications";

// Kept in memory only. A runtime/subject switch clears it, independently of
// conversation read markers and presentation caches.
export const useRuntimeNotifications = create<NotificationSnapshot>(() => emptyNotifications);

function showSystemNotification(item: RuntimeNotification) {
  try {
    const notification = new Notification(i18next.t(`notifications.kinds.${item.kind}`), {
      body: item.target.work_item_id
        ? `${item.target.agent_id} · ${item.target.work_item_id}`
        : item.target.agent_id,
      tag: item.id,
    });
    // Only the page owns the deep link, so a click brings this tab forward.
    notification.onclick = () => window.focus();
  } catch {
    // Browsers reject construction when the permission changed under us.
  }
}

export const systemNotifications = createSystemNotifications({
  permission: readSystemNotificationPermission,
  requestPermission: requestSystemNotificationPermission,
  claim: claimSystemNotification,
  show: showSystemNotification,
});

// Mirrored into a store so the bell panel can render the permission state.
export const useSystemNotificationPermission = create<{
  permission: SystemNotificationPermission;
  refresh: () => void;
  request: () => Promise<void>;
}>(() => ({
  permission: readSystemNotificationPermission(),
  refresh: () => {
    useSystemNotificationPermission.setState({ permission: readSystemNotificationPermission() });
  },
  request: async () => {
    const permission = await systemNotifications.requestPermission();
    useSystemNotificationPermission.setState({ permission });
  },
}));

export const runtimeNotificationSync = createNotificationSync({
  load(signal) {
    const config = getRuntimeConnectionConfig();
    const base = resolveRuntimeApiBase(config);
    const headers = { ...authorizationHeaders(config.token), accept: "application/json" };
    async function list(name: "pending" | "recent"): Promise<RuntimeNotification[]> {
      const response = await fetch(`${base ?? "/api"}/notifications/${name}`, {
        signal, credentials: "include", cache: "no-store", headers,
      });
      if (!response.ok) throw new NotificationHttpError(response.status);
      const { items } = await response.json() as { items: RuntimeNotification[] };
      if (!Array.isArray(items)) throw new Error("Invalid notification list");
      return items;
    }
    return Promise.all([list("pending"), list("recent")]).then(([pending, recent]) => ({ pending, recent }));
  },
  isHidden: () => document.visibilityState === "hidden",
  isViewing(target) {
    const state = useRuntimeStore.getState();
    return state.route === "agent" && state.selectedAgentId === target.agent_id;
  },
  publish: (snapshot) => useRuntimeNotifications.setState(snapshot, true),
  notify: (items) => { void systemNotifications.deliver(items); },
});
