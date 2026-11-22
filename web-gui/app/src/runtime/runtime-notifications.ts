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

// Kept in memory only. A runtime/subject switch clears it, independently of
// conversation read markers and presentation caches.
export const useRuntimeNotifications = create<NotificationSnapshot>(() => emptyNotifications);

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
});
