import { useEffect } from "react";

import { authorizationHeaders, resolveRuntimeApiBase } from "./client";
import {
  getRuntimeClientGeneration,
  getRuntimeConnectionConfig,
  refreshRosterFromStreamHint,
  useRuntimeStore,
} from "./runtime-store";
import { createRosterHintStream, type RosterHintStreamController } from "./roster-hint-stream";
import { runtimeNotificationSync } from "./runtime-notifications";

/**
 * App-lifetime subscription to the global runtime hint stream. The
 * controller singleton survives re-renders and repeated bootstraps; client
 * switches and unhealthy connections stop it through the effect below.
 */
const rosterHintStream: RosterHintStreamController = createRosterHintStream({
  openStream: (signal) => {
    const config = getRuntimeConnectionConfig();
    const baseUrl = resolveRuntimeApiBase(config) ?? "/api";
    return fetch(`${baseUrl}/events/stream`, {
      signal,
      credentials: "include",
      headers: { ...authorizationHeaders(config.token), accept: "text/event-stream" },
    });
  },
  onHint: refreshRosterFromStreamHint,
  onNotificationHint: runtimeNotificationSync.hint,
  onConnected: runtimeNotificationSync.connected,
  onDisconnected: runtimeNotificationSync.disconnected,
});

export function useRosterHintStream(): void {
  useEffect(() => {
    let scope: number | undefined;
    let streamEnabled: boolean | undefined;
    const update = (state: ReturnType<typeof useRuntimeStore.getState>) => {
      const { connection } = state.bootstrap;
      const authorized = state.currentUserLoaded && !connection.authRequired
        && state.discovery.freshness !== "unauthorized";
      const nextScope = authorized ? getRuntimeClientGeneration() : undefined;
      if (scope !== nextScope) {
        if (streamEnabled) rosterHintStream.stop();
        streamEnabled = undefined;
        runtimeNotificationSync.stop();
        scope = nextScope;
      }
      const nextEnabled = authorized && connection.error == null && connection.source === "http";
      if (streamEnabled === nextEnabled) return;
      streamEnabled = nextEnabled;
      if (streamEnabled) {
        runtimeNotificationSync.start();
        rosterHintStream.start();
      } else {
        runtimeNotificationSync.disconnected();
        rosterHintStream.stop();
      }
    };
    // Subscribe directly, so switching credential generations fences requests
    // immediately, not only after React commits a render.
    const unsubscribe = useRuntimeStore.subscribe(update);
    update(useRuntimeStore.getState());
    document.addEventListener("visibilitychange", runtimeNotificationSync.visibilityChanged);
    return () => {
      unsubscribe();
      document.removeEventListener("visibilitychange", runtimeNotificationSync.visibilityChanged);
      rosterHintStream.stop();
      runtimeNotificationSync.stop();
    };
  }, []);
}
